//! Repro: observation on a `DatabaseWrapper` is per-handle, not per-database.
//!
//! `DatabaseWrapper` is `#[derive(Clone)]` and all clones share ONE underlying
//! write connection (`Arc<SqliteDatabase>`). But `enable_observation` mutates a
//! plain `Option<ObservableSqliteDatabase>` field on `&mut self`, so it turns
//! observation on for *that handle only*. A sibling clone taken from the same
//! source handle keeps `observer: None`, and its `acquire_writer()` therefore
//! returns the regular (bypass) writer — so a committed write through it fires
//! no hooks and reaches no subscriber. The failure is silent: no error, no
//! warning, the event simply never arrives.
//!
//! This is exactly why a cached, shared `DatabaseWrapper` cannot have observation
//! "turned on for the database" by any consumer holding only a clone: you can only
//! observe the specific handle you mutate, plus clones made from it *afterward*. In a
//! Tauri app this is the norm — `app.connect(key)` (the Rust counterpart of the JS
//! `Database.load(key)`) hands out a fresh clone of the cached wrapper on every call,
//! so a Rust observer set up on its own clone silently misses writes made through any
//! other connection to the same database. The only handle later clones inherit from is
//! the cached entry, which the plugin mutates solely from its JS `observe` command
//! (via `get_mut` on its private instance cache) — unreachable from Rust callers.
//!
//! Step 2 reproduces the bug. The Control step proves the probe is sound: enable
//! observation BEFORE cloning and the clone inherits the shared broker, so its
//! writes ARE observed.

use std::time::Duration;

use sqlx_sqlite_observer::ObserverConfig;
use sqlx_sqlite_toolkit::DatabaseWrapper;
use tempfile::TempDir;

const TABLE: &str = "Item";
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

#[tokio::main]
async fn main() {
   println!("=== sqlx-sqlite-toolkit: observation is per-clone, not per-database ===\n");

   let temp_dir = TempDir::new().expect("failed to create temp dir");

   let bug_seen_no_event = run_bug_scenario(temp_dir.path().join("bug.db")).await;
   let control_saw_event = run_control_scenario(temp_dir.path().join("control.db")).await;

   drop(temp_dir);

   println!("=== Summary ===");
   if bug_seen_no_event && control_saw_event {
      println!(
         "BUG CONFIRMED: enable_observation() applies only to the handle it is called\n\
          on. A sibling clone of the same database keeps observer=None, so a committed\n\
          write through that clone is silently invisible to the observing clone's\n\
          subscriber. The control (observe-before-clone) received its event, so the\n\
          probe is sound and the silence in Step 2 is the defect, not a bad probe."
      );
   } else {
      println!(
         "BUG NOT REPRODUCED: bug_step_no_event={bug_seen_no_event}, \
          control_saw_event={control_saw_event}. Either observation now propagates\n\
          across clones, or the probe conditions were not met."
      );
   }
}

/// Models the Tauri usage pattern: a consumer holding a *clone* of the shared/cached
/// wrapper (as returned by `app.connect(key)`) enables observation on that clone, while
/// a different consumer writes through a *separate* clone of the same database. Returns
/// `true` if the committed write produced NO event on the observer's subscriber (the bug).
async fn run_bug_scenario(db_path: std::path::PathBuf) -> bool {
   println!("[Step 1] Connect the shared 'cached' handle and create the table");
   let base = DatabaseWrapper::connect(&db_path, None)
      .await
      .expect("connect base");
   base
      .execute(
         format!("CREATE TABLE {TABLE} (id INTEGER PRIMARY KEY, val TEXT)"),
         vec![],
      )
      .await
      .expect("create table");
   println!("  base connected; is_observing = {}\n", base.is_observing());

   println!("[Step 2] Consumer A clones the handle and enables observation ON THE CLONE");
   let mut observer_handle = base.clone();
   observer_handle.enable_observation(ObserverConfig::new().with_tables([TABLE]));
   let mut subscriber = observer_handle
      .observable()
      .expect("observation enabled on observer_handle")
      .subscribe([TABLE]);
   // Internal-state probe: the two handles now DISAGREE about whether the same
   // database is observed — the divergence that produces the silent miss below.
   println!("  base.is_observing            = {}", base.is_observing());
   println!(
      "  observer_handle.is_observing = {}   <- only this handle observes\n",
      observer_handle.is_observing()
   );

   println!("[Step 3] Consumer B (a separate app.connect clone) writes through it");
   let writer_handle = base.clone();
   println!(
      "  writer_handle.is_observing   = {}   <- routes through the regular (bypass) writer",
      writer_handle.is_observing()
   );
   commit_insert(&writer_handle, "from-other-clone").await;
   println!("  committed one row through the non-observing clone\n");

   println!("[Step 4] Did Consumer A's subscriber receive the committed change?");
   let event = tokio::time::timeout(PROBE_TIMEOUT, subscriber.recv()).await;
   let no_event = match event {
      Err(_elapsed) => {
         println!("  no event within {PROBE_TIMEOUT:?} — the write was silently unobserved");
         true
      }
      Ok(Ok(change)) => {
         println!("  received a change event (unexpected): {change:?}");
         false
      }
      Ok(Err(recv_err)) => {
         println!("  subscriber channel error (unexpected): {recv_err}");
         false
      }
   };
   println!();

   base.remove().await.expect("remove bug db");
   no_event
}

/// Control experiment: enable observation on the base handle BEFORE cloning, so
/// the clone inherits the shared broker (matching how the plugin's `observe`
/// command mutates the cached instance so future `connect` clones observe).
/// Returns `true` if a committed write through the clone DID reach the
/// subscriber — proving the probe detects real events.
async fn run_control_scenario(db_path: std::path::PathBuf) -> bool {
   println!("[Control] Enable observation on the base BEFORE handing out clones");
   let mut base = DatabaseWrapper::connect(&db_path, None)
      .await
      .expect("connect control base");
   base
      .execute(
         format!("CREATE TABLE {TABLE} (id INTEGER PRIMARY KEY, val TEXT)"),
         vec![],
      )
      .await
      .expect("create table");
   base.enable_observation(ObserverConfig::new().with_tables([TABLE]));
   let mut subscriber = base
      .observable()
      .expect("observation enabled on base")
      .subscribe([TABLE]);

   let clone = base.clone();
   println!(
      "  clone.is_observing = {}   <- clone made after enable inherits the shared broker",
      clone.is_observing()
   );
   commit_insert(&clone, "from-clone").await;

   let event = tokio::time::timeout(PROBE_TIMEOUT, subscriber.recv()).await;
   let saw_event = matches!(event, Ok(Ok(_)));
   match event {
      Ok(Ok(change)) => println!("  subscriber received the change: {change:?}"),
      Ok(Err(e)) => println!("  subscriber channel error: {e}"),
      Err(_) => println!("  no event within {PROBE_TIMEOUT:?} (unexpected for the control)"),
   }
   println!();

   base.remove().await.expect("remove control db");
   saw_event
}

/// Commit one INSERT through the wrapper's `acquire_writer()`. Whether that
/// routes through the observable or the regular writer depends solely on
/// whether observation is enabled on *this* handle.
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
