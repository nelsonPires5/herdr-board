pub(super) use std::io::{BufRead, BufReader, Write};
pub(super) use std::os::unix::net::UnixListener;
pub(super) use std::path::PathBuf;
pub(super) use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
pub(super) use std::sync::{Arc, Condvar, Mutex};
pub(super) use std::thread;
pub(super) use std::time::{Duration, Instant};

pub(super) use super::enqueue::enqueue_run;
pub(super) use super::finalize::{finalize_run, finalize_run_timeout};
pub(super) use super::pass::{
    dispatch_pass, harness_prompt_env, launch_session, register_spawned_run,
};
pub(super) use super::space::{find_workspace_by_label, resolve_space, resolve_workspace_ref};
pub(super) use crate::settings::DaemonSettings;
pub(super) use crate::spawner::{HerdrLaunchPlan, RuntimeHandle, Spawner};
pub(super) use crate::state::{ActiveRun, Daemon};
pub(super) use crate::store::Store;
pub(super) use board_core::config::Config;
pub(super) use board_core::db::{Db, EnqueueRun, FinalizeRun, LifecycleFaultPoint};
pub(super) use board_core::model::{Card, Run};
pub(super) use board_core::prompt::{assemble_prompt, effective_settings};
pub(super) use board_core::protocol::{
    AwaitingReason, CardCreateParams, CardStatus, CardUpdateParams, ColumnCreateParams,
    ColumnUpdateParams, Effort, Event, Patch, RunOutcome, SpaceKind, Trigger,
};
pub(super) use board_core::{Error, Result};
pub(super) use board_herdr::{AgentStatus, HerdrClient, WorkspaceInfo};
pub(super) use serde_json::Value;
pub(super) use tokio::sync::{broadcast, mpsc, watch};

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

mod concurrency;
mod enqueue;
mod finalize;
mod placement;
