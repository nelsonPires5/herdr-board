pub(super) use std::path::PathBuf;
pub(super) use std::sync::atomic::{AtomicUsize, Ordering};
pub(super) use std::sync::{Arc, Condvar, Mutex};
pub(super) use std::time::{Duration, Instant};

pub(super) use super::enqueue::enqueue_run;
pub(super) use super::finalize::{finalize_run, finalize_run_timeout};
pub(super) use super::launch_plan::{
    argv_is_fork, board_env, harness_prompt_env, register_spawned_run,
};
pub(super) use super::ownership::{owned_pane_ids, reconstruct_owned_tab_id, OwnedPanes};
pub(super) use super::pass::{dispatch_pass, launch_session};
pub(super) use super::space::{
    find_workspace_by_label, resolve_space, resolve_workspace_ref, validate_space_resolvable,
};
pub(super) use crate::spawner::{HerdrLaunchPlan, RuntimeHandle, SpawnError, Spawner};
pub(super) use crate::state::{ActiveRun, Daemon};
pub(super) use crate::testkit::{self, FakeHerdr};
pub(super) use board_core::config::Config;
pub(super) use board_core::db::{Db, EnqueueRun, FinalizeRun, LifecycleFaultPoint};
pub(super) use board_core::model::{Card, Run};
pub(super) use board_core::prompt::{assemble_prompt, effective_settings};
pub(super) use board_core::protocol::{
    AwaitingReason, CardCreateParams, CardStatus, CardUpdateParams, ColumnCreateParams,
    ColumnUpdateParams, Effort, Event, Patch, RunOutcome, SpaceKind, Trigger,
};
pub(super) use board_core::{Error, Result};
pub(super) use board_herdr::{AgentStatus, HerdrClient, PaneInfo, SessionSnapshot, WorkspaceInfo};
pub(super) use serde_json::Value;
pub(super) use tokio::sync::{broadcast, mpsc};

struct MissingPiSpawner;

impl Spawner for MissingPiSpawner {
    fn spawn(&self, req: &HerdrLaunchPlan) -> std::result::Result<RuntimeHandle, SpawnError> {
        assert_eq!(req.argv.first().map(String::as_str), Some("pi"));
        Err(anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "pi not found",
        ))
        .into())
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
    fn spawn(&self, req: &HerdrLaunchPlan) -> std::result::Result<RuntimeHandle, SpawnError> {
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
    fn spawn(&self, _req: &HerdrLaunchPlan) -> std::result::Result<RuntimeHandle, SpawnError> {
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
    fn spawn(&self, req: &HerdrLaunchPlan) -> std::result::Result<RuntimeHandle, SpawnError> {
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
    fn spawn(&self, _req: &HerdrLaunchPlan) -> std::result::Result<RuntimeHandle, SpawnError> {
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
    testkit::daemon()
        .config(config)
        .spawner(spawner)
        .build_parts()
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

/// Serve exactly the four connections made by `resolve_space`: the connect
/// probe, protocol gate, workspace discovery, and the live pane snapshot. Keeping the fixture
/// single-purpose makes cwd failure tests deterministic and independent of
/// a real Herdr process.
fn workspace_resolution_server(snapshot: Option<Value>) -> FakeHerdr {
    workspace_resolution_server_take(snapshot, 4)
}

/// [`workspace_resolution_server`] with a configurable connection budget.
fn workspace_resolution_server_take(snapshot: Option<Value>, take: usize) -> FakeHerdr {
    testkit::herdr_server()
        .take(take)
        .on("workspace.list", |req| {
            testkit::reply(
                req,
                serde_json::json!({"workspaces": [{
                    "workspace_id": "w1", "label": "Feature", "number": 1,
                    "focused": false, "active_tab_id": "", "agent_status": "idle"
                }]}),
            )
        })
        .on("session.snapshot", move |req| match &snapshot {
            Some(snapshot) => testkit::reply(req, serde_json::json!({"snapshot": snapshot})),
            None => testkit::error(req, "snapshot_failed", "session snapshot unavailable"),
        })
        .serve()
}

/// Serve the four calls made while creating a missing `new_workspace`:
/// protocol gate, workspace discovery, create, and live pane snapshot.
fn new_workspace_resolution_server(snapshot: Option<Value>) -> FakeHerdr {
    new_workspace_resolution_server_take(snapshot, 4)
}

/// [`new_workspace_resolution_server`] with a configurable connection budget.
/// `take` counts probe/request connections (`HerdrClient::connect` probes once
/// plus one connection per call), so a test that needs the live snapshot to
/// *succeed* must budget for it.
fn new_workspace_resolution_server_take(snapshot: Option<Value>, take: usize) -> FakeHerdr {
    testkit::herdr_server()
        .take(take)
        .on("workspace.list", |req| {
            testkit::reply(req, serde_json::json!({"workspaces": []}))
        })
        .on("workspace.create", |req| {
            testkit::reply(
                req,
                serde_json::json!({
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
                }),
            )
        })
        .on("session.snapshot", move |req| match &snapshot {
            Some(snapshot) => testkit::reply(req, serde_json::json!({"snapshot": snapshot})),
            None => testkit::error(
                req,
                "snapshot_failed",
                "created workspace snapshot unavailable",
            ),
        })
        .serve()
}

mod atomicity;
mod concurrency;
mod enqueue;
mod finalize;
mod launch_plan;
mod ownership;
mod pane_reuse;
mod registration;
mod space;
