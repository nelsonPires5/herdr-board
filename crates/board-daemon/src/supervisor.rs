//! Conservative, one-pass restart reconciliation.
//!
//! Resolution and runtime snapshot I/O are injectable and complete before any
//! scheduler/store mutation. Uncertain observations are never treated as pane
//! exit.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::spawner::RuntimeHandle;
use board_core::engine::AgentSignal;
use board_core::model::{Card, Run};
use board_core::protocol::RunOutcome;
use board_herdr::{AgentStatus, HerdrClient};

use crate::dispatch;
use crate::session::SessionRegistry;
use crate::state::{ActiveRun, Daemon};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTarget {
    Default(PathBuf),
    Resolved(PathBuf),
    Unresolved,
}

impl SessionTarget {
    fn socket(&self) -> Option<&Path> {
        match self {
            Self::Default(path) | Self::Resolved(path) => Some(path),
            Self::Unresolved => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFailure {
    Deadline,
    Malformed,
    Transport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSnapshot {
    panes: Vec<(String, AgentStatus)>,
}

impl RuntimeSnapshot {
    #[cfg(test)]
    fn new(panes: impl IntoIterator<Item = (impl Into<String>, AgentStatus)>) -> Self {
        Self {
            panes: panes
                .into_iter()
                .map(|(id, status)| (id.into(), status))
                .collect(),
        }
    }

    fn observe(&self, pane_id: &str) -> RuntimeProbe {
        self.panes
            .iter()
            .find(|(id, _)| id == pane_id)
            .map(|(_, status)| RuntimeProbe::Alive(*status))
            .unwrap_or(RuntimeProbe::Gone)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeProbe {
    Alive(AgentStatus),
    Gone,
    Unknown,
}

pub trait SessionResolver: Send + Sync {
    fn resolve_target(&self, session: Option<&str>) -> SessionTarget;
}

pub trait Runtime: Send + Sync {
    fn snapshot(&self, target: &SessionTarget) -> Result<RuntimeSnapshot, ProbeFailure>;
}

pub trait ReconcileClock: Send + Sync {
    fn now(&self) -> Instant;
    fn wall_now_ms(&self) -> i64;
}

#[derive(Default)]
pub struct SystemClock;

impl ReconcileClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wall_now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64
    }
}

impl SessionResolver for SessionRegistry {
    fn resolve_target(&self, session: Option<&str>) -> SessionTarget {
        match session {
            None => SessionTarget::Default(self.default_socket().to_path_buf()),
            Some(_) => self
                .resolve(session)
                .map(|resolved| SessionTarget::Resolved(resolved.socket))
                .unwrap_or(SessionTarget::Unresolved),
        }
    }
}

#[derive(Default)]
pub struct HerdrRuntime;

impl Runtime for HerdrRuntime {
    fn snapshot(&self, target: &SessionTarget) -> Result<RuntimeSnapshot, ProbeFailure> {
        let socket = target.socket().ok_or(ProbeFailure::Transport)?;
        let mut client = HerdrClient::connect(socket).map_err(classify_herdr_error)?;
        let snapshot = client.session_snapshot().map_err(classify_herdr_error)?;
        let panes: Vec<(String, AgentStatus)> =
            crate::herdr_snapshot::snapshot_pane_statuses(snapshot)
                .into_iter()
                .collect();
        Ok(RuntimeSnapshot { panes })
    }
}

fn classify_herdr_error(error: board_herdr::HerdrError) -> ProbeFailure {
    match error {
        board_herdr::HerdrError::Deadline { .. } => ProbeFailure::Deadline,
        board_herdr::HerdrError::Decode(_) => ProbeFailure::Malformed,
        _ => ProbeFailure::Transport,
    }
}

/// Reconcile the durable open-run set once. Worker failure, deadline, malformed
/// reply, unresolved session, and missing persisted pane identity all classify
/// as `Unknown`; only a valid snapshot omitting the exact pane is `Gone`.
pub async fn reconcile_once(
    d: &Arc<Daemon>,
    resolver: Arc<dyn SessionResolver>,
    runtime: Arc<dyn Runtime>,
    clock: Arc<dyn ReconcileClock>,
) {
    let active = match d.store.active_runs() {
        Ok(active) => active,
        Err(error) => {
            tracing::warn!("reconciliation: active_runs failed: {error}");
            return;
        }
    };
    for (run, card) in active {
        let target = resolver.resolve_target(run.session.as_deref());
        let pane_id = run.herdr_pane_id.clone();
        let probe = match (&target, pane_id.as_deref()) {
            (SessionTarget::Unresolved, _) | (_, None) => RuntimeProbe::Unknown,
            (_, Some(pane_id)) => {
                let runtime = runtime.clone();
                let worker_target = target.clone();
                let pane_id = pane_id.to_string();
                tokio::task::spawn_blocking(move || {
                    runtime
                        .snapshot(&worker_target)
                        .map(|snapshot| snapshot.observe(&pane_id))
                        .unwrap_or(RuntimeProbe::Unknown)
                })
                .await
                .unwrap_or(RuntimeProbe::Unknown)
            }
        };
        apply_observation(d, run, card, target, probe, clock.as_ref());
    }
}

fn same_open_run(d: &Arc<Daemon>, observed: &Run, card_id: i64) -> Option<Run> {
    let db = d.store.lock();
    let current = db.get_run(observed.id).ok()?;
    let card = db.get_card(card_id).ok()??;
    (current.started_at.is_some()
        && current.ended_at.is_none()
        && current.card_id == observed.card_id
        && card.id == current.card_id
        && current.session == observed.session
        && current.herdr_pane_id == observed.herdr_pane_id)
        .then_some(current)
}

fn apply_observation(
    d: &Arc<Daemon>,
    run: Run,
    card: Card,
    target: SessionTarget,
    probe: RuntimeProbe,
    clock: &dyn ReconcileClock,
) {
    match probe {
        RuntimeProbe::Unknown => {
            tracing::warn!("reconciliation: run {} remains unknown", run.id);
        }
        RuntimeProbe::Gone => {
            // Revalidate the exact runtime identity after external I/O. The
            // finalizer performs its own atomic open-run check as well.
            if same_open_run(d, &run, card.id).is_none() {
                return;
            }
            let message = "daemon restart: pane exited".to_string();
            let _ = dispatch::finalize_run(
                d,
                run.id,
                RunOutcome::Fail,
                Some(message.clone()),
                Some(message),
                false,
                false,
            );
        }
        RuntimeProbe::Alive(status) => adopt_alive(d, run, card, target, status, clock),
    }
}

fn adopt_alive(
    d: &Arc<Daemon>,
    run: Run,
    card: Card,
    target: SessionTarget,
    status: AgentStatus,
    clock: &dyn ReconcileClock,
) {
    let Some(current_run) = same_open_run(d, &run, card.id) else {
        return;
    };
    let adopted_at = clock.now();
    let wall_now_ms = clock.wall_now_ms();
    let deadline = ActiveRun::reconstruct_deadline(
        adopted_at,
        wall_now_ms,
        current_run.timeout_deadline_at_ms,
    );
    let socket = match target {
        SessionTarget::Default(_) => None,
        SessionTarget::Resolved(socket) => Some(socket),
        SessionTarget::Unresolved => return,
    };
    let handle = RuntimeHandle {
        pane_id: current_run.herdr_pane_id.clone(),
        workspace_id: current_run.herdr_workspace_id.clone(),
        pid: None,
        herdr_socket: socket,
    };
    {
        let mut sched = d.sched.lock().unwrap();
        if sched.active.contains_key(&run.id) {
            return;
        }
        sched.active.insert(
            run.id,
            ActiveRun {
                card_id: card.id,
                handle,
                started: adopted_at,
                timeout_deadline: deadline,
                idle_since: None,
                awaiting_since: ActiveRun::reconstruct_awaiting_since(
                    adopted_at,
                    wall_now_ms,
                    current_run.timeout_paused_at_ms,
                ),
                is_local: false,
                pane_id: current_run.herdr_pane_id.clone(),
            },
        );
    }
    d.refresh_watch();
    let signal = match status {
        AgentStatus::Done => Some(AgentSignal::Done),
        AgentStatus::Blocked => Some(AgentSignal::Blocked),
        AgentStatus::Working => Some(AgentSignal::Working),
        _ => None,
    };
    if let Some(signal) = signal {
        crate::watchers::apply_signal(d, run.id, card.id, signal);
    }
}

#[cfg(test)]
mod tests;
