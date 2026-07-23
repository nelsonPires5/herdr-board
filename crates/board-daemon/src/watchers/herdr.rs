//! Herdr socket supervision, snapshots, and event routing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use board_core::engine::AgentSignal;
use board_core::protocol::{CardStatus, RunOutcome};
use board_herdr::{watch_subscriptions, AgentStatus, HerdrClient, HerdrEvent, HerdrEvents};

use super::{apply_signal, run_open};
use crate::dispatch::finalize_run;
use crate::state::Daemon;

// -- herdr status-event thread ----------------------------------------------

pub(super) trait WatchEventStream: Send {
    fn poll_event(&mut self, timeout: Duration) -> board_herdr::Result<Option<HerdrEvent>>;
}

impl WatchEventStream for HerdrEvents {
    fn poll_event(&mut self, timeout: Duration) -> board_herdr::Result<Option<HerdrEvent>> {
        HerdrEvents::poll_event(self, timeout)
    }
}

#[derive(Default)]
pub(super) struct WatchSnapshot {
    pub(super) panes: HashMap<String, AgentStatus>,
}

pub(super) trait WatchConnector: Send + Sync {
    fn subscribe(
        &self,
        socket: &std::path::Path,
        panes: &[String],
    ) -> board_herdr::Result<Box<dyn WatchEventStream>>;
    fn snapshot(&self, socket: &std::path::Path) -> board_herdr::Result<WatchSnapshot>;
}

struct HerdrWatchConnector;

impl WatchConnector for HerdrWatchConnector {
    fn subscribe(
        &self,
        socket: &std::path::Path,
        panes: &[String],
    ) -> board_herdr::Result<Box<dyn WatchEventStream>> {
        HerdrEvents::connect(socket, &watch_subscriptions(panes))
            .map(|events| Box::new(events) as Box<dyn WatchEventStream>)
    }

    fn snapshot(&self, socket: &std::path::Path) -> board_herdr::Result<WatchSnapshot> {
        let mut client = HerdrClient::connect(socket)?;
        let snapshot = client.session_snapshot()?;
        let panes = crate::herdr_snapshot::snapshot_pane_statuses(snapshot);
        Ok(WatchSnapshot { panes })
    }
}

#[derive(Clone, Copy)]
pub(super) struct WatchTiming {
    pub(super) retry_initial: Duration,
    pub(super) retry_max: Duration,
    pub(super) reconcile: Duration,
    pub(super) poll: Duration,
}

impl Default for WatchTiming {
    fn default() -> Self {
        Self {
            retry_initial: Duration::from_millis(200),
            retry_max: Duration::from_secs(5),
            reconcile: Duration::from_secs(30),
            poll: Duration::from_millis(100),
        }
    }
}

/// State for one socket. Generation, retry, and snapshot schedules are local:
/// changing or losing one session never resets a healthy session's stream.
pub(super) struct SocketWatch {
    pub(super) panes: Vec<String>,
    pub(super) generation: u64,
    pub(super) events: Option<Box<dyn WatchEventStream>>,
    retry_at: Instant,
    retry_delay: Duration,
    reconcile_at: Instant,
}

impl SocketWatch {
    fn new(panes: Vec<String>, generation: u64, now: Instant, timing: WatchTiming) -> Self {
        Self {
            panes,
            generation,
            events: None,
            retry_at: now,
            retry_delay: timing.retry_initial,
            reconcile_at: now,
        }
    }

    fn disconnected(&mut self, now: Instant, timing: WatchTiming) {
        self.events = None;
        self.retry_at = now + self.retry_delay;
        self.retry_delay = (self.retry_delay * 2).min(timing.retry_max);
    }
}

pub(super) struct HerdrSocketSupervisor {
    pub(super) sockets: HashMap<PathBuf, SocketWatch>,
    connector: Arc<dyn WatchConnector>,
    timing: WatchTiming,
}

impl HerdrSocketSupervisor {
    pub(super) fn new(connector: Arc<dyn WatchConnector>, timing: WatchTiming) -> Self {
        Self {
            sockets: HashMap::new(),
            connector,
            timing,
        }
    }

    fn sync_watches(&mut self, wanted: HashMap<PathBuf, Vec<String>>, now: Instant) {
        self.sockets.retain(|socket, _| wanted.contains_key(socket));
        for (socket, panes) in wanted {
            match self.sockets.get_mut(&socket) {
                Some(state) if state.panes != panes => {
                    let generation = state.generation.saturating_add(1);
                    *state = SocketWatch::new(panes, generation, now, self.timing);
                }
                Some(_) => {}
                None => {
                    self.sockets
                        .insert(socket, SocketWatch::new(panes, 1, now, self.timing));
                }
            }
        }
    }

    pub(super) fn step(&mut self, d: &Arc<Daemon>, now: Instant) {
        let wanted = d.watch.lock().unwrap().panes_by_socket.clone();
        self.sync_watches(wanted, now);
        for (socket, state) in &mut self.sockets {
            if d.is_shutdown() {
                return;
            }
            if state.events.is_none() && now >= state.retry_at {
                match self.connector.subscribe(socket, &state.panes) {
                    Ok(events) => {
                        state.events = Some(events);
                        state.retry_delay = self.timing.retry_initial;
                        // The subscription ack has completed before snapshot I/O.
                        reconcile_socket(d, socket, &state.panes, self.connector.as_ref());
                        state.reconcile_at = now + self.timing.reconcile;
                    }
                    Err(error) => {
                        tracing::debug!(?socket, "herdr subscribe failed: {error}");
                        state.disconnected(now, self.timing);
                    }
                }
            }
            if state.events.is_some() && now >= state.reconcile_at {
                reconcile_socket(d, socket, &state.panes, self.connector.as_ref());
                state.reconcile_at = now + self.timing.reconcile;
            }
            let polled = state
                .events
                .as_mut()
                .map(|events| events.poll_event(self.timing.poll));
            match polled {
                Some(Ok(Some(event))) => handle_event_from_socket(d, socket, event),
                Some(Ok(None)) | None => {}
                Some(Err(error)) => {
                    tracing::debug!(?socket, "herdr event stream ended: {error}");
                    state.disconnected(now, self.timing);
                }
            }
        }
    }
}

/// Always-on, per-socket Herdr supervisor. A successful generation always does
/// subscribe → snapshot → poll. Periodic snapshots close event-loss gaps, and
/// uncertain snapshots leave durable runs untouched.
pub(super) fn herdr_event_thread(d: Arc<Daemon>) {
    let mut supervisor =
        HerdrSocketSupervisor::new(Arc::new(HerdrWatchConnector), WatchTiming::default());
    while !d.is_shutdown() {
        supervisor.step(&d, Instant::now());
        if d.is_shutdown() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn reconcile_socket(
    d: &Arc<Daemon>,
    socket: &std::path::Path,
    watched: &[String],
    connector: &dyn WatchConnector,
) {
    // Any transport/decode/deadline failure is Unknown, never Gone.
    let Ok(snapshot) = connector.snapshot(socket) else {
        return;
    };
    for pane_id in watched {
        let event = match snapshot.panes.get(pane_id) {
            Some(status) => HerdrEvent::AgentStatusChanged {
                pane_id: pane_id.clone(),
                workspace_id: None,
                status: *status,
                agent: None,
            },
            None => HerdrEvent::PaneExited {
                pane_id: pane_id.clone(),
                workspace_id: None,
            },
        };
        handle_event_from_socket(d, socket, event);
    }
}

/// Resolve the socket a run belongs to. `None` is the daemon's default
/// session, just as it is for the spawn handle and the watch set.
fn effective_herdr_socket(d: &Arc<Daemon>, socket: Option<&std::path::Path>) -> PathBuf {
    socket
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| d.default_herdr_socket())
}

/// Find an active run only when both Herdr identity components match. Pane ids
/// are scoped to a session socket; matching on the pane alone can route an
/// event from one session to another when two sessions reuse the same id.
fn find_run_by_pane(
    d: &Arc<Daemon>,
    pane_id: &str,
    source_socket: &std::path::Path,
) -> Option<i64> {
    let source_socket = effective_herdr_socket(d, Some(source_socket));
    let s = d.sched.lock().unwrap();
    s.active
        .iter()
        .find(|(_, a)| {
            a.pane_id.as_deref() == Some(pane_id)
                && effective_herdr_socket(d, a.handle.herdr_socket.as_deref()) == source_socket
        })
        .map(|(id, _)| *id)
}

fn card_of(d: &Arc<Daemon>, run_id: i64) -> Option<i64> {
    d.sched
        .lock()
        .unwrap()
        .active
        .get(&run_id)
        .map(|a| a.card_id)
}

fn clear_idle(d: &Arc<Daemon>, run_id: i64) {
    let mut s = d.sched.lock().unwrap();
    if let Some(a) = s.active.get_mut(&run_id) {
        a.idle_since = None;
    }
}

/// Map one herdr event onto an [`AgentSignal`] (or idle arming) for its run.
/// Events without a matching active run are stale and ignored; the engine
/// additionally no-ops signals that don't apply to the card's live status.
pub(super) fn handle_event_from_socket(
    d: &Arc<Daemon>,
    source_socket: &std::path::Path,
    ev: HerdrEvent,
) {
    use board_herdr::AgentStatus;
    match ev {
        HerdrEvent::AgentStatusChanged {
            pane_id, status, ..
        } => {
            let Some(run_id) = find_run_by_pane(d, &pane_id, source_socket) else {
                return;
            };
            let Some(card_id) = card_of(d, run_id) else {
                return;
            };
            match status {
                AgentStatus::Working => {
                    clear_idle(d, run_id);
                    apply_signal(d, run_id, card_id, AgentSignal::Working);
                }
                AgentStatus::Blocked => {
                    clear_idle(d, run_id);
                    apply_signal(d, run_id, card_id, AgentSignal::Blocked);
                }
                // herdr `done` while the run is open (no `board done`): the
                // agent claims completion — card goes `awaiting` immediately,
                // no grace period.
                AgentStatus::Done => {
                    clear_idle(d, run_id);
                    apply_signal(d, run_id, card_id, AgentSignal::Done);
                }
                // `idle` only arms the grace timer; expiry is the ticker's job.
                // A trailing idle after `done` must not rearm an awaiting run.
                AgentStatus::Idle => {
                    let mut s = d.sched.lock().unwrap();
                    let store = d.store.lock();
                    if let Some(a) = s.active.get_mut(&run_id) {
                        let awaiting = store
                            .get_card(a.card_id)
                            .ok()
                            .flatten()
                            .is_some_and(|card| card.status == CardStatus::Awaiting);
                        if !awaiting && a.idle_since.is_none() {
                            a.idle_since = Some(Instant::now());
                        }
                    }
                }
                AgentStatus::Unknown => {}
            }
        }
        HerdrEvent::PaneExited { pane_id, .. } => {
            if let Some(run_id) = find_run_by_pane(d, &pane_id, source_socket) {
                if run_open(d, run_id) {
                    let msg = "pane exited without board done".to_string();
                    let _ = finalize_run(
                        d,
                        run_id,
                        RunOutcome::Fail,
                        Some(msg.clone()),
                        Some(msg),
                        false,
                        false,
                    );
                }
            }
        }
        HerdrEvent::Other(_) => {}
    }
}

// Unit tests that construct events directly use the default session. The live
// event thread always calls `handle_event_from_socket`, retaining its stream's
// source socket.
#[cfg(test)]
pub(super) fn handle_event(d: &Arc<Daemon>, ev: HerdrEvent) {
    let source_socket = d.default_herdr_socket();
    handle_event_from_socket(d, &source_socket, ev);
}
