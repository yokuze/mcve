# Bug Repro: Closing One Window Permanently Stops Observation That Rust Started

## Problem

[`tauri-plugin-sqlite`](https://github.com/silvermine/tauri-plugin-sqlite/tree/b31142edf6814848995a6df13280f2a8a0a2cd80)
(pinned rev `b31142e`) reference-counts observation per *webview label*: the `observe` IPC
command records the calling webview, and observation is disabled for the whole database
once that set of labels empties. A Rust caller that enables observation on a
`DatabaseWrapper` directly records nothing there, so it is invisible to the count while
being fully subject to its teardown.

Destroying a window needs no cooperation from anyone. When the one window that had called
`observe()` closes, the plugin releases its registrations, finds the database at zero
observers, and calls `disable_observation()` — ending a Rust subscription that window never
created and never asked to end. The Rust task's `while let Some(event) = stream.next()`
loop falls through and the task exits. Nothing re-enables observation and nothing logs a
word about it, so every later write takes the unobserved path for the rest of the process's
life.

This does not require closing the last window, which would end the process anyway. It
requires any window to close while another stays open, with the closing one being the only
webview that had registered that database.

## Root cause

`enable_observation` on the toolkit's `DatabaseWrapper`
([wrapper.rs:636](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L636))
creates or reuses the database's broker and returns. It records nothing in the plugin's
`ObserverRegistrations`, because it lives a layer below and has no webview label to record.
Only `commands::observe` registers one
([commands.rs:867](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/commands.rs#L867-L869)):

```rust
let observer_count = observer_regs
   .register(&mut instances, &db_key, webview.label())
   .await;
```

The registry is a `HashMap<String, HashSet<String>>` of database key to webview labels
([subscriptions.rs:290](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/subscriptions.rs#L290)),
so "how many observers does this database have" is only ever "how many webviews called
`observe()`". There is no other kind of entry it can hold.

On `RunEvent::WindowEvent { Destroyed }`
([lib.rs:708](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/lib.rs#L708-L748))
the plugin releases every registration the destroyed label held, then tears down each
database whose label set just emptied:

```rust
let newly_unobserved = observer_regs
   .release_all_for_label(&mut instances, &label)
   .await;
...
for db_key in newly_unobserved {
   active_subs.remove_for_db(&db_key).await;
   if let Some(wrapper) = instances.get_mut(&db_key) {
      wrapper.disable_observation();
   }
}
```

`release_all_for_label`
([subscriptions.rs:401](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/subscriptions.rs#L401-L423))
reports a database as newly unobserved purely on its label set becoming empty, which is the
correct answer to the question it is asked and the wrong answer to the question the caller
is really asking. `disable_observation()`
([wrapper.rs:735](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L735-L737))
then clears the database's observer slot, dropping the broker. The Rust subscriber's
`broadcast::Receiver` sees its sender go away and the stream ends; had the subscriber kept
its `ObservableSqliteDatabase` handle alive it would instead sit silently idle forever,
since every writer reads the now-empty slot and takes the unobserved path.

The asymmetry is the whole defect: `observe()` is the only way to *join* the count, and it
requires a webview label, but `disable_observation()` applies to the whole database
regardless of who enabled it.

## Already documented — why this is still worth filing

This behavior is described in three places at the pinned rev, including the window case
specifically:

   * `disable_observation`'s doc
     ([wrapper.rs:706](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L706-L718)):
     "the plugin's `unobserve()` (or a window being destroyed) can drive that count to zero
     and call this method on the database you are observing, ending your subscription
     without you having called anything."
   * `unobserve`'s doc
     ([commands.rs:1040](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/commands.rs#L1040-L1045)).
   * The README's change-notification caveats
     ([README.md:666](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/README.md#L666-L679)),
     which offer the workaround: "a Rust consumer that must not be torn down needs its own
     database file … or must re-enable observation after a teardown."

Documented is not the same as tolerable here, for three reasons.

The `Destroyed` trigger leaves nothing to coordinate with. The advice to "coordinate above
this crate" works when a caller performs an action — a frontend module calling `unobserve()`
can be made to consult a Rust owner first. A window closing is not an action anyone
performs at a point where coordination could be inserted; it happens because a user hit a
close button, and the release runs inside the plugin's own event handler.

The suggested workaround contradicts the use case. A Rust consumer observes a database
*because* it needs to react to what the webviews write to it. Giving it "its own database
file" gives it a file nobody else writes to. The other option, "re-enable observation after
a teardown," requires a notification that a teardown happened, and there is none — no hook,
no event, no log line.

The failure is silent and permanent. The Rust task exits, no error is surfaced anywhere,
and a webview that later calls `observe()` gets a fresh broker that the old Rust subscriber
was never bound to.

## Expected behavior

An observation registration taken by a Rust caller should keep observation alive on its own,
and a webview lifecycle event should not be able to tear down a subscription the webview
layer never created.

The fix that fits the existing design is to widen who is allowed to hold a registration.
`ObserverRegistrations` already keeps a set per database, and already tears down when that
set empties. The only thing that has to change is the identity stored in it. Today that
identity is a webview label, which a Rust caller cannot supply. Make it an observer identity
with two shapes — a webview label, or an opaque token handed to a direct Rust caller — and
the count starts answering "how many parties want notifications for this database" instead
of "how many windows do".

Hand the Rust registration back as a guard so releasing it is automatic (names below are
illustrative, not existing API):

```rust
// Joins the same count that observe() joins, without needing a webview label.
let registration = db.register_observer(ObserverConfig::new().with_tables(["Item"]));
let mut stream = db.observable().expect("observation enabled").subscribe_stream(["Item"]);

// Dropping `registration` releases it. Observation is disabled only once every
// webview label and every Rust registration is gone.
```

Making the release automatic matters more than it looks, because a leaked registration is
the reason the `Destroyed` handler exists at all: a window that closed without calling
`unobserve()` used to leave its registration behind and keep the broker alive forever
(issue #54's "phantom registration"). A registration released by `Drop` cannot leak that
way. When the subscriber's task ends or its handle goes out of scope, the registration goes
with it, and no caller has to remember anything. An explicit `release()` alone would hand
Rust callers the same footgun the webview layer needed a lifecycle hook to defuse.

Adding a notification that observation was re-enabled, so a direct subscriber could
reattach, would paper over the asymmetry rather than fix it.

## How to run

```text
cargo run
```

from this case directory. The run needs a desktop session: it opens three real windows,
because the `Destroyed` event this bug depends on comes from the windowing system.
`tauri::test::MockRuntime` cannot substitute — it never emits
`RuntimeWindowEvent::Destroyed`, so the plugin's handler is unreachable from a mock app,
which is worth knowing for anyone writing a regression test for this.

The windows appear for a few seconds and close themselves; the verdict is printed to the
terminal. Two controls are built into the run so the result cannot be dismissed as a broken
probe:

   * Step 3 shows the Rust subscriber receiving a committed change moments before the close,
     proving the subscription is live and writes are publishing.
   * Step 4 destroys a *different* window that never called `observe()`, and the Rust
     subscriber keeps working — so what breaks it in step 5 is the released registration,
     not window teardown in general.

Expected output ends with:

```text
[Step 6] Probe the database the Rust side is still holding
  is_observing() = false

[Step 7] Commit another write and probe the Rust subscriber again
  Rust subscriber's stream ended - its task fell out of the loop and exited

=== Summary ===
BUG CONFIRMED: destroying the one window that had called observe() disabled
observation for the whole database, ...
```

## Relevant source files

All at rev `b31142e` in
[silvermine/tauri-plugin-sqlite](https://github.com/silvermine/tauri-plugin-sqlite/tree/b31142edf6814848995a6df13280f2a8a0a2cd80):

| File | Lines | What it contains |
|---|---|---|
| [`src/lib.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/lib.rs#L708-L748) | [708–748](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/lib.rs#L708-L748) | `RunEvent::WindowEvent { Destroyed }` handler — releases the label's registrations and calls `disable_observation()` on every database that reached zero |
| [`src/subscriptions.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/subscriptions.rs#L401-L423) | [401–423](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/subscriptions.rs#L401-L423) | `release_all_for_label` — returns the databases whose *label* set just emptied |
| [`src/subscriptions.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/subscriptions.rs#L258-L290) | [258–290](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/subscriptions.rs#L258-L290) | `ObserverRegistrations` — `HashMap<db_key, HashSet<webview_label>>`; the webview label is the only observer identity it can hold |
| [`src/commands.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/commands.rs#L862-L873) | [862–873](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/commands.rs#L862-L873) | `observe` — the only place a registration is created, using `webview.label()` |
| [`src/commands.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/commands.rs#L1040-L1045) | [1040–1045](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/src/commands.rs#L1040-L1045) | `unobserve` doc — states the reference count only covers webviews |
| [`crates/sqlx-sqlite-toolkit/src/wrapper.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L636-L692) | [636–692](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L636-L692) | `enable_observation` — creates or reuses the database's broker, registering nothing anywhere |
| [`crates/sqlx-sqlite-toolkit/src/wrapper.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L706-L737) | [706–737](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/crates/sqlx-sqlite-toolkit/src/wrapper.rs#L706-L737) | `disable_observation` — clears the database-wide observer slot; its doc describes this exact scenario |
| [`README.md`](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/README.md#L666-L679) | [666–679](https://github.com/silvermine/tauri-plugin-sqlite/blob/b31142edf6814848995a6df13280f2a8a0a2cd80/README.md#L666-L679) | Change-notification caveats — "The reference count only covers webview windows" |

## Related (separate) issues

Both are earlier defects in the same observation machinery, each with its own repro:

   * `cases/003-sqlx-sqlite-toolkit-observation-not-shared-across-clones` — observation used
     to be per-handle rather than per-database. Fixing that (#53) is what made the present
     case reachable: before it, a directly built observable database had its own broker, so
     a Rust consumer and the plugin could not affect each other. Unifying them is the right
     call, and this is a consequence worth closing rather than a reason to revert.
   * `cases/004-sqlx-sqlite-toolkit-reobserve-kills-existing-subscribers` — re-enabling
     observation used to drop the previous broker, silently terminating existing
     subscribers. The per-webview reference counting this case is about (#54) is what fixed
     it.
