# Bug Repro: Re-enabling Observation Silently Terminates Existing Subscribers

## Problem

[`sqlx-sqlite-toolkit`'s](https://github.com/silvermine/tauri-plugin-sqlite/tree/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit)
`DatabaseWrapper::enable_observation` (pinned rev `e1b3836`) replaces the wrapper's
observer, dropping the previous broadcast broker. Every `broadcast::Receiver` already
handed out by that broker then only sees `RecvError::Closed` — the change stream ends with
no error attributable to a cause and no notification. A consumer that set up observation
and subscribed earlier just stops receiving changes.

The plugin's `observe` IPC command
([src/commands.rs:573](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/src/commands.rs#L573))
calls `enable_observation` on the *cached* wrapper for a database key, so this behavior is
reachable by any caller that observes a database another caller is already observing. There
is no way for the second caller to observe additively, and no signal to the first caller
that its stream was disconnected — the second `observe` silently disconnects the first.

### How this surfaces via the Tauri front end

A Tauri front end observes a database from JS: `Database.load(key).observe(tables)` then
`.subscribe(cb)`. Observation is effectively per webview window, but the cached
`DatabaseWrapper` is shared per key. So when a **second window** (or any second caller)
runs `observe` on a key that a **first window** is already observing, `enable_observation`
tears down the first window's broker. The first window's `subscribe` callback simply stops
firing — its live queries freeze — and neither window receives any error. In a multi-window
app, opening a second window that observes the same database is enough to silently break
change notifications in the first.

## Root cause

`enable_observation`
([wrapper.rs:396](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L396))
disables the current observer before installing the new one:

```rust
pub fn enable_observation(&mut self, config: ObserverConfig) {
   self.disable_observation();                 // drops the old ObservableSqliteDatabase
   self.observer = Some(ObservableSqliteDatabase::new(Arc::clone(&self.inner), config));
}
```

The observer is a single slot; there is no additive/multi-observer path. The dropped
`ObservableSqliteDatabase` was the sole owner of its `Arc<ObservationBroker>`, and the
broker owns the only `broadcast::Sender`
([broker.rs:54](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/broker.rs#L54)).
`subscribe()` hands out receivers via `change_tx.subscribe()`
([broker.rs:208](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/broker.rs#L208))
but does not keep the broker alive. So when the old observer is dropped, the sender drops,
and every prior receiver's next `recv()` returns `RecvError::Closed`.

The behavior is documented at the crate level
([wrapper.rs:390–395](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L390-L395)):
"the previous observer is disabled first … causing existing subscriber streams to
terminate. Callers must re-subscribe after re-enabling observation." That contract assumes
one coordinating caller. The `observe` command exposes `enable_observation` to independent
callers who cannot know when to "re-subscribe," and it aborts prior plugin subscription
tasks unconditionally via `remove_for_db`
([commands.rs:592](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/src/commands.rs#L592)),
so the disconnection is silent across callers.

## Expected behavior

Re-observing should not silently disconnect existing subscribers. Concretely, one of:

   - Make `observe` / `enable_observation` idempotent: if the requested table set and
     config match the active observer, keep the existing broker and its subscribers rather
     than tearing it down and rebuilding it; or
   - Support additive/multiple observers (or reference-counted observation) so independent
     callers can each observe a database without evicting one another; or, at minimum,
   - Surface the disconnection — return a distinguishable error or a dedicated close reason
     to affected receivers — so a caller can tell "someone re-observed" apart from a normal
     shutdown, instead of an undifferentiated `Closed`.

## How to run

```
cargo run
```

from this case directory. The program establishes a live subscriber, re-enables
observation on the same handle, then probes both subscribers and prints `BUG CONFIRMED` or
`BUG NOT REPRODUCED`. The subscriber created after the re-enable receiving its event proves
the write published and the probe is sound, so the first subscriber's closure is the defect
and not a missing write.

## Relevant source files

All at rev `e1b3836` in [silvermine/tauri-plugin-sqlite](https://github.com/silvermine/tauri-plugin-sqlite/tree/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81):

| File | Lines | What it contains |
|---|---|---|
| [`crates/sqlx-sqlite-toolkit/src/wrapper.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L390-L402) | [390–402](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L390-L402) | `enable_observation` — disables (drops) the current observer before installing the new one; doc comment states existing streams terminate |
| [`crates/sqlx-sqlite-toolkit/src/wrapper.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L411-L413) | [411–413](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L411-L413) | `disable_observation` — sets `observer = None`, dropping the `ObservableSqliteDatabase` and its broker |
| [`crates/sqlx-sqlite-observer/src/broker.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/broker.rs#L52-L58) | [52–58](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/broker.rs#L52-L58) | `ObservationBroker` — owns the sole `change_tx: broadcast::Sender<TableChange>` |
| [`crates/sqlx-sqlite-observer/src/broker.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/broker.rs#L208-L210) | [208–210](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/crates/sqlx-sqlite-observer/src/broker.rs#L208-L210) | `subscribe()` — hands out a `broadcast::Receiver` without keeping the broker alive |
| [`src/commands.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/src/commands.rs#L573-L617) | [573–617](https://github.com/silvermine/tauri-plugin-sqlite/blob/e1b38366c04d973c3ab4e85cc56fa7ceb26efa81/src/commands.rs#L573-L617) | `observe` command — mutates the cached wrapper and calls `remove_for_db` + `enable_observation`, exposing the teardown to independent callers |

## Related (separate) issue

Observation state is per-`DatabaseWrapper`-handle rather than shared across clones, so
enabling observation on one clone does not observe writes made through sibling clones. That
is a distinct defect with its own repro in
`cases/003-sqlx-sqlite-toolkit-observation-not-shared-across-clones`.
