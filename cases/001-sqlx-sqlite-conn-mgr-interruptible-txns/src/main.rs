use std::time::{Duration, Instant};

use serde_json::json;
use sqlx_sqlite_toolkit::{DatabaseWrapper, InterruptibleTransaction};
use tempfile::TempDir;

/// Newtype wrapper so we can observe when the underlying
/// `InterruptibleTransaction` is actually dropped. The wrapper's `Drop`
/// fires first, then the inner `InterruptibleTransaction` drops normally.
struct LoggingTx {
   label: &'static str,
   _inner: InterruptibleTransaction,
}

impl LoggingTx {
   fn new(label: &'static str, inner: InterruptibleTransaction) -> Self {
      Self {
         label,
         _inner: inner,
      }
   }
}

impl Drop for LoggingTx {
   fn drop(&mut self) {
      println!("  [LoggingTx::drop] dropping tx: {}", self.label);
   }
}

#[tokio::main]
async fn main() {
   println!("=== sqlx-sqlite-toolkit InterruptibleTransaction Drop bug repro ===\n");

   // ---- Step 1 ----
   println!("[Step 1] Setup: create temp dir, connect, create test table");

   let temp_dir = TempDir::new().expect("failed to create temp dir");
   let db_path = temp_dir.path().join("test.db");

   println!("  db path: {}", db_path.display());

   let db = DatabaseWrapper::connect(&db_path, None)
      .await
      .expect("connect failed");

   db.execute(
      "CREATE TABLE test (id INTEGER PRIMARY KEY, val TEXT)".into(),
      vec![],
   )
   .await
   .unwrap();

   println!("  table created\n");

   // ---- Step 2 ----
   println!("[Step 2] Start interruptible transaction, drop without commit/rollback");
   {
      let tx = db
         .begin_interruptible_transaction()
         .execute(vec![(
            "INSERT INTO test (val) VALUES (?)",
            vec![json!("uncommitted")],
         )])
         .await
         .expect("begin_interruptible_transaction.execute failed");

      let tx = LoggingTx::new("step2", tx);

      println!("  transaction started + INSERT executed");

      // Drop without commit/rollback — let it fall out of scope.
      drop(tx);
   }
   println!();

   // ---- Step 3 ----
   println!("[Step 3] Probe writer pool state");

   // 3a — probe the raw pool: if the pool still held the connection as "in use",
   // acquire_writer would block until the writer lock released. A fast return indicates
   // the pool considers the writer idle/available.
   //
   // If you doubt whether this is the case, see ./bin/prove_acquire_blocks.rs for a
   // control experiment that validates the acquire_writer probe behavior. Run it with
   // `cargo run --bin prove_acquire_blocks`
   {
      let start = Instant::now();
      let writer_result =
         tokio::time::timeout(Duration::from_millis(500), db.inner().acquire_writer()).await;

      match writer_result {
         Err(_) => {
            println!(
               "  acquire_writer timed out — pool is likely NOT idle (connection never returned)"
            )
         }
         Ok(Err(e)) => println!("  acquire_writer errored: {e}"),
         Ok(Ok(_writer)) => {
            println!(
               "  acquire_writer returned in {:?} — pool likely considers writer IDLE/available",
               start.elapsed()
            );
         }
      }
   }

   // 3b — reproduce the user-visible failure via the crate's public API.
   let second = db
      .begin_interruptible_transaction()
      .execute(vec![(
         "INSERT INTO test (val) VALUES (?)",
         vec![json!("second")],
      )])
      .await;

   let second_failed = second.is_err();
   match &second {
      Ok(_) => println!("  second begin_interruptible_transaction SUCCEEDED (unexpected)"),
      Err(e) => println!("  second begin_interruptible_transaction FAILED: {e}"),
   }

   if let Ok(tx) = second {
      let _ = tx.rollback().await;
   }
   println!();

   // ---- Step 4 ----
   println!("[Step 4] Verify data state via read pool");
   match db.fetch_all("SELECT val FROM test".into(), vec![]).await {
      Ok(rows) => {
         println!("  row count: {}", rows.len());
         for row in &rows {
            println!("  row: {row:?}");
         }
      }
      Err(e) => println!("  fetch_all failed: {e}"),
   }
   println!();

   // ---- Step 5 ----
   println!("[Step 5] Cleanup");
   if let Err(e) = db.remove().await {
      println!("  db.remove() failed: {e}");
   } else {
      println!("  db.remove() ok");
   }
   drop(temp_dir);
   println!();

   // ---- Summary ----
   println!("=== Summary ===");
   if second_failed {
      println!(
         "BUG CONFIRMED: Dropping an interruptible transaction without commit/rollback\n\
             leaves the write connection with a dangling open transaction. The next caller\n\
             to acquire the writer gets a connection where BEGIN IMMEDIATE fails because\n\
             a transaction is already active."
      );
   } else {
      println!(
         "BUG NOT REPRODUCED: The second transaction succeeded. The auto-rollback\n\
             may have been fixed, or the test conditions were not met."
      );
   }
}
