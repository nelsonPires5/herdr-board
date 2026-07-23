use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::enqueue::enqueue_run;
use super::finalize::{finalize_run, finalize_run_timeout};
use super::pass::{dispatch_pass, harness_prompt_env, launch_session, register_spawned_run};
use super::space::{find_workspace_by_label, resolve_space, resolve_workspace_ref};
use crate::settings::DaemonSettings;
use crate::spawner::{HerdrLaunchPlan, RuntimeHandle, Spawner};
use crate::state::{ActiveRun, Daemon};
use crate::store::Store;
use board_core::config::Config;
use board_core::db::{Db, EnqueueRun, FinalizeRun, LifecycleFaultPoint};
use board_core::model::{Card, Run};
use board_core::prompt::{assemble_prompt, effective_settings};
use board_core::protocol::{
    AwaitingReason, CardCreateParams, CardStatus, CardUpdateParams, ColumnCreateParams,
    ColumnUpdateParams, Effort, Event, Patch, RunOutcome, SpaceKind, Trigger,
};
use board_core::{Error, Result};
use board_herdr::{AgentStatus, HerdrClient, WorkspaceInfo};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, watch};

struct MissingPiSpawner;

impl Spawner for MissingPiSpawner {
    fn spawn(&self, req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
        assert_eq!(req.argv.first().map(String::as_str), Some("pi"));
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "pi not found").into())
    }

    fn kill(&self, _h: &RuntimeHandle) -> anyhow::Result<()> {
        Ok(())
    }

    fn is_alive(&self, _h: &RuntimeHandle) -> anyhow::Result<bool> {
        Ok(false)
    }
}

#[derive(Default)]
struct RecordingSpawner {
    kills: AtomicUsize,
    effects: Mutex<Option<Arc<Mutex<Vec<&'static str>>>>>,
}

#[derive(Default)]
struct CapturingSpawner {
    requests: std::sync::Mutex<Vec<HerdrLaunchPlan>>,
}

#[derive(Default)]
struct FaultPromotionSpawner {
    kills: AtomicUsize,
}

#[derive(Default)]
struct PausedSpawner {
    state: Mutex<PausedSpawnerState>,
    changed: Condvar,
    started_notify: tokio::sync::Notify,
}

#[derive(Default)]
struct PausedSpawnerState {
    started: Vec<String>,
    released: bool,
}

impl PausedSpawner {
    fn started(&self) -> Vec<String> {
        self.state.lock().unwrap().started.clone()
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap();
        state.released = true;
        self.changed.notify_all();
    }
}

impl Spawner for PausedSpawner {
    fn spawn(&self, req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
        let mut state = self.state.lock().unwrap();
        state.started.push(req.name.clone());
        self.started_notify.notify_one();
        self.changed.notify_all();
        while !state.released {
            state = self.changed.wait(state).unwrap();
        }
        Ok(RuntimeHandle {
            pid: Some(4242),
            ..Default::default()
        })
    }

    fn kill(&self, _h: &RuntimeHandle) -> anyhow::Result<()> {
        Ok(())
    }

    fn is_alive(&self, _h: &RuntimeHandle) -> anyhow::Result<bool> {
        Ok(true)
    }
}

impl Spawner for FaultPromotionSpawner {
    fn spawn(&self, _req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
        Ok(RuntimeHandle {
            pid: Some(4242),
            workspace_id: Some("spawned-workspace".into()),
            pane_id: Some("spawned-pane".into()),
            ..Default::default()
        })
    }

    fn kill(&self, _h: &RuntimeHandle) -> anyhow::Result<()> {
        self.kills.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn is_alive(&self, _h: &RuntimeHandle) -> anyhow::Result<bool> {
        Ok(true)
    }
}

impl Spawner for CapturingSpawner {
    fn spawn(&self, req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
        self.requests.lock().unwrap().push(req.clone());
        Ok(RuntimeHandle {
            pid: Some(4242),
            ..Default::default()
        })
    }

    fn kill(&self, _h: &RuntimeHandle) -> anyhow::Result<()> {
        Ok(())
    }

    fn is_alive(&self, _h: &RuntimeHandle) -> anyhow::Result<bool> {
        Ok(false)
    }
}

impl Spawner for RecordingSpawner {
    fn spawn(&self, _req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle> {
        unreachable!("registration tests provide the spawned handle")
    }

    fn kill(&self, _h: &RuntimeHandle) -> anyhow::Result<()> {
        self.kills.fetch_add(1, Ordering::SeqCst);
        if let Some(log) = self.effects.lock().unwrap().as_ref() {
            log.lock().unwrap().push("kill");
        }
        Ok(())
    }

    fn is_alive(&self, _h: &RuntimeHandle) -> anyhow::Result<bool> {
        Ok(false)
    }
}

fn test_daemon_with_receivers(
    spawner: Arc<dyn Spawner>,
) -> (
    Arc<Daemon>,
    broadcast::Receiver<Event>,
    mpsc::UnboundedReceiver<()>,
) {
    test_daemon_with_config(spawner, Config::default())
}

fn test_daemon_with_config(
    spawner: Arc<dyn Spawner>,
    config: Config,
) -> (
    Arc<Daemon>,
    broadcast::Receiver<Event>,
    mpsc::UnboundedReceiver<()>,
) {
    let (events_tx, events_rx) = broadcast::channel(16);
    let (dispatch_tx, dispatch_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let daemon = Arc::new(Daemon::new(
        Store::new(Db::open_in_memory().unwrap()),
        config,
        DaemonSettings::default(),
        PathBuf::from("/tmp/board-test.db"),
        PathBuf::from("/tmp/board-test.sock"),
        spawner,
        None,
        None,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ));
    (daemon, events_rx, dispatch_rx)
}

fn test_daemon(spawner: Arc<dyn Spawner>) -> Arc<Daemon> {
    test_daemon_with_receivers(spawner).0
}

fn ws(id: &str, label: &str) -> WorkspaceInfo {
    WorkspaceInfo {
        workspace_id: id.to_string(),
        label: label.to_string(),
        number: 0,
        focused: false,
        active_tab_id: String::new(),
        agent_status: AgentStatus::Unknown,
    }
}

/// Serve exactly the three calls made by `resolve_space`: protocol gate,
/// workspace discovery, and the live pane snapshot. Keeping the fixture
/// single-purpose makes cwd failure tests deterministic and independent of
/// a real Herdr process.
fn workspace_resolution_server(snapshot: Option<Value>) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("workspace-resolution.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    thread::spawn(move || {
        for incoming in listener.incoming().take(3) {
            let Ok(stream) = incoming else { break };
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let request: Value = serde_json::from_str(line.trim()).unwrap();
            let response = match request["method"].as_str().unwrap() {
                "ping" => serde_json::json!({
                    "id": request["id"],
                    "result": {
                        "type": "pong", "version": "0.7.5", "protocol": 17,
                        "capabilities": {}
                    }
                }),
                "workspace.list" => serde_json::json!({
                    "id": request["id"],
                    "result": {"workspaces": [{
                        "workspace_id": "w1", "label": "Feature", "number": 1,
                        "focused": false, "active_tab_id": "", "agent_status": "idle"
                    }]}
                }),
                "session.snapshot" => match &snapshot {
                    Some(snapshot) => serde_json::json!({
                        "id": request["id"],
                        "result": {"snapshot": snapshot}
                    }),
                    None => serde_json::json!({
                        "id": request["id"],
                        "error": {
                            "code": "snapshot_failed",
                            "message": "session snapshot unavailable"
                        }
                    }),
                },
                method => panic!("unexpected workspace resolution method: {method}"),
            };
            writeln!(writer, "{response}").unwrap();
            writer.flush().unwrap();
        }
    });
    (dir, socket)
}

/// Serve the four calls made while creating a missing `new_workspace`:
/// protocol gate, workspace discovery, create, and live pane snapshot.
fn new_workspace_resolution_server(snapshot: Option<Value>) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("new-workspace-resolution.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    thread::spawn(move || {
        for incoming in listener.incoming().take(4) {
            let Ok(stream) = incoming else { break };
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let request: Value = serde_json::from_str(line.trim()).unwrap();
            let response = match request["method"].as_str().unwrap() {
                "ping" => serde_json::json!({
                    "id": request["id"],
                    "result": {
                        "type": "pong", "version": "0.7.5", "protocol": 17,
                        "capabilities": {}
                    }
                }),
                "workspace.list" => serde_json::json!({
                    "id": request["id"], "result": {"workspaces": []}
                }),
                "workspace.create" => serde_json::json!({
                    "id": request["id"],
                    "result": {
                        "type": "workspace_created",
                        "workspace": {
                            "workspace_id": "created-ws", "label": "Created", "number": 1,
                            "focused": false, "active_tab_id": "created-ws:t1",
                            "agent_status": "unknown"
                        },
                        "tab": {
                            "tab_id": "created-ws:t1", "workspace_id": "created-ws",
                            "label": "tab", "focused": false, "number": 1,
                            "pane_count": 1, "agent_status": "unknown"
                        },
                        "root_pane": {
                            "pane_id": "created-ws:p1", "terminal_id": "term-1",
                            "workspace_id": "created-ws", "tab_id": "created-ws:t1",
                            "focused": true, "revision": 0, "agent_status": "unknown"
                        }
                    }
                }),
                "session.snapshot" => match &snapshot {
                    Some(snapshot) => serde_json::json!({
                        "id": request["id"], "result": {"snapshot": snapshot}
                    }),
                    None => serde_json::json!({
                        "id": request["id"],
                        "error": {
                            "code": "snapshot_failed",
                            "message": "created workspace snapshot unavailable"
                        }
                    }),
                },
                method => panic!("unexpected new-workspace resolution method: {method}"),
            };
            writeln!(writer, "{response}").unwrap();
            writer.flush().unwrap();
        }
    });
    (dir, socket)
}

#[test]
fn pi_is_builtin_and_does_not_receive_custom_prompt_env() {
    assert!(harness_prompt_env("pi", "prompt", Some("system")).is_empty());
    assert!(harness_prompt_env("claude", "prompt", Some("system")).is_empty());
    assert_eq!(
        harness_prompt_env("fake", "prompt", Some("system")),
        vec![
            ("BOARD_PROMPT".into(), "prompt".into()),
            (
                "BOARD_SYSTEM_PROMPT".into(),
                board_core::harness::protocol_system_prompt(Some("system")),
            ),
        ]
    );
    // No column prompt → the trailer alone, never a missing env var.
    assert_eq!(
        harness_prompt_env("fake", "prompt", None),
        vec![
            ("BOARD_PROMPT".into(), "prompt".into()),
            (
                "BOARD_SYSTEM_PROMPT".into(),
                board_core::harness::protocol_system_prompt(None),
            ),
        ]
    );
}

#[test]
fn pi_fork_persists_the_new_target_session_id() {
    let d = test_daemon(Arc::new(MissingPiSpawner));
    let (card_id, column_id, old_session) = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "retry".into(),
                harness: Some("pi".into()),
                effort: Some(Effort::Low),
                ..Default::default()
            })
            .unwrap();
        let old_session = "11111111-1111-4111-8111-111111111111";
        db.set_card_session(card.id, old_session).unwrap();
        let prior = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "prior",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: Some(old_session),
                session: None,
            })
            .unwrap();
        db.promote_run_uow(prior.id, None, None, None).unwrap();
        let prior_id = prior.id;
        db.finalize_run_uow(&FinalizeRun {
            run_id: prior_id,
            outcome: RunOutcome::Ok,
            summary: None,
            comments: &[(&format!("agent:{}", prior_id), "done")],
            target_column_id: None,
            final_status: CardStatus::Done,
            final_awaiting_reason: None,
            next: None,
        })
        .unwrap();
        (card.id, card.column_id, old_session.to_string())
    };

    let run = enqueue_run(&d, card_id, column_id, true).unwrap();
    let card = d.store.lock().get_card(card_id).unwrap().unwrap();
    let new_session = card.session_id.unwrap();
    assert_ne!(new_session, old_session);
    assert_eq!(run.session_id.as_deref(), Some(new_session.as_str()));
    assert!(run.launch_spec.is_some());
    assert_eq!(
        run.launch_spec.as_ref().unwrap().execution().argv,
        serde_json::from_str::<Vec<String>>(&run.argv_json).unwrap()
    );
    let argv: Vec<String> = serde_json::from_str(&run.argv_json).unwrap();
    assert!(argv
        .windows(2)
        .any(|w| w == ["--fork", old_session.as_str()]));
    assert!(argv
        .windows(2)
        .any(|w| w == ["--session-id", new_session.as_str()]));
}

#[test]
fn enqueue_run_final_guard_prevents_duplicate_open_runs() {
    let d = test_daemon(Arc::new(MissingPiSpawner));
    let (card_id, column_id) = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "single open run".into(),
                ..Default::default()
            })
            .unwrap();
        (card.id, card.column_id)
    };

    let first = enqueue_run(&d, card_id, column_id, true).unwrap();
    let err = enqueue_run(&d, card_id, column_id, true).unwrap_err();
    assert_eq!(err.code(), 3);
    assert!(err.to_string().contains("open run"));
    let open_runs: Vec<_> = d
        .store
        .lock()
        .list_runs(card_id)
        .unwrap()
        .into_iter()
        .filter(|run| run.ended_at.is_none())
        .collect();
    assert_eq!(open_runs.len(), 1);
    assert_eq!(open_runs[0].id, first.id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_claims_a1_and_b1_before_launch_and_serializes_competing_passes() {
    let spawner = Arc::new(PausedSpawner::default());
    let config = Config {
        max_concurrent: 2,
        ..Default::default()
    };
    let (d, _, _) = test_daemon_with_config(spawner.clone(), config);
    let (a1, a2, b1) = {
        let db = d.store.lock();
        let make = |title: &str, space_ref: &str| {
            db.create_card(&CardCreateParams {
                title: title.into(),
                space_kind: Some(SpaceKind::Workspace),
                space_ref: Some(space_ref.into()),
                ..Default::default()
            })
            .unwrap()
        };
        let a1 = make("A1", "space-a");
        let a2 = make("A2", "space-a");
        let b1 = make("B1", "space-b");
        for card in [&a1, &a2, &b1] {
            db.enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: card.title.as_str(),
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        }
        (a1, a2, b1)
    };

    // Deliberately race two callers. The per-daemon pass lock must keep the
    // second caller behind the first pass's pre-launch claims.
    let first = tokio::spawn({
        let d = d.clone();
        async move { dispatch_pass(&d).await }
    });
    let second = tokio::spawn({
        let d = d.clone();
        async move { dispatch_pass(&d).await }
    });

    while spawner.started().len() < 2 {
        spawner.started_notify.notified().await;
    }
    let started = spawner.started();
    assert_eq!(started.len(), 2, "global cap was exceeded: {started:?}");
    assert!(started
        .iter()
        .any(|name| name.starts_with(&format!("card-{}-", a1.id))));
    assert!(started
        .iter()
        .any(|name| name.starts_with(&format!("card-{}-", b1.id))));
    assert!(!started
        .iter()
        .any(|name| name.starts_with(&format!("card-{}-", a2.id))));

    spawner.release();
    first.await.unwrap();
    second.await.unwrap();

    let db = d.store.lock();
    let active_ids: Vec<_> = db
        .active_runs_with_cards()
        .unwrap()
        .into_iter()
        .map(|(_, card)| card.id)
        .collect();
    let queued_ids: Vec<_> = db
        .queued_runs_with_cards()
        .unwrap()
        .into_iter()
        .map(|(_, card)| card.id)
        .collect();
    assert_eq!(active_ids, vec![a1.id, b1.id]);
    assert_eq!(queued_ids, vec![a2.id]);
    assert_eq!(spawner.started().len(), 2);
}

#[tokio::test]
async fn promotion_fault_reopens_queued_state_without_started_effects_and_kills_handle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("promotion-fault.db");
    let armed = Arc::new(AtomicBool::new(false));
    let fault_armed = armed.clone();
    let db = Db::open_with_lifecycle_fault_hook(&path, move |point| {
        if fault_armed.load(Ordering::SeqCst) && point == LifecycleFaultPoint::PromoteAfterRunUpdate
        {
            return Err(Error::InvalidState("injected promotion fault".into()));
        }
        Ok(())
    })
    .unwrap();
    let card = db
        .create_card(&CardCreateParams {
            title: "promotion fault".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "prompt",
            system_prompt_snapshot: Some("system"),
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    let card_id = card.id;
    let run_id = run.id;
    let spawner = Arc::new(FaultPromotionSpawner::default());
    let (events_tx, mut events_rx) = broadcast::channel(16);
    let (dispatch_tx, mut dispatch_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let d = Arc::new(Daemon::new(
        Store::new(db),
        Config::default(),
        DaemonSettings::default(),
        path.clone(),
        dir.path().join("board.sock"),
        spawner.clone(),
        None,
        None,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ));
    armed.store(true, Ordering::SeqCst);

    dispatch_pass(&d).await;

    assert_eq!(spawner.kills.load(Ordering::SeqCst), 1);
    assert!(!d.sched.lock().unwrap().active.contains_key(&run_id));
    let watch = d.watch.lock().unwrap();
    assert!(watch.panes_by_socket.is_empty());
    assert_eq!(watch.generation, 0);
    drop(watch);
    assert!(matches!(
        events_rx.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        dispatch_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    drop(d);
    let reopened = Db::open(&path).unwrap();
    let card = reopened.get_card(card_id).unwrap().unwrap();
    let run = reopened.get_run(run_id).unwrap();
    assert_eq!(card.status, CardStatus::Queued);
    assert!(run.started_at.is_none());
    assert!(run.herdr_workspace_id.is_none());
    assert!(run.herdr_pane_id.is_none());
}

#[tokio::test]
async fn spawn_failure_finalization_is_atomic_and_uses_finalize_run_uow() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spawn-fail-finalize.db");
    let armed = Arc::new(AtomicBool::new(false));
    let hook_observed = Arc::new(AtomicBool::new(false));
    let fault_armed = armed.clone();
    let fault_observed = hook_observed.clone();
    let db = Db::open_with_lifecycle_fault_hook(&path, move |point| {
        if fault_armed.load(Ordering::SeqCst)
            && point == LifecycleFaultPoint::FinalizeAfterRunUpdate
        {
            fault_observed.store(true, Ordering::SeqCst);
            return Err(Error::InvalidState("injected finalize fault".into()));
        }
        Ok(())
    })
    .unwrap();
    let card = db
        .create_card(&CardCreateParams {
            title: "spawn fail finalize".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "pi",
            argv_json: r#"["pi"]"#,
            prompt_snapshot: "prompt",
            system_prompt_snapshot: Some("system"),
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    let card_id = card.id;
    let run_id = run.id;

    // Capture exact queued card/run/comments before constructing the daemon.
    let captured_card = db.get_card(card_id).unwrap().unwrap();
    let captured_run = db.get_run(run_id).unwrap();
    let captured_comments = db.list_comments(card_id).unwrap();

    let spawner = Arc::new(MissingPiSpawner);
    let (events_tx, mut events_rx) = broadcast::channel(16);
    let (dispatch_tx, mut dispatch_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let d = Arc::new(Daemon::new(
        Store::new(db),
        Config::default(),
        DaemonSettings::default(),
        path.clone(),
        dir.path().join("board.sock"),
        spawner,
        None,
        None,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ));

    // Arm the fault point only before dispatch.
    armed.store(true, Ordering::SeqCst);

    dispatch_pass(&d).await;

    // The hook must have been observed.
    assert!(
        hook_observed.load(Ordering::SeqCst),
        "FinalizeAfterRunUpdate hook was never observed – fail_queued_run bypasses finalize_run_uow"
    );

    // No terminal event or dispatch wake escaped.
    assert!(matches!(
        events_rx.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        dispatch_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    // Reopen DB must exactly equal captured state.
    drop(d);
    let reopened = Db::open(&path).unwrap();
    let card = reopened.get_card(card_id).unwrap().unwrap();
    let run = reopened.get_run(run_id).unwrap();
    let comments = reopened.list_comments(card_id).unwrap();
    assert_eq!(card, captured_card);
    assert_eq!(run, captured_run);
    assert_eq!(comments, captured_comments);
}

#[test]
fn spawned_run_registration_starts_row_card_and_active_bookkeeping_together() {
    let spawner = Arc::new(RecordingSpawner::default());
    let d = test_daemon(spawner.clone());
    let (card_id, run_id) = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "register atomically".into(),
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
        (card.id, run.id)
    };
    let started = Instant::now();

    assert!(register_spawned_run(
        &d,
        run_id,
        RuntimeHandle {
            pid: Some(41),
            ..Default::default()
        },
        started,
        None,
        None,
    )
    .unwrap());

    let sched = d.sched.lock().unwrap();
    let db = d.store.lock();
    assert!(db.get_run(run_id).unwrap().started_at.is_some());
    assert_eq!(
        db.get_card(card_id).unwrap().unwrap().status,
        CardStatus::Running
    );
    assert_eq!(sched.active.get(&run_id).unwrap().handle.pid, Some(41));
    assert_eq!(spawner.kills.load(Ordering::SeqCst), 0);
}

#[test]
fn spawned_run_registration_kills_handle_when_row_was_cancelled() {
    let spawner = Arc::new(RecordingSpawner::default());
    let d = test_daemon(spawner.clone());
    let (card_id, run_id) = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "cancelled during spawn".into(),
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
        db.finalize_run_uow(&FinalizeRun {
            run_id: run.id,
            outcome: RunOutcome::Cancelled,
            summary: Some("cancelled"),
            comments: &[],
            target_column_id: None,
            final_status: CardStatus::Failed,
            final_awaiting_reason: None,
            next: None,
        })
        .unwrap();
        (card.id, run.id)
    };

    assert!(!register_spawned_run(
        &d,
        run_id,
        RuntimeHandle {
            pid: Some(42),
            ..Default::default()
        },
        Instant::now(),
        None,
        None,
    )
    .unwrap());

    let db = d.store.lock();
    let run = db.get_run(run_id).unwrap();
    assert!(run.started_at.is_none());
    assert_eq!(run.outcome, Some(RunOutcome::Cancelled));
    assert_eq!(
        db.get_card(card_id).unwrap().unwrap().status,
        CardStatus::Failed
    );
    drop(db);
    assert!(!d.sched.lock().unwrap().active.contains_key(&run_id));
    assert_eq!(spawner.kills.load(Ordering::SeqCst), 1);
}

#[test]
fn auto_transition_enqueues_once_inside_finalization_transaction() {
    let d = test_daemon(Arc::new(MissingPiSpawner));
    let (card_id, run_id, target_id) = {
        let db = d.store.lock();
        let source = db
            .create_column(&ColumnCreateParams {
                name: "Source".into(),
                trigger: Some(Trigger::Auto),
                ..Default::default()
            })
            .unwrap();
        let target = db
            .create_column(&ColumnCreateParams {
                name: "Target".into(),
                trigger: Some(Trigger::Auto),
                ..Default::default()
            })
            .unwrap();
        db.update_column(&ColumnUpdateParams {
            id: source.id,
            on_success_column_id: Patch::Set(target.id),
            ..Default::default()
        })
        .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                column_id: Some(source.id),
                title: "chain".into(),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: source.id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, None, None, None).unwrap();
        (card.id, run.id, target.id)
    };

    let (_, card) = finalize_run(&d, run_id, RunOutcome::Ok, None, None, false, true).unwrap();

    assert_eq!(card.column_id, target_id);
    assert_eq!(card.status, CardStatus::Queued);
    let runs = d.store.lock().list_runs(card_id).unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs.iter().filter(|run| run.ended_at.is_none()).count(), 1);
    let next = runs.iter().find(|run| run.ended_at.is_none()).unwrap();
    assert!(
        next.launch_spec.is_some(),
        "auto-hop must materialize exactly one v11 spec"
    );
    assert_eq!(next.session, card.session);
}

#[test]
fn finalization_planning_error_preserves_exact_prior_state_and_emits_nothing() {
    let (d, mut events, mut dispatch) = test_daemon_with_receivers(Arc::new(MissingPiSpawner));
    let (card_id, run_id, target_id) = {
        let db = d.store.lock();
        let source = db
            .create_column(&ColumnCreateParams {
                name: "Source".into(),
                ..Default::default()
            })
            .unwrap();
        let target = db
            .create_column(&ColumnCreateParams {
                name: "Target".into(),
                trigger: Some(Trigger::Auto),
                ..Default::default()
            })
            .unwrap();
        db.update_column(&ColumnUpdateParams {
            id: source.id,
            on_success_column_id: Patch::Set(target.id),
            ..Default::default()
        })
        .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                column_id: Some(source.id),
                title: "bad next harness".into(),
                harness: Some("missing".into()),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: source.id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, None, None, None).unwrap();
        db.set_card_awaiting(card.id, AwaitingReason::AgentDone)
            .unwrap();
        (card.id, run.id, target.id)
    };

    let err = finalize_run(&d, run_id, RunOutcome::Ok, None, None, false, true).unwrap_err();
    assert!(err.to_string().contains("unknown harness"));

    let db = d.store.lock();
    let run = db.get_run(run_id).unwrap();
    let card = db.get_card(card_id).unwrap().unwrap();
    assert!(run.ended_at.is_none());
    assert_eq!(run.outcome, None);
    assert_ne!(card.column_id, target_id);
    assert_eq!(card.status, CardStatus::Awaiting);
    assert_eq!(card.awaiting_reason, Some(AwaitingReason::AgentDone));
    assert_eq!(db.list_runs(card_id).unwrap().len(), 1);
    assert!(db.list_comments(card_id).unwrap().is_empty());
    drop(db);
    assert!(events.try_recv().is_err());
    assert!(dispatch.try_recv().is_err());
}

fn file_daemon(
    db: Db,
    path: PathBuf,
    spawner: Arc<dyn Spawner>,
) -> (
    Arc<Daemon>,
    broadcast::Receiver<Event>,
    mpsc::UnboundedReceiver<()>,
) {
    let (events_tx, events_rx) = broadcast::channel(32);
    let (dispatch_tx, dispatch_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let daemon = Arc::new(Daemon::new(
        Store::new(db),
        Config::default(),
        DaemonSettings::default(),
        path,
        PathBuf::from("/tmp/board-finalize-test.sock"),
        spawner,
        None,
        None,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ));
    (daemon, events_rx, dispatch_rx)
}

fn assert_no_effects(
    d: &Arc<Daemon>,
    events: &mut broadcast::Receiver<Event>,
    dispatch: &mut mpsc::UnboundedReceiver<()>,
    spawner: &RecordingSpawner,
    run_id: i64,
) {
    assert_eq!(spawner.kills.load(Ordering::SeqCst), 0);
    assert!(d.sched.lock().unwrap().active.contains_key(&run_id));
    assert!(
        events.try_recv().is_err(),
        "terminal event escaped rollback"
    );
    assert!(
        dispatch.try_recv().is_err(),
        "dispatch wake escaped rollback"
    );
}

#[test]
fn daemon_comment_insert_fault_reopens_exact_prior_state_without_precommit_effects() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("comment-fault.db");
    let db = Db::open(&path).unwrap();
    let (card_id, run_id, column_id) = {
        let card = db
            .create_card(&CardCreateParams {
                title: "comment rollback".into(),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "prompt",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, Some("workspace"), Some("pane"), None)
            .unwrap();
        db.set_card_awaiting(card.id, AwaitingReason::AgentDone)
            .unwrap();
        db.add_comment(card.id, "user", "durable before").unwrap();
        (card.id, run.id, card.column_id)
    };
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER abort_daemon_comment BEFORE INSERT ON comments
             BEGIN SELECT RAISE(ABORT, 'injected daemon comment failure'); END;",
        )
        .unwrap();
    let spawner = Arc::new(RecordingSpawner::default());
    let (d, mut events, mut dispatch) = file_daemon(db, path.clone(), spawner.clone());
    let effects = Arc::new(Mutex::new(Vec::new()));
    *d.effect_log.lock().unwrap() = Some(effects.clone());
    d.sched.lock().unwrap().active.insert(
        run_id,
        ActiveRun {
            card_id,
            handle: RuntimeHandle {
                pane_id: Some("pane".into()),
                ..Default::default()
            },
            started: Instant::now(),
            timeout_deadline: None,
            idle_since: None,
            awaiting_since: Some(Instant::now()),
            is_local: false,
            pane_id: Some("pane".into()),
        },
    );

    let err = finalize_run(
        &d,
        run_id,
        RunOutcome::Cancelled,
        Some("must roll back".into()),
        Some("must not persist".into()),
        true,
        true,
    )
    .unwrap_err();
    assert!(err.to_string().contains("injected daemon comment failure"));
    assert_no_effects(&d, &mut events, &mut dispatch, &spawner, run_id);
    assert!(effects.lock().unwrap().is_empty());
    drop(d);

    let reopened = Db::open(&path).unwrap();
    let run = reopened.get_run(run_id).unwrap();
    let card = reopened.get_card(card_id).unwrap().unwrap();
    assert!(run.ended_at.is_none());
    assert_eq!(run.outcome, None);
    assert_eq!(run.result_summary, None);
    assert_eq!(card.column_id, column_id);
    assert_eq!(card.status, CardStatus::Awaiting);
    assert_eq!(card.awaiting_reason, Some(AwaitingReason::AgentDone));
    let comments = reopened.list_comments(card_id).unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].author, "user");
    assert_eq!(comments[0].body, "durable before");
    assert_eq!(reopened.list_runs(card_id).unwrap().len(), 1);
}

#[test]
fn daemon_auto_hop_enqueue_fault_reopens_exact_prior_state_without_precommit_effects() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auto-hop-fault.db");
    let db = Db::open(&path).unwrap();
    let (card_id, run_id, source_id) = {
        let source = db
            .create_column(&ColumnCreateParams {
                name: "Fault source".into(),
                ..Default::default()
            })
            .unwrap();
        let target = db
            .create_column(&ColumnCreateParams {
                name: "Fault auto target".into(),
                trigger: Some(Trigger::Auto),
                ..Default::default()
            })
            .unwrap();
        db.update_column(&ColumnUpdateParams {
            id: source.id,
            on_success_column_id: Patch::Set(target.id),
            ..Default::default()
        })
        .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "auto hop rollback".into(),
                column_id: Some(source.id),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: source.id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "prompt",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, Some("workspace"), Some("pane"), None)
            .unwrap();
        db.add_comment(card.id, "user", "durable before").unwrap();
        (card.id, run.id, source.id)
    };
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(&format!(
            "CREATE TRIGGER abort_daemon_next BEFORE INSERT ON runs
             WHEN NEW.card_id={card_id}
             BEGIN SELECT RAISE(ABORT, 'injected daemon next enqueue failure'); END;"
        ))
        .unwrap();
    let spawner = Arc::new(RecordingSpawner::default());
    let (d, mut events, mut dispatch) = file_daemon(db, path.clone(), spawner.clone());
    let effects = Arc::new(Mutex::new(Vec::new()));
    *d.effect_log.lock().unwrap() = Some(effects.clone());
    d.sched.lock().unwrap().active.insert(
        run_id,
        ActiveRun {
            card_id,
            handle: RuntimeHandle {
                pane_id: Some("pane".into()),
                ..Default::default()
            },
            started: Instant::now(),
            timeout_deadline: None,
            idle_since: None,
            awaiting_since: None,
            is_local: false,
            pane_id: Some("pane".into()),
        },
    );

    let err = finalize_run(
        &d,
        run_id,
        RunOutcome::Ok,
        Some("must roll back".into()),
        Some("must not persist".into()),
        true,
        true,
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("injected daemon next enqueue failure"));
    assert_no_effects(&d, &mut events, &mut dispatch, &spawner, run_id);
    assert!(effects.lock().unwrap().is_empty());
    assert_eq!(d.sched.lock().unwrap().chain_hops.get(&card_id), None);
    drop(d);

    let reopened = Db::open(&path).unwrap();
    let run = reopened.get_run(run_id).unwrap();
    let card = reopened.get_card(card_id).unwrap().unwrap();
    assert!(run.ended_at.is_none());
    assert_eq!(run.outcome, None);
    assert_eq!(run.result_summary, None);
    assert_eq!(card.column_id, source_id);
    assert_eq!(card.status, CardStatus::Running);
    assert_eq!(card.awaiting_reason, None);
    let comments = reopened.list_comments(card_id).unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].body, "durable before");
    assert_eq!(reopened.list_runs(card_id).unwrap().len(), 1);
}

#[derive(Clone, Copy, Debug)]
enum TerminalPath {
    BoardDone,
    Cancel,
    Timeout,
    PaneExit,
}

fn invoke_terminal_path(d: &Arc<Daemon>, run_id: i64, path: TerminalPath) -> Result<(Run, Card)> {
    match path {
        TerminalPath::BoardDone => finalize_run(
            d,
            run_id,
            RunOutcome::Ok,
            Some("board done".into()),
            None,
            false,
            true,
        ),
        TerminalPath::Cancel => finalize_run(
            d,
            run_id,
            RunOutcome::Cancelled,
            Some("cancel".into()),
            None,
            true,
            false,
        ),
        TerminalPath::Timeout => finalize_run_timeout(
            d,
            run_id,
            Instant::now(),
            RunOutcome::Fail,
            Some("timeout".into()),
            Some("timeout".into()),
            true,
            true,
        )?
        .ok_or_else(|| Error::InvalidState("timeout lost".into())),
        TerminalPath::PaneExit => finalize_run(
            d,
            run_id,
            RunOutcome::Fail,
            Some("pane exit".into()),
            Some("pane exit".into()),
            false,
            false,
        ),
    }
}

#[test]
fn terminal_winner_duplicate_and_stale_matrix_is_idempotent() {
    let paths = [
        TerminalPath::BoardDone,
        TerminalPath::Cancel,
        TerminalPath::Timeout,
        TerminalPath::PaneExit,
    ];
    for winner in paths {
        for loser in paths {
            let spawner = Arc::new(RecordingSpawner::default());
            let (d, mut events, mut dispatch) = test_daemon_with_receivers(spawner.clone());
            let (card_id, run_id) = {
                let db = d.store.lock();
                let card = db
                    .create_card(&CardCreateParams {
                        title: format!("winner {winner:?}, loser {loser:?}"),
                        ..Default::default()
                    })
                    .unwrap();
                let run = db
                    .enqueue_run_uow(&EnqueueRun {
                        card_id: card.id,
                        column_id: card.column_id,
                        harness: "pi",
                        argv_json: "[]",
                        prompt_snapshot: "prompt",
                        system_prompt_snapshot: None,
                        launch_spec_json: None,
                        session_id: None,
                        session: None,
                    })
                    .unwrap();
                db.promote_run_uow(run.id, Some("workspace"), Some("pane"), None)
                    .unwrap();
                (card.id, run.id)
            };
            d.sched.lock().unwrap().active.insert(
                run_id,
                ActiveRun {
                    card_id,
                    handle: RuntimeHandle {
                        pane_id: Some("pane".into()),
                        ..Default::default()
                    },
                    started: Instant::now(),
                    timeout_deadline: Some(Instant::now() - Duration::from_secs(1)),
                    idle_since: None,
                    awaiting_since: None,
                    is_local: false,
                    pane_id: Some("pane".into()),
                },
            );

            let (won_run, won_card) = invoke_terminal_path(&d, run_id, winner).unwrap();
            let won_outcome = won_run.outcome;
            let won_status = won_card.status;
            let won_column = won_card.column_id;
            let won_comments = d.store.lock().list_comments(card_id).unwrap();
            while events.try_recv().is_ok() {}
            while dispatch.try_recv().is_ok() {}
            let kills = spawner.kills.load(Ordering::SeqCst);

            let duplicate = invoke_terminal_path(&d, run_id, loser).unwrap();
            assert_eq!(duplicate.0.outcome, won_outcome, "{winner:?} vs {loser:?}");
            assert_eq!(duplicate.1.status, won_status, "{winner:?} vs {loser:?}");
            assert!(events.try_recv().is_err());
            assert!(dispatch.try_recv().is_err());
            assert_eq!(spawner.kills.load(Ordering::SeqCst), kills);
            assert_eq!(d.store.lock().list_comments(card_id).unwrap(), won_comments);

            let replacement = enqueue_run(&d, card_id, won_column, true).unwrap();
            while events.try_recv().is_ok() {}
            while dispatch.try_recv().is_ok() {}
            let stale = invoke_terminal_path(&d, run_id, loser).unwrap();
            assert_eq!(
                stale.0.outcome, won_outcome,
                "stale {winner:?} vs {loser:?}"
            );
            assert_eq!(spawner.kills.load(Ordering::SeqCst), kills);
            assert!(events.try_recv().is_err());
            assert!(dispatch.try_recv().is_err());
            let db = d.store.lock();
            let replacement = db.get_run(replacement.id).unwrap();
            assert!(replacement.ended_at.is_none());
            assert_eq!(
                db.get_card(card_id).unwrap().unwrap().status,
                CardStatus::Queued
            );
            assert_eq!(db.list_comments(card_id).unwrap(), won_comments);
        }
    }
}

#[test]
fn successful_finalization_records_exact_postcommit_effect_order() {
    let spawner = Arc::new(RecordingSpawner::default());
    let (d, _events, _dispatch) = test_daemon_with_receivers(spawner.clone());
    let (card_id, run_id) = {
        let db = d.store.lock();
        let source = db
            .create_column(&ColumnCreateParams {
                name: "effect source".into(),
                ..Default::default()
            })
            .unwrap();
        let review = db
            .create_column(&ColumnCreateParams {
                name: "Review".into(),
                trigger: Some(Trigger::Manual),
                ..Default::default()
            })
            .unwrap();
        db.update_column(&ColumnUpdateParams {
            id: source.id,
            on_success_column_id: Patch::Set(review.id),
            ..Default::default()
        })
        .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "ordered effects".into(),
                column_id: Some(source.id),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: source.id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "prompt",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, Some("workspace"), Some("pane"), None)
            .unwrap();
        (card.id, run.id)
    };
    d.sched.lock().unwrap().active.insert(
        run_id,
        ActiveRun {
            card_id,
            handle: RuntimeHandle {
                pane_id: Some("pane".into()),
                ..Default::default()
            },
            started: Instant::now(),
            timeout_deadline: None,
            idle_since: None,
            awaiting_since: None,
            is_local: false,
            pane_id: Some("pane".into()),
        },
    );
    let effects = Arc::new(Mutex::new(Vec::new()));
    *d.effect_log.lock().unwrap() = Some(effects.clone());
    *spawner.effects.lock().unwrap() = Some(effects.clone());

    finalize_run(&d, run_id, RunOutcome::Ok, None, None, true, true).unwrap();

    assert_eq!(
        *effects.lock().unwrap(),
        [
            "scheduler",
            "watch",
            "kill",
            "notification",
            "run_ended",
            "board_changed",
            "dispatch_wake"
        ]
    );
}

#[derive(Debug, PartialEq, Eq)]
struct EnqueueSnapshotSpec {
    harness: String,
    model: Option<String>,
    effort: Option<Effort>,
    permission_mode: Option<String>,
    system_prompt: Option<String>,
    fresh_session: bool,
    prompt: String,
    session: Option<String>,
}

// Test-only seam for the authoritative-lock contract: production enqueue
// must call the pure snapshot builders again from the locked state rather
// than persist the values prepared before the lock.
fn authoritative_enqueue_snapshot(
    card: &board_core::model::Card,
    column: &board_core::model::Column,
    comments: &[board_core::model::Comment],
) -> EnqueueSnapshotSpec {
    let settings = effective_settings(card, column).unwrap();
    EnqueueSnapshotSpec {
        harness: settings.harness,
        model: settings.model,
        effort: settings.effort,
        permission_mode: settings.permission_mode,
        system_prompt: settings.system_prompt,
        fresh_session: settings.fresh_session,
        prompt: assemble_prompt(&card.description, comments),
        session: card.session.clone(),
    }
}

#[test]
fn enqueue_snapshot_spec_rebuilds_after_authoritative_card_changes() {
    let d = test_daemon(Arc::new(MissingPiSpawner));
    let (card_id, column_id) = {
        let db = d.store.lock();
        let column = db
            .create_column(&ColumnCreateParams {
                name: "authoritative old".into(),
                system_prompt: Some("old settings".into()),
                model_override: Some("old-model".into()),
                ..Default::default()
            })
            .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "authoritative snapshot".into(),
                column_id: Some(column.id),
                harness: Some("pi".into()),
                description: Some("old prompt".into()),
                session: Some("old-herdr-session".into()),
                ..Default::default()
            })
            .unwrap();
        db.add_comment(card.id, "user", "old comment").unwrap();
        (card.id, column.id)
    };

    let prepared = {
        let db = d.store.lock();
        authoritative_enqueue_snapshot(
            &db.get_card(card_id).unwrap().unwrap(),
            &db.get_column(column_id).unwrap().unwrap(),
            &db.list_comments(card_id).unwrap(),
        )
    };

    {
        let db = d.store.lock();
        db.update_card(&CardUpdateParams {
            id: card_id,
            description: Some("new prompt".into()),
            model: Patch::Set("new-model".into()),
            session: Patch::Set("new-herdr-session".into()),
            ..Default::default()
        })
        .unwrap();
        db.update_column(&ColumnUpdateParams {
            id: column_id,
            system_prompt: Patch::Set("new settings".into()),
            model_override: Patch::Set("new-column-model".into()),
            ..Default::default()
        })
        .unwrap();
        db.add_comment(card_id, "user", "new comment").unwrap();
    }

    let rebuilt = {
        let db = d.store.lock();
        authoritative_enqueue_snapshot(
            &db.get_card(card_id).unwrap().unwrap(),
            &db.get_column(column_id).unwrap().unwrap(),
            &db.list_comments(card_id).unwrap(),
        )
    };
    assert_ne!(prepared, rebuilt);
    assert_eq!(rebuilt.harness, "pi");
    assert_eq!(rebuilt.model.as_deref(), Some("new-column-model"));
    assert_eq!(rebuilt.system_prompt.as_deref(), Some("new settings"));
    assert_eq!(rebuilt.session.as_deref(), Some("new-herdr-session"));
    assert!(rebuilt.prompt.contains("new prompt"));
    assert!(rebuilt.prompt.contains("new comment"));
    assert!(!rebuilt.prompt.contains("old prompt"));
    // Existing comments remain part of the authoritative current list;
    // the new comment must not be dropped while rebuilding.
    assert!(rebuilt.prompt.contains("old comment"));
}

#[tokio::test]
async fn queued_managed_pi_uses_enqueue_time_system_snapshot() {
    let spawner = Arc::new(CapturingSpawner::default());
    let d = test_daemon(spawner.clone());
    let (card_id, column_id) = {
        let db = d.store.lock();
        let column = db
            .create_column(&ColumnCreateParams {
                name: "Execute".into(),
                trigger: Some(Trigger::Auto),
                system_prompt: Some("old column instructions".into()),
                ..Default::default()
            })
            .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "snapshot dispatch".into(),
                column_id: Some(column.id),
                harness: Some("pi".into()),
                description: Some("task body".into()),
                ..Default::default()
            })
            .unwrap();
        (card.id, column.id)
    };
    let run = enqueue_run(&d, card_id, column_id, false).unwrap();
    let exact = run.launch_spec.as_ref().unwrap().execution().clone();
    let old = board_core::harness::protocol_system_prompt(Some("old column instructions"));
    d.store
        .lock()
        .update_card(&CardUpdateParams {
            id: card_id,
            description: Some("edited task must not launch".into()),
            model: Patch::Set("edited-model".into()),
            ..Default::default()
        })
        .unwrap();
    d.store
        .lock()
        .update_column(&ColumnUpdateParams {
            id: column_id,
            system_prompt: Patch::Set("new column instructions".into()),
            ..Default::default()
        })
        .unwrap();

    dispatch_pass(&d).await;

    let requests = spawner.requests.lock().unwrap();
    let req = &requests[0];
    assert_eq!(req.argv, exact.argv);
    assert_eq!(req.agent_kind, exact.agent_kind);
    assert_eq!(req.initial_prompt, exact.initial_prompt);
    assert_eq!(req.system_prompt, exact.system_prompt);
    assert_eq!(req.agent_kind.as_deref(), Some("pi"));
    assert_eq!(
        req.initial_prompt.as_deref(),
        Some(run.prompt_snapshot.as_str())
    );
    assert_eq!(req.system_prompt.as_deref(), Some(old.as_str()));
    assert!(req
        .argv
        .iter()
        .all(|arg| !arg.contains("old column instructions")));
    assert!(req.argv.iter().all(|arg| !arg.contains("task body")));
}

#[tokio::test]
async fn queued_configured_harness_uses_enqueue_time_system_snapshot() {
    let spawner = Arc::new(CapturingSpawner::default());
    let mut d = test_daemon(spawner.clone());
    Arc::get_mut(&mut d).unwrap().config.harness.insert(
        "custom".into(),
        board_core::config::HarnessDef {
            argv: vec!["custom-agent".into()],
            ..Default::default()
        },
    );
    let (card_id, column_id) = {
        let db = d.store.lock();
        let column = db
            .create_column(&ColumnCreateParams {
                name: "Configured".into(),
                system_prompt: Some("configured old".into()),
                ..Default::default()
            })
            .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "configured snapshot".into(),
                column_id: Some(column.id),
                harness: Some("custom".into()),
                description: Some("configured task".into()),
                ..Default::default()
            })
            .unwrap();
        (card.id, column.id)
    };
    let run = enqueue_run(&d, card_id, column_id, false).unwrap();
    let exact = run.launch_spec.as_ref().unwrap().execution().clone();
    Arc::get_mut(&mut d)
        .unwrap()
        .config
        .harness
        .get_mut("custom")
        .unwrap()
        .argv = vec!["edited-agent-must-not-launch".into()];
    d.store
        .lock()
        .update_card(&CardUpdateParams {
            id: card_id,
            description: Some("edited configured task".into()),
            ..Default::default()
        })
        .unwrap();
    d.store
        .lock()
        .update_column(&ColumnUpdateParams {
            id: column_id,
            system_prompt: Patch::Set("configured new".into()),
            ..Default::default()
        })
        .unwrap();
    dispatch_pass(&d).await;
    let requests = spawner.requests.lock().unwrap();
    let req = &requests[0];
    assert_eq!(req.argv, exact.argv);
    assert_eq!(req.agent_kind, exact.agent_kind);
    assert_eq!(req.initial_prompt, exact.initial_prompt);
    assert_eq!(req.system_prompt, exact.system_prompt);
    assert_eq!(&req.env[..exact.env.len()], exact.env.as_slice());
    assert_eq!(req.env.len(), exact.env.len() + 4);
    let env = &req.env;
    assert_eq!(
        env.iter()
            .find(|(k, _)| k == "BOARD_SYSTEM_PROMPT")
            .unwrap()
            .1,
        board_core::harness::protocol_system_prompt(Some("configured old"))
    );
    assert_eq!(
        env.iter().find(|(k, _)| k == "BOARD_BIN").map(|(_, v)| v),
        Some(
            &std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        )
    );
}

#[test]
fn v11_placement_uses_run_session_while_legacy_uses_current_card_session() {
    let d = test_daemon(Arc::new(MissingPiSpawner));
    let card = d
        .store
        .lock()
        .create_card(&CardCreateParams {
            title: "session snapshot".into(),
            session: Some("enqueue-session".into()),
            ..Default::default()
        })
        .unwrap();
    let mut run = enqueue_run(&d, card.id, card.column_id, false).unwrap();
    assert!(run.launch_spec.is_some());
    assert_eq!(run.session.as_deref(), Some("enqueue-session"));

    // Model a queued card edit in the dispatch snapshot: v11 ignores it.
    let mut edited_card = card;
    edited_card.session = Some("edited-session".into());
    assert_eq!(launch_session(&run, &edited_card), Some("enqueue-session"));

    // The same row shape without a v11 spec follows the documented legacy
    // adapter and therefore observes the current card session.
    run.launch_spec = None;
    assert_eq!(launch_session(&run, &edited_card), Some("edited-session"));
}

#[tokio::test]
async fn v7_and_pre_v7_launch_adapters_remain_explicit() {
    let spawner = Arc::new(CapturingSpawner::default());
    let mut config = Config::default();
    config.harness.insert(
        "custom".into(),
        board_core::config::HarnessDef {
            argv: vec!["custom".into()],
            ..Default::default()
        },
    );
    let (d, _, _) = test_daemon_with_config(spawner.clone(), config);
    let (v7_card, legacy_card, column_id) = {
        let db = d.store.lock();
        let column = db
            .create_column(&ColumnCreateParams {
                name: "Adapters".into(),
                system_prompt: Some("current".into()),
                ..Default::default()
            })
            .unwrap();
        let v7 = db
            .create_card(&CardCreateParams {
                title: "v7".into(),
                column_id: Some(column.id),
                harness: Some("custom".into()),
                space_ref: Some("v7".into()),
                ..Default::default()
            })
            .unwrap();
        let legacy = db
            .create_card(&CardCreateParams {
                title: "legacy".into(),
                column_id: Some(column.id),
                harness: Some("custom".into()),
                space_ref: Some("legacy".into()),
                ..Default::default()
            })
            .unwrap();
        db.enqueue_run_uow(&EnqueueRun {
            card_id: v7.id,
            column_id: column.id,
            harness: "custom",
            argv_json: r#"["v7-command"]"#,
            prompt_snapshot: "v7-prompt",
            system_prompt_snapshot: Some("v7-system-exact"),
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
        db.enqueue_run_uow(&EnqueueRun {
            card_id: legacy.id,
            column_id: column.id,
            harness: "custom",
            argv_json: r#"["legacy-command"]"#,
            prompt_snapshot: "legacy-prompt",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
        (v7.id, legacy.id, column.id)
    };
    dispatch_pass(&d).await;
    let requests = spawner.requests.lock().unwrap();
    let v7 = requests.iter().find(|r| r.argv == ["v7-command"]).unwrap();
    assert!(v7
        .env
        .contains(&("BOARD_SYSTEM_PROMPT".into(), "v7-system-exact".into())));
    let legacy = requests
        .iter()
        .find(|r| r.argv == ["legacy-command"])
        .unwrap();
    assert!(legacy.env.contains(&(
        "BOARD_SYSTEM_PROMPT".into(),
        board_core::harness::protocol_system_prompt(Some("current"))
    )));
    assert_ne!(v7_card, legacy_card);
    assert!(column_id > 0);
}

#[tokio::test]
async fn spawn_failure_for_missing_pi_marks_run_failed_with_system_comment() {
    let d = test_daemon(Arc::new(MissingPiSpawner));
    let (card_id, column_id) = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "missing pi".into(),
                ..Default::default()
            })
            .unwrap();
        (card.id, card.column_id)
    };
    let run = enqueue_run(&d, card_id, column_id, false).unwrap();

    dispatch_pass(&d).await;

    let db = d.store.lock();
    let finished = db.get_run(run.id).unwrap();
    assert_eq!(finished.outcome, Some(RunOutcome::Fail));
    assert_eq!(
        db.get_card(card_id).unwrap().unwrap().status,
        CardStatus::Failed
    );
    assert!(db
        .list_comments(card_id)
        .unwrap()
        .iter()
        .any(|comment| comment.author == "system"
            && comment.body.contains("spawn failed")
            && comment.body.contains("pi not found")));
}

#[test]
fn scoped_run_transition_uses_the_cards_board_columns() {
    let d = test_daemon(Arc::new(MissingPiSpawner));
    let (card, run, target) = {
        let db = d.store.lock();
        let board = db.open_board("/scoped").unwrap();
        let auto = db
            .create_column(&ColumnCreateParams {
                board_id: Some(board.id),
                name: "Execute".into(),
                trigger: Some(Trigger::Auto),
                ..Default::default()
            })
            .unwrap();
        let done = db
            .create_column(&ColumnCreateParams {
                board_id: Some(board.id),
                name: "Done".into(),
                ..Default::default()
            })
            .unwrap();
        db.update_column(&ColumnUpdateParams {
            id: auto.id,
            on_success_column_id: Patch::Set(done.id),
            ..Default::default()
        })
        .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                board_id: Some(board.id),
                column_id: Some(auto.id),
                title: "scoped transition".into(),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: auto.id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, None, None, None).unwrap();
        (card, run, done)
    };

    let (_, moved) = finalize_run(&d, run.id, RunOutcome::Ok, None, None, false, true).unwrap();
    assert_eq!(moved.board_id, card.board_id);
    assert_eq!(moved.column_id, target.id);
}

#[test]
fn resolve_ref_by_id_then_label() {
    let all = [ws("w1", "Alpha"), ws("w2", "Beta")];
    assert_eq!(resolve_workspace_ref(&all, "w2").unwrap(), "w2");
    // Case-insensitive label match.
    assert_eq!(resolve_workspace_ref(&all, "alpha").unwrap(), "w1");
}

#[test]
fn resolve_ref_unknown_lists_known() {
    let all = [ws("w1", "Alpha")];
    let err = resolve_workspace_ref(&all, "ghost").unwrap_err();
    assert!(err.contains("ghost"));
    assert!(err.contains("w1"));
}

#[test]
fn new_workspace_reuse_matches_label_case_insensitively() {
    let all = [ws("w1", "Alpha"), ws("w2", "MyFeature")];
    // Reuse: label already open → return its id (no create).
    assert_eq!(
        find_workspace_by_label(&all, "myfeature").as_deref(),
        Some("w2")
    );
}

#[test]
fn new_workspace_create_when_absent() {
    let all = [ws("w1", "Alpha")];
    // Absent → None → dispatch will call workspace.create.
    assert!(find_workspace_by_label(&all, "brand-new").is_none());
}

#[test]
fn existing_workspace_resolution_fails_when_snapshot_fails() {
    let (_dir, socket) = workspace_resolution_server(None);
    let mut client = HerdrClient::connect(&socket).unwrap();
    let err = resolve_space(&mut client, SpaceKind::Workspace, Some("w1"), None)
        .expect_err("a snapshot failure must prevent launch without a cwd");
    assert!(err.to_string().contains("session snapshot unavailable"));
}

#[test]
fn workspace_resolution_fails_without_live_cwd_for_existing_and_reused_spaces() {
    let missing_cwd_snapshot = serde_json::json!({
        "panes": [{
            "pane_id": "w1:p1",
            "workspace_id": "w1",
            "focused": false,
            "revision": 1
        }]
    });

    for (kind, space_ref, space_cwd) in [
        (SpaceKind::Workspace, "w1", None),
        (SpaceKind::NewWorkspace, "Feature", Some("/fallback")),
    ] {
        let (_dir, socket) = workspace_resolution_server(Some(missing_cwd_snapshot.clone()));
        let mut client = HerdrClient::connect(&socket).unwrap();
        let err = resolve_space(&mut client, kind, Some(space_ref), space_cwd)
            .expect_err("a missing live pane cwd must not fall back or be omitted");
        assert!(err.to_string().contains("cwd"), "{err}");
    }
}

#[test]
fn newly_created_workspace_requires_live_snapshot_cwd() {
    for snapshot in [
        None,
        Some(serde_json::json!({
            "panes": [{
                "pane_id": "created-ws:p1",
                "workspace_id": "created-ws",
                "focused": false,
                "revision": 1
            }]
        })),
    ] {
        let (_dir, socket) = new_workspace_resolution_server(snapshot);
        let mut client = HerdrClient::connect(&socket).unwrap();
        let err = resolve_space(
            &mut client,
            SpaceKind::NewWorkspace,
            Some("Created"),
            Some("/requested-but-unverified"),
        )
        .expect_err("a created workspace must prove its cwd from a live pane snapshot");
        assert!(err.to_string().contains("cwd") || err.to_string().contains("snapshot"));
    }
}

#[test]
fn new_workspace_selected_socket_preflights_protocol_before_resolution() {
    // RED: dispatch must gate the selected socket before resolve_space. A
    // mismatched socket must receive exactly ping; workspace.list/create,
    // session.snapshot, and spawner placement must not be reached.
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("selected-herdr.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let methods = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&methods);
    thread::spawn(move || {
        for stream in listener.incoming().take(3) {
            let Ok(stream) = stream else { break };
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let request: Value = serde_json::from_str(line.trim()).unwrap();
            seen.lock()
                .unwrap()
                .push(request["method"].as_str().unwrap().into());
            let result = match request["method"].as_str().unwrap() {
                "ping" => serde_json::json!({
                    "type": "pong", "version": "0.7.4", "protocol": 17,
                    "capabilities": {}
                }),
                "workspace.list" => serde_json::json!({
                    "workspaces": [{
                        "workspace_id": "w1", "label": "feature", "number": 1,
                        "focused": false, "active_tab_id": "", "agent_status": "idle"
                    }]
                }),
                "session.snapshot" => serde_json::json!({}),
                other => panic!("unexpected mutating/placement method: {other}"),
            };
            writeln!(
                writer,
                "{}",
                serde_json::json!({
                    "id": request["id"], "result": result
                })
            )
            .unwrap();
            writer.flush().unwrap();
        }
    });

    let mut client = HerdrClient::connect(&socket).unwrap();
    let result = resolve_space(
        &mut client,
        SpaceKind::NewWorkspace,
        Some("feature"),
        Some("/tmp/feature"),
    );

    let actual_methods = methods.lock().unwrap().clone();
    assert_eq!(actual_methods, vec!["ping"]);
    let err = result.expect_err("protocol mismatch must stop workspace resolution");
    assert!(err
        .to_string()
        .contains("Herdr 0.7.5 with protocol 17 is required"));
}

// ---------------------------------------------------------------------------
// T13: manual enqueue vs auto-hop → identical persisted EnqueueRun fields
// ---------------------------------------------------------------------------

/// `prepare_enqueue_values` produces the same persisted `EnqueueRun` fields
/// whether called from the manual `enqueue_run` path or from the auto-hop
/// path inside `finalize_run`. The comparison excludes the random session id
/// (a fresh UUID minted on each call) as well as row ids and timestamps.
/// Transition-generated comments must not contaminate the auto-hop
/// preparation: `prepare_enqueue_values` reads DB comments, and the
/// transition comment is only persisted inside the same `finalize_run_uow`
/// *after* the next-enqueue values have been prepared.
#[test]
fn prepare_enqueue_values_is_deterministic_for_equivalent_inputs() {
    let spawner = Arc::new(MissingPiSpawner);
    let mut d = test_daemon(spawner.clone());
    Arc::get_mut(&mut d).unwrap().config.harness.insert(
        "custom-det".into(),
        board_core::config::HarnessDef {
            argv: vec!["custom-det-agent".into()],
            ..Default::default()
        },
    );

    // Columns with identical settings so both paths resolve the same
    // effective configuration. Source on_success → Target.
    let (source_id, target_id) = {
        let db = d.store.lock();
        let source = db
            .create_column(&ColumnCreateParams {
                name: "Src".into(),
                trigger: Some(Trigger::Auto),
                system_prompt: Some("col-sys".into()),
                model_override: Some("col-model".into()),
                ..Default::default()
            })
            .unwrap();
        let target = db
            .create_column(&ColumnCreateParams {
                name: "Tgt".into(),
                trigger: Some(Trigger::Auto),
                system_prompt: Some("col-sys".into()),
                model_override: Some("col-model".into()),
                ..Default::default()
            })
            .unwrap();
        db.update_column(&ColumnUpdateParams {
            id: source.id,
            on_success_column_id: Patch::Set(target.id),
            ..Default::default()
        })
        .unwrap();
        (source.id, target.id)
    };

    // One card, first used for the manual path (in target column), then
    // moved to source for the auto-hop path.
    let card_id = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                column_id: Some(target_id),
                title: "det".into(),
                harness: Some("custom-det".into()),
                description: Some("task body".into()),
                session: Some("herdr-session".into()),
                ..Default::default()
            })
            .unwrap();
        db.add_comment(card.id, "user", "user note").unwrap();
        card.id
    };

    // --- Manual path: enqueue directly to target column ---
    let manual = enqueue_run(&d, card_id, target_id, false).unwrap();

    // Complete the manual run quietly (no extra comments) so the card can be
    // reused for the auto-hop path without contaminating the comment list.
    {
        let db = d.store.lock();
        let open = db.open_run_for_card(card_id).unwrap().unwrap();
        db.promote_run_uow(open.id, None, None, None).unwrap();
        db.finalize_run_uow(&FinalizeRun {
            run_id: open.id,
            outcome: RunOutcome::Ok,
            summary: None,
            comments: &[],
            target_column_id: None,
            final_status: CardStatus::Done,
            final_awaiting_reason: None,
            next: None,
        })
        .unwrap();
    }

    // --- Auto-hop path: move card to source, enqueue a dummy run, finalize ---
    d.store.lock().move_card(card_id, source_id, None).unwrap();
    let src_run = {
        let db = d.store.lock();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id,
                column_id: source_id,
                harness: "custom-det",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, None, None, None).unwrap();
        run
    };
    finalize_run(&d, src_run.id, RunOutcome::Ok, None, None, false, true).unwrap();
    let auto = {
        let db = d.store.lock();
        db.list_runs(card_id)
            .unwrap()
            .into_iter()
            .find(|r| r.ended_at.is_none())
            .unwrap()
    };

    // --- Compare: only session_id is non-deterministic ---
    assert!(manual.session_id.is_some());
    assert!(auto.session_id.is_some());
    assert_ne!(manual.session_id, auto.session_id);

    assert_eq!(manual.harness, auto.harness);
    assert_eq!(manual.argv_json, auto.argv_json);
    assert_eq!(manual.prompt_snapshot, auto.prompt_snapshot);
    assert_eq!(manual.system_prompt_snapshot, auto.system_prompt_snapshot);
    assert_eq!(manual.session, auto.session);
    assert_eq!(manual.launch_spec, auto.launch_spec);

    // Self-consistency: argv_json must round-trip through the execution spec.
    let spec = manual.launch_spec.as_ref().unwrap();
    assert_eq!(
        &serde_json::from_str::<Vec<String>>(&manual.argv_json).unwrap(),
        &spec.execution().argv
    );
}
