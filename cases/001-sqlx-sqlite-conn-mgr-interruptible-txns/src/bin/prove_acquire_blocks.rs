//! Control experiment for the acquire_writer probe in main.rs.
//!
//! Claim under test: if the writer pool is NOT idle (a WriteGuard is held elsewhere),
//! then `tokio::time::timeout(500ms, db.inner().acquire_writer())` returns
//! `Err(Elapsed)`. If that holds, then in main.rs the fast success observed after
//! dropping an interruptible tx proves the pool IS idle.
//!
//! Experiment phases:
//!
//!   A. Cold probe with nobody holding the writer → expect fast success.
//!   B. Hold a WriteGuard, then probe → expect timeout.
//!   C. Drop the held WriteGuard, probe again → expect fast success.

use std::time::{Duration, Instant};

use sqlx_sqlite_toolkit::DatabaseWrapper;
use tempfile::TempDir;

#[tokio::main]
async fn main() {
   println!("=== prove_acquire_blocks: control experiment for acquire_writer probe ===\n");

   let temp_dir = TempDir::new().expect("tempdir");
   let db_path = temp_dir.path().join("control.db");
   let db = DatabaseWrapper::connect(&db_path, None)
      .await
      .expect("connect");

   // ---- Phase A — cold probe, no holder ----
   println!("[A] Cold probe (no holder) — expect fast success");
   probe(&db, "A").await;
   println!();

   // ---- Phase B — hold writer, probe ----
   println!("[B] Hold WriteGuard, then probe — expect timeout");
   let held = db.inner().acquire_writer().await.expect("acquire held");
   println!("  held writer acquired; pool writer semaphore now exhausted");
   probe(&db, "B").await;
   drop(held);
   println!("  held writer dropped\n");

   // ---- Phase C — after drop, probe again ----
   println!("[C] Post-drop probe — expect fast success");
   probe(&db, "C").await;
   println!();

   // cleanup
   let _ = db.remove().await;
   drop(temp_dir);

   println!("=== Summary ===");
   println!(
      "If A + C are fast and B times out, the assumption holds: fast return from\n\
         acquire_writer under 500ms timeout => nobody else held the writer =>\n\
         pool considered the connection idle/available."
   );
}

async fn probe(db: &DatabaseWrapper, label: &str) {
   let start = Instant::now();
   let result = tokio::time::timeout(Duration::from_millis(500), db.inner().acquire_writer()).await;
   let elapsed = start.elapsed();
   match result {
      Err(_) => println!("  [{label}] acquire_writer TIMED OUT after {elapsed:?} (pool NOT idle)"),
      Ok(Err(e)) => println!("  [{label}] acquire_writer errored after {elapsed:?}: {e}"),
      Ok(Ok(_w)) => println!("  [{label}] acquire_writer returned in {elapsed:?} (pool IDLE)"),
   }
}
