//! Repro: re-enabling observation silently terminates existing subscribers.
//!
//! `DatabaseWrapper::enable_observation` calls `disable_observation()` first,
//! dropping the previous `ObservableSqliteDatabase` and its broadcast broker.
//! Any `broadcast::Receiver` handed out by the old broker then only sees
//! `RecvError::Closed` — the change stream just ends. The code holding that
//! receiver gets no signal that someone else re-observed; it simply stops
//! receiving changes.
//!
//! This bites the moment observation has more than one setup site on one
//! database. In tauri-plugin-sqlite the `observe` IPC command mutates the
//! *cached* wrapper in place, and observation is per webview window: when a
//! second window calls `observe` on a database the first window already
//! observes, the first window's subscription dies silently.
//!
//! Steps 1-2 establish a live subscriber (Window 1). Step 3 models a second
//! consumer (Window 2) re-observing the same handle. Step 4 shows Window 2's
//! new subscriber works (proving the write published and the probe is sound)
//! while Window 1's subscriber is dead.

use std::time::Duration;

use sqlx_sqlite_observer::ObserverConfig;
use sqlx_sqlite_toolkit::DatabaseWrapper;
use tempfile::TempDir;

const TABLE: &str = "Item";
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

#[tokio::main]
async fn main() {
   println!("=== sqlx-sqlite-toolkit: re-observe silently kills existing subscribers ===\n");

   let temp_dir = TempDir::new().expect("failed to create temp dir");
   let db_path = temp_dir.path().join("shared.db");

   println!("[Step 1] Connect the shared 'cached' handle and create the table");
   let mut db = DatabaseWrapper::connect(&db_path, None)
      .await
      .expect("connect");
   db.execute(
      format!("CREATE TABLE {TABLE} (id INTEGER PRIMARY KEY, val TEXT)"),
      vec![],
   )
   .await
   .expect("create table");
   println!("  connected\n");

   println!("[Step 2] Window 1 enables observation and subscribes");
   db.enable_observation(ObserverConfig::new().with_tables([TABLE]));
   let mut window1 = db
      .observable()
      .expect("observation enabled")
      .subscribe([TABLE]);
   // Sanity: Window 1's subscriber is live and receives a committed change.
   commit_insert(&db, "before-reobserve").await;
   match tokio::time::timeout(PROBE_TIMEOUT, window1.recv()).await {
      Ok(Ok(change)) => println!("  window 1 received (as expected): {}", short(&change)),
      other => println!("  window 1 did NOT receive its first change (unexpected): {other:?}"),
   }
   println!();

   println!("[Step 3] Window 2 calls observe again on the SAME cached handle (re-enable)");
   db.enable_observation(ObserverConfig::new().with_tables([TABLE]));
   let mut window2 = db
      .observable()
      .expect("observation re-enabled")
      .subscribe([TABLE]);
   println!("  previous broker dropped; window 2 subscribed to the new one\n");

   println!("[Step 4] Commit one more change, then probe both subscribers");
   commit_insert(&db, "after-reobserve").await;

   let window2_ok = match tokio::time::timeout(PROBE_TIMEOUT, window2.recv()).await {
      Ok(Ok(change)) => {
         println!("  window 2 received (works): {}", short(&change));
         true
      }
      other => {
         println!("  window 2 did NOT receive (unexpected): {other:?}");
         false
      }
   };

   let window1_dead = match tokio::time::timeout(PROBE_TIMEOUT, window1.recv()).await {
      Ok(Ok(change)) => {
         println!("  window 1 still alive (unexpected): {}", short(&change));
         false
      }
      Ok(Err(recv_err)) => {
         println!("  window 1 stream closed: {recv_err}");
         true
      }
      Err(_) => {
         println!("  window 1 received nothing within {PROBE_TIMEOUT:?}");
         true
      }
   };
   println!();

   db.remove().await.expect("remove db");
   drop(temp_dir);

   println!("=== Summary ===");
   if window1_dead && window2_ok {
      println!(
         "BUG CONFIRMED: re-enabling observation dropped the previous broadcast broker,\n\
          so Window 1's existing subscriber stopped receiving changes with no signal —\n\
          it just sees a closed stream. Window 2's new subscriber received the same\n\
          committed change, proving the write published and the probe is sound. A second\n\
          observation setup site (e.g. a second window's `observe` call) silently\n\
          disconnects the first."
      );
   } else {
      println!(
         "BUG NOT REPRODUCED: window1_dead={window1_dead}, window2_ok={window2_ok}. Either\n\
          re-enabling observation now preserves existing subscribers, or the probe\n\
          conditions were not met."
      );
   }
}

/// Commit one INSERT through the wrapper's (currently observable) writer.
async fn commit_insert(db: &DatabaseWrapper, val: &str) {
   let mut writer = db.acquire_writer().await.expect("acquire writer");
   sqlx::query("BEGIN IMMEDIATE")
      .execute(&mut *writer)
      .await
      .expect("begin");
   sqlx::query("INSERT INTO Item (val) VALUES (?)")
      .bind(val)
      .execute(&mut *writer)
      .await
      .expect("insert");
   sqlx::query("COMMIT")
      .execute(&mut *writer)
      .await
      .expect("commit");
}

/// Compact one-line description of a change for readable output.
fn short(change: &sqlx_sqlite_observer::change::TableChange) -> String {
   format!("{} {:?} rowid={:?}", change.table, change.operation, change.rowid)
}
