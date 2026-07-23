use super::*;
use crate::session::SessionRegistry;
use crate::settings::DaemonSettings;
use crate::spawner::LocalSpawner;
use crate::store::Store;
use board_core::config::{Config, HarnessDef};
use board_core::db::{Db, EnqueueRun, FinalizeRun, LifecycleFaultPoint, BOARD_ID};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use tokio::sync::{broadcast, mpsc, watch};

fn test_daemon(config: Config) -> Arc<Daemon> {
    test_daemon_with_registry(config, None)
}

fn test_daemon_with_registry(
    config: Config,
    session_registry: Option<SessionRegistry>,
) -> Arc<Daemon> {
    let db = Db::open_in_memory().unwrap();
    let store = Store::new(db);
    let (events_tx, _events_rx) = broadcast::channel(16);
    let (dispatch_tx, _dispatch_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    Arc::new(Daemon::new(
        store,
        config,
        DaemonSettings::default(),
        PathBuf::from("/tmp/board-test.db"),
        PathBuf::from("/tmp/board-test.sock"),
        Arc::new(LocalSpawner::new()),
        None, // no herdr
        session_registry,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ))
}

fn add_run_with_pane(d: &Arc<Daemon>, pane: Option<&str>) -> i64 {
    let db = d.store.lock();
    let card = db
        .create_card(&CardCreateParams {
            title: "focus target".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "p",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    db.promote_run_uow(run.id, Some("w1"), pane, None).unwrap();
    card.id
}

fn fake_herdr(reply: &'static str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let stream = incoming.unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                continue;
            }
            let request: Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(request["method"], "pane.focus");
            let id = request["id"].as_str().unwrap();
            writeln!(writer, "{{\"id\":\"{id}\",{reply}}}").unwrap();
            break;
        }
    });
    (dir, path)
}

mod boards;
mod cards;
mod discovery;
mod lifecycle;
mod validation;
