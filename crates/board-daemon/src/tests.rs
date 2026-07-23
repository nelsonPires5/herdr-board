use super::*;
use std::time::Duration;

use crate::spawner::{HerdrLaunchPlan, Spawner};
use board_core::db::EnqueueRun;
use board_core::engine::AgentSignal;
use board_core::protocol::{
    AwaitingReason, CardCreateParams, CardStatus, ColumnUpdateParams, Patch,
};

struct AliveSpawner;

impl Spawner for AliveSpawner {
    fn spawn(&self, _req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
        unreachable!("adoption test does not spawn")
    }

    fn kill(&self, _handle: &RuntimeHandle) -> anyhow::Result<()> {
        Ok(())
    }

    fn is_alive(&self, _handle: &RuntimeHandle) -> anyhow::Result<bool> {
        Ok(true)
    }
}

#[tokio::test]
async fn adopted_awaiting_run_keeps_timeout_paused_until_work_resumes() {
    let db = Db::open_in_memory().unwrap();
    let card = db
        .create_card(&CardCreateParams {
            title: "review across restart".into(),
            ..Default::default()
        })
        .unwrap();
    db.update_column(&ColumnUpdateParams {
        id: card.column_id,
        timeout_minutes: Patch::Set(1),
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
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    db.promote_run_uow(run.id, Some("w1"), Some("p1"), Some(now_ms + 60_000))
        .unwrap();
    db.pause_run_timeout_uow(card.id, AwaitingReason::AgentDone, now_ms)
        .unwrap();

    let (events_tx, _events_rx) = broadcast::channel(16);
    let (dispatch_tx, _dispatch_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let d = Arc::new(Daemon::new(
        Store::new(db),
        Config::default(),
        DaemonSettings::default(),
        PathBuf::from("/tmp/board-adopt.db"),
        PathBuf::from("/tmp/board-adopt.sock"),
        Arc::new(AliveSpawner),
        None,
        None,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ));

    adopt_runs(&d).await;
    let original_deadline = {
        let mut sched = d.sched.lock().unwrap();
        let active = sched.active.get_mut(&run.id).unwrap();
        assert!(active.awaiting_since.is_some());
        let deadline = active.timeout_deadline.unwrap();
        active.awaiting_since = Some(active.awaiting_since.unwrap() - Duration::from_secs(3600));
        deadline
    };

    watchers::apply_signal(&d, run.id, card.id, AgentSignal::Working);

    let resumed = d.store.lock().get_card(card.id).unwrap().unwrap();
    assert_eq!(resumed.status, CardStatus::Running);
    let sched = d.sched.lock().unwrap();
    let active = sched.active.get(&run.id).unwrap();
    assert!(active.awaiting_since.is_none());
    assert!(
        active.timeout_deadline.unwrap() >= original_deadline + Duration::from_secs(3599),
        "long post-restart review must be excluded from elapsed timeout"
    );
}
