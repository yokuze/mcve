# Bug Repro: Dropped Interruptible Connections Do Not Roll Back

## Problem

When
[`sqlx-sqlite-toolkit`'s](https://github.com/silvermine/tauri-plugin-sqlite/blob/240eb77/crates/sqlx-sqlite-toolkit)
`InterruptibleTransaction` (backed by `ActiveInterruptibleTransaction`) is dropped without
calling `commit()` or `rollback()`, the underlying write connection is returned to the
pool with an open `BEGIN IMMEDIATE` transaction. Subsequent calls to
`begin_interruptible_transaction` (before the connection pool timeout) cause SQLite to
throw an error: "cannot start a transaction within a transaction."

## Root cause

The `Drop` impl at
[`crates/sqlx-sqlite-toolkit/src/transactions.rs:233-245`](https://github.com/silvermine/tauri-plugin-sqlite/blob/240eb77/crates/sqlx-sqlite-toolkit/src/transactions.rs#L233-L245)
only logs a debug message — it does not issue a `ROLLBACK`, and there is no other
mechanism, other than the connection pool timeout, that will cause a `ROLLBACK`.

SQLx's normal auto-rollback mechanism lives in
[`Transaction<DB>::Drop`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-core/src/transaction.rs#L260-L275):
when a `Transaction` wrapper is dropped while still open, its `Drop` impl
calls
[`TransactionManager::start_rollback`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-core/src/transaction.rs#L41-L43),
which for SQLite queues a
[`Command::Rollback { tx: None }`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-sqlite/src/connection/worker.rs#L87-L89)
to the
[`ConnectionWorker`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-sqlite/src/connection/worker.rs#L32).
That queued rollback then runs before the connection is released, because
[`return_to_pool()`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-core/src/pool/connection.rs#L275-L328)
awaits
[`ping()`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-core/src/pool/connection.rs#L314),
and the worker processes its command queue FIFO.

The interruptible transaction bypasses all of this. It issues raw
`sqlx::query("BEGIN IMMEDIATE").execute()` via
[`TransactionWriter::begin_immediate()`](https://github.com/silvermine/tauri-plugin-sqlite/blob/240eb77/crates/sqlx-sqlite-toolkit/src/transactions.rs#L59-L62),
which goes through
[`Command::Execute`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-sqlite/src/connection/worker.rs#L63-L69)
and never constructs a `Transaction<Sqlite>` wrapper.

When the
[`WriteGuard`](https://github.com/silvermine/tauri-plugin-sqlite/blob/240eb77/crates/sqlx-sqlite-conn-mgr/src/write_guard.rs#L34-L63)
is dropped and its inner
[`PoolConnection<Sqlite>`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-core/src/pool/connection.rs#L21)
is returned to the pool:

1. [`PoolConnection::Drop`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-core/src/pool/connection.rs#L199-L211)
   spawns a task calling
   [`return_to_pool()`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-core/src/pool/connection.rs#L275-L328).
2. `return_to_pool()` awaits
   [`ping()`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-core/src/pool/connection.rs#L314),
   which drains the worker's command queue. The queue contains only the `BEGIN IMMEDIATE`
   + `INSERT` that already ran — no `Command::Rollback` was ever enqueued, because no
   `Transaction` wrapper existed to enqueue one.
3. The connection is released back to the idle pool with `BEGIN IMMEDIATE` still active on
   the underlying `sqlite3*` handle.
4. The next
   [`acquire_writer()`](https://github.com/silvermine/tauri-plugin-sqlite/blob/240eb77/crates/sqlx-sqlite-conn-mgr/src/database.rs#L252)
   call hands out this dirty connection.
5. The next `BEGIN IMMEDIATE` fails with "cannot start a transaction within a
   transaction."

## Expected behavior

The `Drop` impl should issue a `ROLLBACK` before dropping the writer, or the crate should
use SQLx's `TransactionManager` instead of raw SQL so that the existing auto-rollback
machinery works.

## How to run

```
cargo run --bin sqlx-conn-mgr-txn-bug
```

## Relevant source files

All at rev `240eb77` in [silvermine/tauri-plugin-sqlite](https://github.com/silvermine/tauri-plugin-sqlite/tree/240eb77):

| File | Lines | What it contains |
|---|---|---|
| [`crates/sqlx-sqlite-toolkit/src/transactions.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/240eb77/crates/sqlx-sqlite-toolkit/src/transactions.rs#L233-L245) | [233–245](https://github.com/silvermine/tauri-plugin-sqlite/blob/240eb77/crates/sqlx-sqlite-toolkit/src/transactions.rs#L233-L245) | `ActiveInterruptibleTransaction::Drop` — logs but does not rollback |
| [`crates/sqlx-sqlite-toolkit/src/transactions.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/240eb77/crates/sqlx-sqlite-toolkit/src/transactions.rs#L59-L62) | [59–62](https://github.com/silvermine/tauri-plugin-sqlite/blob/240eb77/crates/sqlx-sqlite-toolkit/src/transactions.rs#L59-L62) | `TransactionWriter::begin_immediate()` — issues raw `BEGIN IMMEDIATE` via `sqlx::query` |
| [`crates/sqlx-sqlite-conn-mgr/src/write_guard.rs`](https://github.com/silvermine/tauri-plugin-sqlite/blob/240eb77/crates/sqlx-sqlite-conn-mgr/src/write_guard.rs#L34-L64) | [34–64](https://github.com/silvermine/tauri-plugin-sqlite/blob/240eb77/crates/sqlx-sqlite-conn-mgr/src/write_guard.rs#L34-L64) | `WriteGuard` — no custom `Drop`, relies on `PoolConnection::Drop` |

And in [launchbadge/sqlx](https://github.com/launchbadge/sqlx/tree/v0.8.6) (version 0.8.6):

| File | Lines | What it contains |
|---|---|---|
| [`sqlx-core/src/pool/connection.rs`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-core/src/pool/connection.rs#L199-L211) | [199–211](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-core/src/pool/connection.rs#L199-L211) | `PoolConnection::Drop` — spawns `return_to_pool()` |
| [`sqlx-core/src/pool/connection.rs`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-core/src/pool/connection.rs#L275-L328) | [275–328](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-core/src/pool/connection.rs#L275-L328) | `return_to_pool()` — calls `ping()` then releases to idle pool; no transaction check |
| [`sqlx-sqlite/src/connection/worker.rs`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-sqlite/src/connection/worker.rs#L275-L303) | [275–303](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-sqlite/src/connection/worker.rs#L275-L303) | `Command::Rollback` handler — checks `transaction_depth`, no-ops if depth is 0 |
| [`sqlx-sqlite/src/connection/worker.rs`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-sqlite/src/connection/worker.rs#L38-L42) | [38–42](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-sqlite/src/connection/worker.rs#L38-L42) | `WorkerSharedState` — `transaction_depth: AtomicUsize` only updated by `Command::Begin`/`Commit`/`Rollback`, not by `Command::Execute` |
| [`sqlx-sqlite/src/connection/handle.rs`](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-sqlite/src/connection/handle.rs#L88-L100) | [88–100](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-sqlite/src/connection/handle.rs#L88-L100) | `ConnectionHandle::Drop` — calls `sqlite3_close()`, which is where SQLite actually rolls back |
