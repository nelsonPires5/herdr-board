//! Private watcher tests shared setup.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::spawner::RuntimeHandle;
use crate::state::{ActiveRun, Daemon};
use crate::testkit;
use board_core::config::Config;
use board_core::db::{Db, EnqueueRun};
use board_core::protocol::{CardCreateParams, Event};
use board_herdr::{AgentStatus, HerdrEvent};
use tokio::sync::broadcast;

fn active_daemon() -> (Arc<Daemon>, i64, i64, broadcast::Receiver<Event>) {
    let config = Config {
        idle_grace_seconds: 5,
        ..Default::default()
    };
    let db = Db::open_in_memory().unwrap();
    let card = db
        .create_card(&CardCreateParams {
            title: "watch".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "pi",
            argv_json: "[\"pi\"]",
            prompt_snapshot: "prompt",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: Some("session"),
            session: None,
        })
        .unwrap();
    db.promote_run_uow(run.id, Some("w1"), Some("p1"), None)
        .unwrap();

    let built = testkit::daemon()
        .db(db)
        .config(config)
        .db_path(PathBuf::from("/tmp/board-watch.db"))
        .build();
    let d = built.daemon;
    let events_rx = built.events;
    d.sched.lock().unwrap().active.insert(
        run.id,
        ActiveRun {
            card_id: card.id,
            handle: RuntimeHandle::default(),
            started: Instant::now(),
            timeout_deadline: None,
            idle_since: None,
            awaiting_since: None,
            is_local: false,
            pane_id: Some("p1".into()),
        },
    );
    (d, run.id, card.id, events_rx)
}

fn status(status: AgentStatus) -> HerdrEvent {
    HerdrEvent::AgentStatusChanged {
        pane_id: "p1".into(),
        workspace_id: Some("w1".into()),
        status,
        agent: Some("pi".into()),
    }
}

mod recovery;
mod signals;
mod sockets;
mod timeout;
