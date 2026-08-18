//! Repro: destroying one webview window disables observation that Rust enabled,
//! silently ending a Rust subscriber's stream.
//!
//! `tauri-plugin-sqlite` reference-counts observation per *webview label*: the
//! `observe` IPC command records the calling webview in `ObserverRegistrations`,
//! and observation is torn down for the whole database once that set empties.
//! A Rust caller that enables observation on a `DatabaseWrapper` directly
//! records nothing there, so it is invisible to the count while being fully
//! subject to its teardown.
//!
//! The trigger this case reproduces needs no cooperation from anyone: when a
//! window is destroyed, the plugin's `RunEvent::WindowEvent { Destroyed }`
//! handler releases every registration that window held, and any database whose
//! label set just emptied gets `disable_observation()`. The Rust subscriber's
//! broker is dropped out from under it, its stream ends, and its task exits.
//!
//! Layout of the run. Three windows: one that observes, one that never does and
//! gets destroyed as a control, and one that never does and stays open so the
//! process outlives the other two.
//!   Step 1  Rust connects to MAIN, enables observation, subscribes (as a
//!           Rust-side setup step would).
//!   Step 2  The observer window calls `observe(MAIN, ['Item'])` over IPC.
//!   Step 3  Control: a committed write reaches the Rust subscriber, proving
//!           the subscription is live and the probe is sound.
//!   Step 4  Control: destroy a window that never called `observe()`. Its
//!           release finds no registration, so nothing is torn down and the
//!           Rust subscriber keeps working - destroying a window is not by
//!           itself what breaks this.
//!   Step 5  Destroy the observer window. No `unobserve()` is called by anyone.
//!   Step 6  Probe the internal state: `is_observing()` on the database the
//!           Rust side is still holding.
//!   Step 7  Commit another write and probe the Rust subscriber again.

use std::time::Duration;

use futures::StreamExt;
use sqlx_sqlite_observer::ObserverConfig;
use sqlx_sqlite_observer::change::TableChangeEvent;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_sqlite::{Connection, DatabaseWrapper};
use tempfile::TempDir;
use tokio::sync::mpsc;

const DB_KEY: &str = "MAIN";
const TABLE: &str = "Item";
const OBSERVER_WINDOW: &str = "observer-window";
/// Never observes; destroyed in Step 4 as a control.
const BYSTANDER_WINDOW: &str = "bystander-window";
/// Never observes; stays open so destroying the other two does not end the process.
const KEEPALIVE_WINDOW: &str = "keepalive-window";

/// How long to wait for a change to reach the Rust subscriber.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long to wait for the observer window to boot and call `observe()`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// The `Destroyed` handler releases registrations from a spawned task, so the
/// teardown lands shortly after `destroy()` returns rather than during it.
const TEARDOWN_GRACE: Duration = Duration::from_millis(750);

/// What the observer window reports back once its IPC calls have run.
enum WindowSignal {
   Observing,
   Failed(String),
}

/// What the Rust-side subscriber task saw.
enum SubscriberSignal {
   Change(String),
   StreamEnded,
}

struct Handshake(mpsc::UnboundedSender<WindowSignal>);

#[tauri::command]
fn observer_window_ready(handshake: tauri::State<'_, Handshake>) {
   let _ = handshake.0.send(WindowSignal::Observing);
}

#[tauri::command]
fn observer_window_failed(handshake: tauri::State<'_, Handshake>, reason: String) {
   let _ = handshake.0.send(WindowSignal::Failed(reason));
}

fn main() {
   let temp_dir = TempDir::new().expect("create temp dir");
   let db_path = temp_dir.path().join("main.db");
   let (handshake_tx, handshake_rx) = mpsc::unbounded_channel();

   let sqlite = tauri_plugin_sqlite::Builder::new()
      .register_database(DB_KEY, &db_path, None)
      .expect("register database")
      .build()
      .expect("build sqlite plugin");

   let app = tauri::Builder::default()
      .plugin(sqlite)
      .manage(Handshake(handshake_tx))
      .invoke_handler(tauri::generate_handler![
         observer_window_ready,
         observer_window_failed
      ])
      .setup(move |app| {
         let handle = app.handle().clone();

         open_window(&handle, OBSERVER_WINDOW, "observer")?;
         open_window(&handle, BYSTANDER_WINDOW, "bystander")?;
         open_window(&handle, KEEPALIVE_WINDOW, "bystander")?;

         tauri::async_runtime::spawn(async move {
            run_repro(handle, handshake_rx).await;
         });

         Ok(())
      })
      .build(tauri::generate_context!())
      .expect("build app");

   app.run_return(|_, _| {});
   drop(temp_dir);
}

fn open_window(app: &AppHandle, label: &str, role: &str) -> tauri::Result<()> {
   let url = WebviewUrl::App(format!("index.html?role={role}").into());

   WebviewWindowBuilder::new(app, label, url)
      .title(label)
      .inner_size(420.0, 220.0)
      .build()?;

   Ok(())
}

async fn run_repro(app: AppHandle, mut handshake: mpsc::UnboundedReceiver<WindowSignal>) {
   println!("=== tauri-plugin-sqlite: destroying a window kills Rust-side observation ===\n");

   println!("[Step 1] Rust connects to {DB_KEY}, enables observation, and subscribes");
   let db = app.connect(DB_KEY).await.expect("connect to MAIN");
   db.execute(
      format!("CREATE TABLE IF NOT EXISTS {TABLE} (id INTEGER PRIMARY KEY, val TEXT)"),
      vec![],
   )
   .execute()
   .await
   .expect("create table");

   db.enable_observation(ObserverConfig::new().with_tables([TABLE]));
   let mut changes = spawn_rust_subscriber(&db);
   println!(
      "  observation enabled by Rust; is_observing() = {}",
      db.is_observing()
   );
   println!("  this registered nothing in the plugin's per-webview observer count\n");

   println!("[Step 2] Waiting for '{OBSERVER_WINDOW}' to call observe({DB_KEY}, ['{TABLE}'])");
   match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake.recv()).await {
      Ok(Some(WindowSignal::Observing)) => {
         println!("  observer window is registered; the other two never touched {DB_KEY}\n");
      }
      Ok(Some(WindowSignal::Failed(reason))) => {
         return abort(
            &app,
            &format!("the observer window's IPC calls failed: {reason}"),
         );
      }
      _ => {
         return abort(
            &app,
            &format!("the observer window never reported in within {HANDSHAKE_TIMEOUT:?}"),
         );
      }
   }

   println!("[Step 3] Control: commit a write and check the Rust subscriber is live");
   commit_insert(&db, "before-window-close").await;
   let control_live = match probe(&mut changes).await {
      Some(SubscriberSignal::Change(table)) => {
         println!("  Rust subscriber received a change on '{table}' (as expected)\n");
         true
      }
      Some(SubscriberSignal::StreamEnded) => {
         println!("  Rust subscriber's stream already ended (unexpected)\n");
         false
      }
      None => {
         println!("  Rust subscriber received nothing within {PROBE_TIMEOUT:?} (unexpected)\n");
         false
      }
   };

   println!("[Step 4] Control: destroy '{BYSTANDER_WINDOW}', which never called observe()");
   destroy_window(&app, BYSTANDER_WINDOW).await;
   let observing_after_control = db.is_observing();
   commit_insert(&db, "after-unrelated-window-close").await;
   let survives_unrelated_close = match probe(&mut changes).await {
      Some(SubscriberSignal::Change(table)) => {
         println!("  is_observing() = {observing_after_control}");
         println!("  Rust subscriber received a change on '{table}' - still working\n");
         true
      }
      _ => {
         println!("  Rust subscriber stopped after an unrelated window closed (unexpected)\n");
         false
      }
   };

   println!("[Step 5] Destroy '{OBSERVER_WINDOW}'. Nobody calls unobserve()");
   destroy_window(&app, OBSERVER_WINDOW).await;
   println!("  window destroyed; '{KEEPALIVE_WINDOW}' is still open, so the process lives on\n");

   println!("[Step 6] Probe the database the Rust side is still holding");
   let still_observing = db.is_observing();
   println!("  is_observing() = {still_observing}\n");

   println!("[Step 7] Commit another write and probe the Rust subscriber again");
   commit_insert(&db, "after-window-close").await;
   let subscriber_dead = match probe(&mut changes).await {
      Some(SubscriberSignal::Change(table)) => {
         println!("  Rust subscriber received a change on '{table}' (still alive)\n");
         false
      }
      Some(SubscriberSignal::StreamEnded) => {
         println!("  Rust subscriber's stream ended - its task fell out of the loop and exited\n");
         true
      }
      None => {
         println!("  Rust subscriber received nothing within {PROBE_TIMEOUT:?}\n");
         true
      }
   };

   println!("=== Summary ===");
   if control_live && survives_unrelated_close && subscriber_dead && !still_observing {
      println!(
         "BUG CONFIRMED: destroying the one window that had called observe() disabled\n\
          observation for the whole database, ending a subscription that window never\n\
          created and never asked to end. The Rust caller enabled observation itself and\n\
          called no plugin API, so it holds no webview label and is invisible to the\n\
          reference count that decided the teardown - while being fully subject to it.\n\
          The two controls rule out the alternatives: the same subscriber received a\n\
          committed change moments earlier (so the probe is sound and the write\n\
          published), and destroying a window that never observed left it untouched (so\n\
          it is the released registration doing this, not window teardown in general).\n\
          Nothing re-enables observation, so every later write takes the unobserved path."
      );
   } else {
      println!(
         "BUG NOT REPRODUCED: control_live={control_live}, \
          survives_unrelated_close={survives_unrelated_close},\n\
          subscriber_dead={subscriber_dead}, still_observing={still_observing}. Either a\n\
          Rust-side registration now participates in the count, or the probe conditions\n\
          were not met."
      );
   }

   app.exit(0);
}

/// The Rust-side setup step from the report: enable observation, take a stream,
/// hand it to a long-lived task.
///
/// The `ObservableSqliteDatabase` handle is dropped once the stream exists, as
/// it would be in a setup function that returns only the stream. That matters
/// for the *shape* of the failure, not for whether it happens: the handle owns
/// an `Arc` of the broker, so holding it would keep the channel open and leave
/// the subscriber silently idle instead of ended. Either way no further change
/// is ever published, because the writer path reads the database's observer
/// slot, which the teardown cleared.
fn spawn_rust_subscriber(db: &DatabaseWrapper) -> mpsc::UnboundedReceiver<SubscriberSignal> {
   let observable = db.observable().expect("observation enabled");
   let mut stream = observable.subscribe_stream([TABLE]);
   let (tx, rx) = mpsc::unbounded_channel();

   drop(observable);

   tauri::async_runtime::spawn(async move {
      while let Some(event) = stream.next().await {
         if let TableChangeEvent::Change(change) = event {
            let _ = tx.send(SubscriberSignal::Change(change.table));
         }
      }
      let _ = tx.send(SubscriberSignal::StreamEnded);
   });

   rx
}

/// Destroys a window and waits for the plugin's `Destroyed` handler, which
/// releases registrations from a task spawned off the event rather than inline.
async fn destroy_window(app: &AppHandle, label: &str) {
   app.get_webview_window(label)
      .unwrap_or_else(|| panic!("window '{label}' exists"))
      .destroy()
      .unwrap_or_else(|_| panic!("destroy window '{label}'"));
   tokio::time::sleep(TEARDOWN_GRACE).await;
}

async fn commit_insert(db: &DatabaseWrapper, val: &str) {
   db.execute(
      format!("INSERT INTO {TABLE} (val) VALUES ('{val}')"),
      vec![],
   )
   .execute()
   .await
   .expect("insert");
}

async fn probe(
   changes: &mut mpsc::UnboundedReceiver<SubscriberSignal>,
) -> Option<SubscriberSignal> {
   tokio::time::timeout(PROBE_TIMEOUT, changes.recv())
      .await
      .ok()
      .flatten()
}

fn abort(app: &AppHandle, reason: &str) {
   println!("\n=== Summary ===");
   println!("BUG NOT REPRODUCED: {reason}");
   app.exit(1);
}
