use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::super::herdr::{
    handle_event_from_socket, HerdrSocketSupervisor, WatchConnector, WatchEventStream,
    WatchSnapshot, WatchTiming,
};
use super::active_daemon;
use crate::settings::DaemonSettings;
use crate::spawner::{LocalSpawner, RuntimeHandle};
use crate::state::{ActiveRun, Daemon};
use crate::store::Store;
use board_core::config::Config;
use board_core::db::{Db, EnqueueRun};
use board_core::protocol::{CardCreateParams, CardStatus, RunOutcome};
use board_herdr::{AgentStatus, HerdrError, HerdrEvent};
use tokio::sync::{broadcast, mpsc, watch};

/// Build two active herdr runs sharing a pane id but living on separate
/// session sockets. The current pane-only matcher is deliberately used to
/// choose which run is *not* socket A, making these tests deterministic
/// despite HashMap iteration order.
fn two_socket_daemon() -> (Arc<Daemon>, i64, i64, i64, i64, PathBuf) {
    let config = Config {
        idle_grace_seconds: 5,
        ..Default::default()
    };
    let db = Db::open_in_memory().unwrap();
    let card_a = db
        .create_card(&CardCreateParams {
            title: "socket A".into(),
            ..Default::default()
        })
        .unwrap();
    let card_b = db
        .create_card(&CardCreateParams {
            title: "socket B".into(),
            ..Default::default()
        })
        .unwrap();
    let run_a = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card_a.id,
            column_id: card_a.column_id,
            harness: "pi",
            argv_json: "[\"pi\"]",
            prompt_snapshot: "prompt A",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: Some("session-a"),
            session: None,
        })
        .unwrap();
    let run_b = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card_b.id,
            column_id: card_b.column_id,
            harness: "pi",
            argv_json: "[\"pi\"]",
            prompt_snapshot: "prompt B",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: Some("session-b"),
            session: None,
        })
        .unwrap();
    for (run, workspace) in [(&run_a, "workspace-a"), (&run_b, "workspace-b")] {
        db.promote_run_uow(run.id, Some(workspace), Some("shared-pane"), None)
            .unwrap();
    }

    let (events_tx, _events_rx) = broadcast::channel(16);
    let (dispatch_tx, _dispatch_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let d = Arc::new(Daemon::new(
        Store::new(db),
        config,
        DaemonSettings::default(),
        PathBuf::from("/tmp/board-watch-two-sockets.db"),
        PathBuf::from("/tmp/board-watch-two-sockets.sock"),
        Arc::new(LocalSpawner::new()),
        None,
        None,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ));
    for (run, card, workspace, socket) in [
        (
            &run_a,
            &card_a,
            "workspace-a",
            PathBuf::from("/tmp/herdr-a.sock"),
        ),
        (
            &run_b,
            &card_b,
            "workspace-b",
            PathBuf::from("/tmp/herdr-b.sock"),
        ),
    ] {
        d.sched.lock().unwrap().active.insert(
            run.id,
            ActiveRun {
                card_id: card.id,
                handle: RuntimeHandle {
                    pane_id: Some("shared-pane".into()),
                    workspace_id: Some(workspace.into()),
                    herdr_socket: Some(socket),
                    ..Default::default()
                },
                started: Instant::now(),
                timeout_deadline: None,
                idle_since: None,
                awaiting_since: None,
                is_local: false,
                pane_id: Some("shared-pane".into()),
            },
        );
    }

    // Current code cannot use this source socket and would select this
    // run. Designate the other run as socket A so the expected target is
    // guaranteed to differ from that incorrect pane-only selection.
    let pane_only_match = {
        let sched = d.sched.lock().unwrap();
        sched
            .active
            .iter()
            .find(|(_, active)| active.pane_id.as_deref() == Some("shared-pane"))
            .map(|(run_id, _)| *run_id)
            .unwrap()
    };
    let source_run = if pane_only_match == run_a.id {
        run_b.id
    } else {
        run_a.id
    };
    let other_run = if source_run == run_a.id {
        run_b.id
    } else {
        run_a.id
    };
    let socket_a = PathBuf::from("/tmp/herdr-a.sock");
    let socket_b = PathBuf::from("/tmp/herdr-b.sock");
    {
        let mut sched = d.sched.lock().unwrap();
        sched
            .active
            .get_mut(&source_run)
            .unwrap()
            .handle
            .herdr_socket = Some(socket_a.clone());
        sched
            .active
            .get_mut(&other_run)
            .unwrap()
            .handle
            .herdr_socket = Some(socket_b);
    }
    d.refresh_watch();
    let source_card = if source_run == run_a.id {
        card_a.id
    } else {
        card_b.id
    };
    let other_card = if other_run == run_a.id {
        card_a.id
    } else {
        card_b.id
    };
    (d, source_run, source_card, other_run, other_card, socket_a)
}

/// A test-only stand-in for one iteration of the event thread's
/// `socket -> stream` loop. The source socket is passed through to the
/// handler so duplicate pane ids remain session-scoped.
fn event_polled_from_socket(d: &Arc<Daemon>, source_socket: &PathBuf, event: HerdrEvent) {
    assert!(d
        .watch
        .lock()
        .unwrap()
        .panes_by_socket
        .contains_key(source_socket));
    handle_event_from_socket(d, source_socket, event);
}

fn shared_status(status: AgentStatus) -> HerdrEvent {
    HerdrEvent::AgentStatusChanged {
        pane_id: "shared-pane".into(),
        workspace_id: None,
        status,
        agent: Some("pi".into()),
    }
}

#[derive(Default)]
struct FakeSocketState {
    connect: HashMap<PathBuf, VecDeque<bool>>,
    snapshots: HashMap<PathBuf, VecDeque<Option<HashMap<String, AgentStatus>>>>,
    events: HashMap<PathBuf, VecDeque<Result<Option<HerdrEvent>, ()>>>,
    log: Vec<String>,
    connects: HashMap<PathBuf, usize>,
    drops: HashMap<PathBuf, usize>,
}

struct FakeSocketConnector(Arc<Mutex<FakeSocketState>>);

struct FakeSocketStream {
    socket: PathBuf,
    state: Arc<Mutex<FakeSocketState>>,
}

impl Drop for FakeSocketStream {
    fn drop(&mut self) {
        *self
            .state
            .lock()
            .unwrap()
            .drops
            .entry(self.socket.clone())
            .or_default() += 1;
    }
}

impl WatchEventStream for FakeSocketStream {
    fn poll_event(&mut self, _timeout: Duration) -> board_herdr::Result<Option<HerdrEvent>> {
        let mut state = self.state.lock().unwrap();
        state.log.push(format!("poll:{}", self.socket.display()));
        match state
            .events
            .entry(self.socket.clone())
            .or_default()
            .pop_front()
        {
            Some(Ok(event)) => Ok(event),
            Some(Err(())) => Err(HerdrError::Disconnected),
            None => Ok(None),
        }
    }
}

impl WatchConnector for FakeSocketConnector {
    fn subscribe(
        &self,
        socket: &Path,
        _panes: &[String],
    ) -> board_herdr::Result<Box<dyn WatchEventStream>> {
        let mut state = self.0.lock().unwrap();
        state.log.push(format!("subscribe:{}", socket.display()));
        *state.connects.entry(socket.to_path_buf()).or_default() += 1;
        let succeeds = state
            .connect
            .entry(socket.to_path_buf())
            .or_default()
            .pop_front()
            .unwrap_or(true);
        if !succeeds {
            return Err(HerdrError::Disconnected);
        }
        Ok(Box::new(FakeSocketStream {
            socket: socket.to_path_buf(),
            state: self.0.clone(),
        }))
    }

    fn snapshot(&self, socket: &Path) -> board_herdr::Result<WatchSnapshot> {
        let mut state = self.0.lock().unwrap();
        state.log.push(format!("snapshot:{}", socket.display()));
        match state
            .snapshots
            .entry(socket.to_path_buf())
            .or_default()
            .pop_front()
        {
            Some(Some(panes)) => Ok(WatchSnapshot { panes }),
            Some(None) => Err(HerdrError::Disconnected),
            None => Ok(WatchSnapshot {
                panes: HashMap::from([
                    ("p1".into(), AgentStatus::Working),
                    ("shared-pane".into(), AgentStatus::Working),
                    ("changed-pane".into(), AgentStatus::Working),
                ]),
            }),
        }
    }
}

fn fake_supervisor(state: Arc<Mutex<FakeSocketState>>) -> HerdrSocketSupervisor {
    HerdrSocketSupervisor::new(
        Arc::new(FakeSocketConnector(state)),
        WatchTiming {
            retry_initial: Duration::from_millis(10),
            retry_max: Duration::from_millis(40),
            reconcile: Duration::from_millis(20),
            poll: Duration::ZERO,
        },
    )
}

fn watch_on(d: &Arc<Daemon>, run_id: i64, socket: &Path, pane: &str) {
    let mut sched = d.sched.lock().unwrap();
    let active = sched.active.get_mut(&run_id).unwrap();
    active.pane_id = Some(pane.into());
    active.handle.pane_id = Some(pane.into());
    active.handle.herdr_socket = Some(socket.to_path_buf());
    drop(sched);
    d.refresh_watch();
}

#[test]
fn fake_socket_appearing_after_initial_failure_connects_without_restart() {
    let (d, run_id, _, _) = active_daemon();
    let socket = PathBuf::from("/late.sock");
    watch_on(&d, run_id, &socket, "p1");
    let state = Arc::new(Mutex::new(FakeSocketState::default()));
    state
        .lock()
        .unwrap()
        .connect
        .insert(socket.clone(), VecDeque::from([false, true]));
    let mut supervisor = fake_supervisor(state.clone());
    let start = Instant::now();

    supervisor.step(&d, start);
    supervisor.step(&d, start + Duration::from_millis(9));
    supervisor.step(&d, start + Duration::from_millis(10));

    assert_eq!(state.lock().unwrap().connects.get(&socket), Some(&2));
    assert!(supervisor.sockets.get(&socket).unwrap().events.is_some());
}

#[test]
fn fake_socket_disconnect_reconnects_only_the_affected_socket() {
    let (d, _, _, _, _, socket_a) = two_socket_daemon();
    let socket_b = PathBuf::from("/tmp/herdr-b.sock");
    let state = Arc::new(Mutex::new(FakeSocketState::default()));
    state
        .lock()
        .unwrap()
        .events
        .insert(socket_a.clone(), VecDeque::from([Err(()), Ok(None)]));
    let mut supervisor = fake_supervisor(state.clone());
    let start = Instant::now();

    supervisor.step(&d, start);
    supervisor.step(&d, start + Duration::from_millis(10));

    let state = state.lock().unwrap();
    assert_eq!(state.connects.get(&socket_a), Some(&2));
    assert_eq!(state.connects.get(&socket_b), Some(&1));
    assert_eq!(state.drops.get(&socket_b).copied().unwrap_or(0), 0);
}

#[test]
fn fake_socket_subscription_change_does_not_reset_other_socket_generation() {
    let (d, source_run, _, _, _, socket_a) = two_socket_daemon();
    let socket_b = PathBuf::from("/tmp/herdr-b.sock");
    let state = Arc::new(Mutex::new(FakeSocketState::default()));
    let mut supervisor = fake_supervisor(state.clone());
    let start = Instant::now();
    supervisor.step(&d, start);
    let generation_b = supervisor.sockets.get(&socket_b).unwrap().generation;

    watch_on(&d, source_run, &socket_a, "changed-pane");
    supervisor.step(&d, start + Duration::from_millis(1));

    let state = state.lock().unwrap();
    assert_eq!(state.connects.get(&socket_a), Some(&2));
    assert_eq!(state.connects.get(&socket_b), Some(&1));
    assert_eq!(
        supervisor.sockets.get(&socket_b).unwrap().generation,
        generation_b
    );
    assert_eq!(state.drops.get(&socket_b).copied().unwrap_or(0), 0);
}

#[test]
fn fake_socket_generation_orders_subscribe_then_snapshot_then_poll() {
    let (d, run_id, _, _) = active_daemon();
    let socket = PathBuf::from("/ordered.sock");
    watch_on(&d, run_id, &socket, "p1");
    let state = Arc::new(Mutex::new(FakeSocketState::default()));
    fake_supervisor(state.clone()).step(&d, Instant::now());

    assert_eq!(
        state.lock().unwrap().log,
        vec![
            "subscribe:/ordered.sock",
            "snapshot:/ordered.sock",
            "poll:/ordered.sock"
        ]
    );
}

#[test]
fn fake_socket_periodic_snapshot_closes_gap_and_finalizes_only_once() {
    let (d, run_id, card_id, _) = active_daemon();
    let socket = PathBuf::from("/gap.sock");
    watch_on(&d, run_id, &socket, "p1");
    let state = Arc::new(Mutex::new(FakeSocketState::default()));
    state.lock().unwrap().snapshots.insert(
        socket.clone(),
        VecDeque::from([
            Some(HashMap::from([("p1".into(), AgentStatus::Working)])),
            Some(HashMap::new()),
            Some(HashMap::new()),
        ]),
    );
    let mut supervisor = fake_supervisor(state);
    let start = Instant::now();
    supervisor.step(&d, start);
    supervisor.step(&d, start + Duration::from_millis(20));
    supervisor.step(&d, start + Duration::from_millis(40));

    let db = d.store.lock();
    assert_eq!(db.get_run(run_id).unwrap().outcome, Some(RunOutcome::Fail));
    assert_eq!(
        db.list_comments(card_id).unwrap().len(),
        1,
        "duplicate missing-pane observations must not duplicate finalization"
    );
}

#[test]
fn fake_socket_snapshot_failure_is_unknown_and_never_finalizes() {
    let (d, run_id, card_id, _) = active_daemon();
    let socket = PathBuf::from("/unknown.sock");
    watch_on(&d, run_id, &socket, "p1");
    let state = Arc::new(Mutex::new(FakeSocketState::default()));
    state
        .lock()
        .unwrap()
        .snapshots
        .insert(socket, VecDeque::from([None, None]));
    let mut supervisor = fake_supervisor(state);
    let start = Instant::now();
    supervisor.step(&d, start);
    supervisor.step(&d, start + Duration::from_millis(20));

    let db = d.store.lock();
    assert!(db.get_run(run_id).unwrap().ended_at.is_none());
    assert_eq!(
        db.get_card(card_id).unwrap().unwrap().status,
        CardStatus::Running
    );
    assert!(db.list_comments(card_id).unwrap().is_empty());
}

#[test]
fn fake_socket_shutdown_interrupts_retry_without_another_connect() {
    let (d, run_id, _, _) = active_daemon();
    let socket = PathBuf::from("/shutdown.sock");
    watch_on(&d, run_id, &socket, "p1");
    let state = Arc::new(Mutex::new(FakeSocketState::default()));
    state
        .lock()
        .unwrap()
        .connect
        .insert(socket.clone(), VecDeque::from([false, true]));
    let mut supervisor = fake_supervisor(state.clone());
    let start = Instant::now();
    supervisor.step(&d, start);
    d.trigger_shutdown();
    supervisor.step(&d, start + Duration::from_secs(1));

    assert_eq!(state.lock().unwrap().connects.get(&socket), Some(&1));
}

#[test]
fn fake_socket_duplicate_pane_ids_are_routed_by_socket() {
    let (d, source_run, source_card, other_run, other_card, socket_a) = two_socket_daemon();
    let state = Arc::new(Mutex::new(FakeSocketState::default()));
    state.lock().unwrap().events.insert(
        socket_a,
        VecDeque::from([Ok(Some(shared_status(AgentStatus::Blocked)))]),
    );
    fake_supervisor(state).step(&d, Instant::now());

    let db = d.store.lock();
    assert!(db.get_run(source_run).unwrap().ended_at.is_none());
    assert!(db.get_run(other_run).unwrap().ended_at.is_none());
    assert_eq!(
        db.get_card(source_card).unwrap().unwrap().status,
        CardStatus::Blocked
    );
    assert_eq!(
        db.get_card(other_card).unwrap().unwrap().status,
        CardStatus::Running
    );
}

#[test]
fn socket_a_status_event_does_not_update_socket_b_duplicate_pane() {
    let (d, source_run, source_card, other_run, other_card, socket_a) = two_socket_daemon();

    event_polled_from_socket(&d, &socket_a, shared_status(AgentStatus::Blocked));

    let db = d.store.lock();
    assert_eq!(db.get_run(source_run).unwrap().ended_at, None);
    assert_eq!(db.get_run(other_run).unwrap().ended_at, None);
    assert_eq!(
        db.get_card(source_card).unwrap().unwrap().status,
        CardStatus::Blocked,
        "socket A's event must update socket A's card",
    );
    assert_eq!(
        db.get_card(other_card).unwrap().unwrap().status,
        CardStatus::Running,
        "socket A's event must not update socket B's card",
    );
}

#[test]
fn socket_a_pane_exit_does_not_finalize_socket_b_duplicate_pane() {
    let (d, source_run, source_card, other_run, other_card, socket_a) = two_socket_daemon();

    event_polled_from_socket(
        &d,
        &socket_a,
        HerdrEvent::PaneExited {
            pane_id: "shared-pane".into(),
            workspace_id: None,
        },
    );

    let db = d.store.lock();
    assert_eq!(
        db.get_run(source_run).unwrap().outcome,
        Some(RunOutcome::Fail)
    );
    assert!(db.get_run(other_run).unwrap().ended_at.is_none());
    assert_eq!(
        db.get_card(source_card).unwrap().unwrap().status,
        CardStatus::Failed,
        "socket A's pane exit must finalize socket A's run",
    );
    assert_eq!(
        db.get_card(other_card).unwrap().unwrap().status,
        CardStatus::Running,
        "socket A's pane exit must not finalize socket B's run",
    );
}
