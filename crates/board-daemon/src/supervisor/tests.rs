use super::*;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use crate::spawner::{HerdrLaunchPlan, Spawner};
use board_core::config::Config;
use board_core::db::{Db, EnqueueRun};
use board_core::protocol::{AwaitingReason, CardCreateParams, CardStatus};
use tokio::sync::{broadcast, mpsc, watch};

use crate::settings::{DaemonSettings, SpawnerKind};
use crate::store::Store;

struct NoSpawn;
impl Spawner for NoSpawn {
    fn spawn(&self, _: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
        panic!("reconciliation test must not spawn")
    }
    fn kill(&self, _: &RuntimeHandle) -> anyhow::Result<()> {
        Ok(())
    }
    fn is_alive(&self, _: &RuntimeHandle) -> anyhow::Result<bool> {
        Ok(false)
    }
}

#[derive(Clone)]
struct FixedClock {
    instant: Instant,
    wall_ms: i64,
}
impl ReconcileClock for FixedClock {
    fn now(&self) -> Instant {
        self.instant
    }
    fn wall_now_ms(&self) -> i64 {
        self.wall_ms
    }
}

struct ScriptedResolver {
    targets: HashMap<Option<String>, SessionTarget>,
    calls: Arc<Mutex<Vec<Option<String>>>>,
}
impl SessionResolver for ScriptedResolver {
    fn resolve_target(&self, session: Option<&str>) -> SessionTarget {
        let key = session.map(str::to_string);
        self.calls.lock().unwrap().push(key.clone());
        self.targets
            .get(&key)
            .cloned()
            .unwrap_or(SessionTarget::Unresolved)
    }
}

enum Script {
    Snapshot(RuntimeSnapshot),
    Failure(ProbeFailure),
    Panic,
    FinalizeOk(Arc<Daemon>, i64),
}

struct ScriptedRuntime {
    scripts: Mutex<VecDeque<Script>>,
    calls: Arc<Mutex<Vec<SessionTarget>>>,
}
impl Runtime for ScriptedRuntime {
    fn snapshot(&self, target: &SessionTarget) -> Result<RuntimeSnapshot, ProbeFailure> {
        self.calls.lock().unwrap().push(target.clone());
        match self.scripts.lock().unwrap().pop_front().unwrap() {
            Script::Snapshot(snapshot) => Ok(snapshot),
            Script::Failure(error) => Err(error),
            Script::Panic => panic!("scripted probe panic"),
            Script::FinalizeOk(d, run_id) => {
                // This would deadlock if reconciliation retained the DB or
                // scheduler lock while performing runtime I/O.
                assert!(d.sched.try_lock().is_ok());
                let card_id = d.store.lock().get_run(run_id).unwrap().card_id;
                dispatch::finalize_run(
                    &d,
                    run_id,
                    RunOutcome::Ok,
                    Some("concurrent board done".into()),
                    None,
                    false,
                    false,
                )
                .unwrap();
                assert!(d.store.lock().get_card(card_id).unwrap().is_some());
                Ok(RuntimeSnapshot::new([("pane-1", AgentStatus::Working)]))
            }
        }
    }
}

struct Fixture {
    d: Arc<Daemon>,
    run: Run,
    card: Card,
    clock: Arc<FixedClock>,
}

fn fixture(session: Option<&str>, pane: Option<&str>) -> Fixture {
    let db = Db::open_in_memory().unwrap();
    let card = db
        .create_card(&CardCreateParams {
            title: "recover".into(),
            session: session.map(str::to_string),
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
            session_id: session,
            session,
        })
        .unwrap();
    db.promote_run_uow(run.id, Some("workspace-1"), pane, Some(12_000))
        .unwrap();
    let run = db.get_run(run.id).unwrap();
    let card = db.get_card(card.id).unwrap().unwrap();
    let (events_tx, _) = broadcast::channel(16);
    let (dispatch_tx, _) = mpsc::unbounded_channel();
    let (shutdown_tx, _) = watch::channel(false);
    let d = Arc::new(Daemon::new(
        Store::new(db),
        Config::default(),
        DaemonSettings::default(),
        PathBuf::from("/tmp/t09.db"),
        PathBuf::from("/tmp/t09.sock"),
        Arc::new(NoSpawn),
        None,
        Some(crate::session::SessionRegistry::new(PathBuf::from(
            "/exact-default.sock",
        ))),
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ));
    Fixture {
        d,
        run,
        card,
        clock: Arc::new(FixedClock {
            instant: Instant::now(),
            wall_ms: 10_000,
        }),
    }
}

fn resolver(session: Option<&str>, target: SessionTarget) -> Arc<ScriptedResolver> {
    Arc::new(ScriptedResolver {
        targets: HashMap::from([(session.map(str::to_string), target)]),
        calls: Arc::new(Mutex::new(Vec::new())),
    })
}

fn runtime(script: Script) -> Arc<ScriptedRuntime> {
    Arc::new(ScriptedRuntime {
        scripts: Mutex::new(VecDeque::from([script])),
        calls: Arc::new(Mutex::new(Vec::new())),
    })
}

fn assert_open_unchanged(f: &Fixture) {
    let db = f.d.store.lock();
    let run = db.get_run(f.run.id).unwrap();
    assert!(run.ended_at.is_none());
    assert_eq!(run.outcome, None);
    assert_eq!(
        db.get_card(f.card.id).unwrap().unwrap().status,
        CardStatus::Running
    );
    drop(db);
    assert!(f.d.sched.lock().unwrap().active.is_empty());
    assert!(f.d.watch.lock().unwrap().panes_by_socket.is_empty());
}

#[tokio::test]
async fn unresolved_named_session_is_unknown_without_runtime_io_or_finalize() {
    let f = fixture(Some("missing"), Some("pane-1"));
    let runtime = runtime(Script::Panic);
    reconcile_once(
        &f.d,
        resolver(Some("missing"), SessionTarget::Unresolved),
        runtime.clone(),
        f.clock.clone(),
    )
    .await;
    assert!(runtime.calls.lock().unwrap().is_empty());
    assert_open_unchanged(&f);
}

async fn assert_probe_failure_is_unknown(failure: ProbeFailure) {
    let f = fixture(None, Some("pane-1"));
    reconcile_once(
        &f.d,
        resolver(None, SessionTarget::Default(PathBuf::from("/default.sock"))),
        runtime(Script::Failure(failure)),
        f.clock.clone(),
    )
    .await;
    assert_open_unchanged(&f);
}

#[tokio::test]
async fn runtime_deadline_is_unknown_without_finalize() {
    assert_probe_failure_is_unknown(ProbeFailure::Deadline).await;
}

#[tokio::test]
async fn malformed_snapshot_is_unknown_without_finalize() {
    assert_probe_failure_is_unknown(ProbeFailure::Malformed).await;
}

#[tokio::test]
async fn join_panic_is_unknown_without_finalize() {
    let f = fixture(None, Some("pane-1"));
    reconcile_once(
        &f.d,
        resolver(None, SessionTarget::Default(PathBuf::from("/default.sock"))),
        runtime(Script::Panic),
        f.clock.clone(),
    )
    .await;
    assert_open_unchanged(&f);
}

#[tokio::test]
async fn valid_snapshot_missing_pane_fails_run_without_column_transition() {
    let f = fixture(None, Some("pane-1"));
    let original_column = f.card.column_id;
    reconcile_once(
        &f.d,
        resolver(None, SessionTarget::Default(PathBuf::from("/default.sock"))),
        runtime(Script::Snapshot(RuntimeSnapshot::new([(
            "some-other-pane",
            AgentStatus::Working,
        )]))),
        f.clock.clone(),
    )
    .await;
    let db = f.d.store.lock();
    let run = db.get_run(f.run.id).unwrap();
    let card = db.get_card(f.card.id).unwrap().unwrap();
    assert_eq!(run.outcome, Some(RunOutcome::Fail));
    assert!(run.ended_at.is_some());
    assert_eq!(card.status, CardStatus::Failed);
    assert_eq!(card.column_id, original_column);
}

#[tokio::test]
async fn alive_done_adopts_exact_pane_and_socket_then_enters_awaiting_open() {
    let f = fixture(Some("named"), Some("pane-1"));
    let socket = PathBuf::from("/sessions/named.sock");
    let runtime = runtime(Script::Snapshot(RuntimeSnapshot::new([(
        "pane-1",
        AgentStatus::Done,
    )])));
    reconcile_once(
        &f.d,
        resolver(Some("named"), SessionTarget::Resolved(socket.clone())),
        runtime.clone(),
        f.clock.clone(),
    )
    .await;

    assert_eq!(
        runtime.calls.lock().unwrap().as_slice(),
        &[SessionTarget::Resolved(socket.clone())]
    );
    let sched = f.d.sched.lock().unwrap();
    let active = sched.active.get(&f.run.id).unwrap();
    assert_eq!(active.pane_id.as_deref(), Some("pane-1"));
    assert_eq!(active.handle.pane_id.as_deref(), Some("pane-1"));
    assert_eq!(active.handle.herdr_socket.as_ref(), Some(&socket));
    assert_eq!(
        active.timeout_deadline.unwrap(),
        f.clock.instant + Duration::from_millis(2_000)
    );
    drop(sched);
    assert_eq!(
        f.d.watch.lock().unwrap().panes_by_socket.get(&socket),
        Some(&vec!["pane-1".to_string()])
    );
    let db = f.d.store.lock();
    assert_eq!(
        db.get_card(f.card.id).unwrap().unwrap().awaiting_reason,
        Some(AwaitingReason::AgentDone)
    );
    assert!(db.get_run(f.run.id).unwrap().ended_at.is_none());
}

#[tokio::test]
async fn alive_blocked_restores_status_and_default_socket_watch_identity() {
    let f = fixture(None, Some("pane-1"));
    let socket = PathBuf::from("/exact-default.sock");
    reconcile_once(
        &f.d,
        resolver(None, SessionTarget::Default(socket.clone())),
        runtime(Script::Snapshot(RuntimeSnapshot::new([(
            "pane-1",
            AgentStatus::Blocked,
        )]))),
        f.clock.clone(),
    )
    .await;
    assert_eq!(
        f.d.store
            .lock()
            .get_card(f.card.id)
            .unwrap()
            .unwrap()
            .status,
        CardStatus::Blocked
    );
    let active = f.d.sched.lock().unwrap();
    assert!(active
        .active
        .get(&f.run.id)
        .unwrap()
        .handle
        .herdr_socket
        .is_none());
    drop(active);
    assert_eq!(f.d.default_herdr_socket(), socket);
    assert_eq!(
        f.d.watch.lock().unwrap().panes_by_socket.get(&socket),
        Some(&vec!["pane-1".to_string()])
    );
}

#[tokio::test]
async fn stale_alive_observation_loses_to_concurrent_board_done() {
    let f = fixture(None, Some("pane-1"));
    reconcile_once(
        &f.d,
        resolver(None, SessionTarget::Default(PathBuf::from("/default.sock"))),
        runtime(Script::FinalizeOk(f.d.clone(), f.run.id)),
        f.clock.clone(),
    )
    .await;
    let db = f.d.store.lock();
    assert_eq!(db.get_run(f.run.id).unwrap().outcome, Some(RunOutcome::Ok));
    assert_ne!(
        db.get_card(f.card.id).unwrap().unwrap().status,
        CardStatus::Running,
        "the stale Alive observation must not restore running"
    );
    drop(db);
    assert!(f.d.sched.lock().unwrap().active.is_empty());
    assert!(f.d.watch.lock().unwrap().panes_by_socket.is_empty());
}

#[tokio::test]
async fn duplicate_alive_pass_is_idempotent() {
    let f = fixture(None, Some("pane-1"));
    let target = SessionTarget::Default(PathBuf::from("/default.sock"));
    for _ in 0..2 {
        reconcile_once(
            &f.d,
            resolver(None, target.clone()),
            runtime(Script::Snapshot(RuntimeSnapshot::new([(
                "pane-1",
                AgentStatus::Done,
            )]))),
            f.clock.clone(),
        )
        .await;
    }
    assert_eq!(f.d.sched.lock().unwrap().active.len(), 1);
    assert_eq!(f.d.watch.lock().unwrap().generation, 1);
    let db = f.d.store.lock();
    assert_eq!(
        db.get_card(f.card.id).unwrap().unwrap().awaiting_reason,
        Some(AwaitingReason::AgentDone)
    );
    assert!(db.get_run(f.run.id).unwrap().ended_at.is_none());
}

#[tokio::test]
async fn unknown_persisted_run_still_occupies_global_queue_capacity() {
    let mut f = fixture(None, Some("pane-1"));
    Arc::get_mut(&mut f.d).unwrap().config.max_concurrent = 1;
    let queued_card =
        f.d.store
            .lock()
            .create_card(&CardCreateParams {
                title: "must remain queued".into(),
                ..Default::default()
            })
            .unwrap();
    let queued =
        f.d.store
            .lock()
            .enqueue_run_uow(&EnqueueRun {
                card_id: queued_card.id,
                column_id: queued_card.column_id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
    reconcile_once(
        &f.d,
        resolver(None, SessionTarget::Default(PathBuf::from("/default.sock"))),
        runtime(Script::Failure(ProbeFailure::Transport)),
        f.clock.clone(),
    )
    .await;
    dispatch::dispatch_pass(&f.d).await;
    let queued = f.d.store.lock().get_run(queued.id).unwrap();
    assert!(queued.started_at.is_none());
    assert!(queued.ended_at.is_none());
}

#[tokio::test]
async fn herdr_startup_invokes_reconciliation_even_without_initial_client() {
    let f = fixture(None, Some("pane-1"));
    assert!(f.d.herdr.is_none(), "simulates initial connect failure");
    assert_eq!(f.d.settings.spawner, SpawnerKind::Herdr);
    let runtime = runtime(Script::Failure(ProbeFailure::Transport));
    crate::startup_recovery_with(
        &f.d,
        resolver(None, SessionTarget::Default(PathBuf::from("/default.sock"))),
        runtime.clone(),
        f.clock.clone(),
    )
    .await;
    assert_eq!(runtime.calls.lock().unwrap().len(), 1);
    assert_open_unchanged(&f);
}
