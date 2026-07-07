# Bug Repro: Observation Enabled On One `DatabaseWrapper` Clone Does Not Observe Writes Through Sibling Clones

## Problem

[`sqlx-sqlite-toolkit`'s](https://github.com/silvermine/tauri-plugin-sqlite/tree/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit)
`DatabaseWrapper` (pinned rev `e1b3836`) is `#[derive(Clone)]` and every clone shares
one underlying write connection. Yet `enable_observation()` turns observation on for the
*handle it is called on only*. A sibling clone — taken from the same source handle before
observation was enabled — keeps `observer: None`, so its `acquire_writer()` returns the
regular (bypass) writer. A committed write through that clone fires no SQLite hooks and
reaches no subscriber. There is no error and no warning: the change event simply never
arrives.

### How this surfaces via the Tauri front end

When this plugin backs a Tauri app, the front end observes a database over IPC:
`Database.load(key)` then `.observe(tables)` + `.subscribe(...)`. Those JS commands run
against the plugin's single cached `DatabaseWrapper` for that key (the plugin mutates the
cached entry in place), so the front end's own live queries work.

The trap is any **Rust-side** consumer that also wants change notifications. The Rust
counterpart of `Database.load(key)` is `app.connect(key)`, which returns a *clone* of the
cached wrapper. A consumer that does `app.connect(key)` → `enable_observation(...)` →
`subscribe(...)` observes only that clone. Every other code path — command handlers,
background tasks — writes through its *own* `app.connect(key)` clone, whose `observer` is
`None`, so those writes take the regular (bypass) writer and never reach the observing
clone's subscriber. A change made by a Tauri command is silently invisible to a Rust
observer set up this way, and any front-end view driven off that observer never updates.
The only handle whose mutation later clones inherit is the cached entry itself, which the
plugin mutates solely from the JS `observe` command — a path Rust callers cannot reach,
because the instance cache is private to the plugin crate.

## Root cause

`DatabaseWrapper` stores observation as a per-value field, not shared state:

```rust
#[derive(Clone)]
pub struct DatabaseWrapper {
   inner: Arc<SqliteDatabase>,               // shared across clones
   observer: Option<ObservableSqliteDatabase>,  // per-clone; None unless THIS handle enabled it
}
```

`enable_observation(&mut self, ...)`
([wrapper.rs:396](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L396))
sets `self.observer = Some(...)` on one value. It cannot reach clones already handed out,
and it does not mutate the source handle those clones came from.

`acquire_writer()`
([wrapper.rs:92](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L92))
routes on `self.observer` alone: `Some` → observable writer (registers hooks); `None` →
regular writer (bypass). A clone with `observer: None` therefore writes without ever
installing the commit/preupdate hooks, so nothing is published.

The observation hooks are per-acquire, confirming that only the observing handle's writes
are seen: `ObservableSqliteDatabase::acquire_writer()`
([conn_mgr.rs:147](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/conn_mgr.rs#L147))
registers hooks on acquire, and `ObservableWriteGuard::Drop`
([conn_mgr.rs:307](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/conn_mgr.rs#L307))
unregisters them — so a regular write through a sibling clone leaves no hooks behind on
the shared connection either.

The one case that *does* work: a clone taken *after* `enable_observation` shares the
observer, because `ObservableSqliteDatabase`'s `Clone`
([conn_mgr.rs:222](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/conn_mgr.rs#L222))
shares the broker `Arc`. This is exactly what the plugin's own `observe` command relies on:
it mutates the *cached* wrapper in place via `get_mut`
([commands.rs:597](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/src/commands.rs#L597)),
so subsequent `connect` clones inherit observation. A Rust consumer holding only a clone
has no equivalent — it cannot mutate the cached entry, and the cache is private to the
plugin crate.

## Expected behavior

Enabling observation should apply to the database, not to one handle. Concretely, one of:

   - Store the observer behind shared interior state (e.g. `Arc<ArcSwapOption<...>>`) so
     that enabling observation on any clone makes all clones — existing and future —
     observe the same broker and route writes through the observable writer; or
   - Provide a way to enable observation such that a caller holding a clone can turn it on
     for the shared database; or, at minimum,
   - Make the silent bypass detectable — e.g. warn (or expose a check) when a write is
     committed through a non-observing handle of a database that has an observer live
     elsewhere — instead of dropping the change with no signal.

As written, the only correct usage is "enable observation on a handle before any clone is
distributed, and only ever write through that handle or its descendants." For a type whose
whole purpose is to be a cheap, shared, `Clone`-able handle to one database — and which a
Tauri app obtains as a fresh clone on every `app.connect(key)` — that contract is
surprising and, in practice, silently violated.

## How to run

```
cargo run
```

from this case directory. The program runs the bug scenario and a control, then prints
`BUG CONFIRMED` or `BUG NOT REPRODUCED`. The control (enable-before-clone) receiving its
event proves the probe detects real notifications, so the silence in the bug scenario is
the defect and not a faulty probe.

## Relevant source files

All at rev `e1b3836` in [silvermine/tauri-plugin-sqlite](https://github.com/silvermine/tauri-plugin-sqlite/tree/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81):

| File | Lines | What it contains |
|---|---|---|
| [`crates/sqlx-sqlite-toolkit/src/wrapper.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L67-L72) | [67–72](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L67-L72) | `#[derive(Clone)] DatabaseWrapper` with per-value `observer: Option<...>` beside a shared `inner: Arc<SqliteDatabase>` |
| [`crates/sqlx-sqlite-toolkit/src/wrapper.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L92-L100) | [92–100](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L92-L100) | `acquire_writer()` — routes to observable vs regular writer based solely on `self.observer` |
| [`crates/sqlx-sqlite-toolkit/src/wrapper.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L396-L402) | [396–402](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L396-L402) | `enable_observation(&mut self, ...)` — sets the observer on this handle only |
| [`crates/sqlx-sqlite-observer/src/conn_mgr.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/conn_mgr.rs#L147-L164) | [147–164](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/conn_mgr.rs#L147-L164) | `ObservableSqliteDatabase::acquire_writer()` — registers hooks per-acquire |
| [`crates/sqlx-sqlite-observer/src/conn_mgr.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/conn_mgr.rs#L222-L229) | [222–229](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/conn_mgr.rs#L222-L229) | `impl Clone for ObservableSqliteDatabase` — shares the broker `Arc`, so a clone-after-enable observes |
| [`crates/sqlx-sqlite-observer/src/conn_mgr.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/conn_mgr.rs#L307-L320) | [307–320](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/conn_mgr.rs#L307-L320) | `ObservableWriteGuard::Drop` — unregisters hooks, so a regular write leaves none behind |
| [`src/commands.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/src/commands.rs#L573-L617) | [573–617](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/src/commands.rs#L573-L617) | JS `observe` command — mutates the *cached* wrapper via `get_mut` so future `connect` clones inherit observation (the workaround Rust consumers can't reach) |

## Related (separate) issue

Re-calling `enable_observation` / the `observe` command replaces the observer and aborts
existing subscription streams — documented at
[wrapper.rs:390–395](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L390-L395)
and enforced in the command via `remove_for_db`
([commands.rs:592](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/src/commands.rs#L592)).
Combined with observation being per-window, a second window calling `observe` silently
ends the first window's change streams. That is a distinct defect and, per one-bug-per-case,
belongs in its own case rather than here.
