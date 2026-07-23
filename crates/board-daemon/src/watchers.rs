//! Background watchers: the timeout/idle ticker, the LocalSpawner liveness
//! poller, and the herdr status-event thread.
//!
//! Watchers only OBSERVE: herdr pane statuses and idle expiry are translated
//! into [`AgentSignal`]s, the pure engine ([`decide_signal`]) decides the card
//! transition, and [`apply_signal`] is the single application point (DB write,
//! event, notification). Terminal finalization (`finalize_run`) is reserved
//! for pane-exit and column-timeout, per `docs/protocol.md` §4.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use board_core::engine::{decide_signal, format_duration, AgentSignal};
use board_core::protocol::{BoardChangedReason, CardStatus, RunOutcome};
use board_herdr::{
    watch_subscriptions, AgentStatus, HerdrClient, HerdrEvent, HerdrEvents, NotificationSound,
};

use crate::dispatch::{finalize_run, finalize_run_timeout};
use crate::state::Daemon;

/// Is the run still open (started, not ended) in the DB?
fn run_open(d: &Arc<Daemon>, run_id: i64) -> bool {
    match d.store.lock().get_run(run_id) {
        Ok(r) => r.started_at.is_some() && r.ended_at.is_none(),
        Err(_) => false,
    }
}

// -- timeout / idle ticker ---------------------------------------------------

/// Every `tick_ms`: kill runs past their column timeout (→ fail + on_fail) and
/// move runs idle beyond `idle_grace_seconds` to `awaiting` (the run stays
/// OPEN for human review — it is never auto-failed). Runs whose card is
/// `awaiting` are skipped entirely: the column timeout is paused and the idle
/// check no longer applies.
pub async fn timeout_ticker(d: Arc<Daemon>) {
    let mut rx = d.shutdown_rx();
    let mut iv = tokio::time::interval(Duration::from_millis(d.settings.tick_ms));
    loop {
        tokio::select! {
            _ = iv.tick() => check(&d),
            _ = rx.changed() => break,
        }
        if d.is_shutdown() {
            break;
        }
    }
}

fn check(d: &Arc<Daemon>) {
    check_at(d, Instant::now());
}

#[derive(Debug)]
struct Candidate {
    run_id: i64,
    card_id: i64,
    elapsed: Duration,
    timed_out: bool,
    observed_idle_since: Option<Instant>,
}

/// Snapshot and classify active runs. Cards already `awaiting` are skipped:
/// their run stays open and the column timeout is paused.
fn classify_candidates(d: &Arc<Daemon>, now: Instant) -> Vec<Candidate> {
    let idle_grace = Duration::from_secs(d.config.idle_grace_seconds);
    let mut candidates = Vec::new();
    let s = d.sched.lock().unwrap();
    let store = d.store.lock();
    for (run_id, a) in &s.active {
        let awaiting = store
            .get_card(a.card_id)
            .ok()
            .flatten()
            .map(|c| c.status == CardStatus::Awaiting)
            .unwrap_or(false);
        if awaiting {
            continue;
        }
        let timed_out = a.timeout_deadline.is_some_and(|dl| now >= dl);
        let observed_idle_since = (!timed_out)
            .then_some(a.idle_since)
            .flatten()
            .filter(|idle| now.saturating_duration_since(*idle) >= idle_grace);
        if timed_out || observed_idle_since.is_some() {
            candidates.push(Candidate {
                run_id: *run_id,
                card_id: a.card_id,
                elapsed: now.saturating_duration_since(a.started),
                timed_out,
                observed_idle_since,
            });
        }
    }
    candidates
}

fn apply_candidate(d: &Arc<Daemon>, c: Candidate, now: Instant) {
    if c.timed_out {
        let msg = format!(
            "run timed out after {}; applying on_fail",
            format_duration(Some(c.elapsed.as_secs() as i64))
        );
        if let Err(e) = finalize_run_timeout(
            d,
            c.run_id,
            now,
            RunOutcome::Fail,
            Some(msg.clone()),
            Some(msg),
            true,
            true,
        ) {
            tracing::warn!("timeout finalize run {}: {e}", c.run_id);
        }
    } else if let Some(observed_idle_since) = c.observed_idle_since {
        // Idle past the grace period without `board done`: the agent may have
        // finished silently. Awaiting (run open) — never a failure. Revalidate
        // the exact idle observation because a newer Working event may have
        // cleared or replaced it since classification.
        apply_idle_expired(d, c.run_id, c.card_id, observed_idle_since, now);
    }
}

/// Deterministic timeout/idle pass. Tests inject `now`; the ticker uses the
/// current monotonic instant.
fn check_at(d: &Arc<Daemon>, now: Instant) {
    for candidate in classify_candidates(d, now) {
        apply_candidate(d, candidate, now);
    }
}

// -- signal application ------------------------------------------------------

#[derive(Clone, Copy)]
enum SignalGuard {
    None,
    IdleExpired {
        observed_idle_since: Instant,
        now: Instant,
        grace: Duration,
    },
}

/// The single application point for engine signal decisions: watchers/ticker
/// emit [`AgentSignal`]s, [`decide_signal`] decides, this writes the decision
/// to the DB, maintains timeout-pause bookkeeping, and emits the board event
/// plus any notification. No-op for stale/no-op signals (engine `None`).
pub(crate) fn apply_signal(d: &Arc<Daemon>, run_id: i64, card_id: i64, signal: AgentSignal) {
    apply_signal_guarded(d, run_id, card_id, signal, SignalGuard::None);
}

fn apply_idle_expired(
    d: &Arc<Daemon>,
    run_id: i64,
    card_id: i64,
    observed_idle_since: Instant,
    now: Instant,
) {
    apply_signal_guarded(
        d,
        run_id,
        card_id,
        AgentSignal::IdleExpired,
        SignalGuard::IdleExpired {
            observed_idle_since,
            now,
            grace: Duration::from_secs(d.config.idle_grace_seconds),
        },
    );
}

fn apply_signal_guarded(
    d: &Arc<Daemon>,
    run_id: i64,
    card_id: i64,
    signal: AgentSignal,
    guard: SignalGuard,
) {
    let applied_at = Instant::now();
    let wall_now_ms = d.wall_now_ms();
    let dec = {
        // Signals and finalizers share one lock order. The exact active run and
        // its open DB row are revalidated while both locks are held, so a run
        // removed by finalization can never write the card afterward.
        let mut sched = d.sched.lock().unwrap();
        let Some(active) = sched.active.get_mut(&run_id) else {
            return;
        };
        if active.card_id != card_id {
            return;
        }
        let db = d.store.lock();
        if let SignalGuard::IdleExpired {
            observed_idle_since,
            now,
            grace,
        } = guard
        {
            if active.idle_since != Some(observed_idle_since)
                || now.saturating_duration_since(observed_idle_since) < grace
            {
                return;
            }
        }
        let run = match db.get_run(run_id) {
            Ok(run) => run,
            Err(_) => return,
        };
        if run.card_id != card_id || run.started_at.is_none() || run.ended_at.is_some() {
            return;
        }
        let card = match db.get_card(card_id) {
            Ok(Some(card)) => card,
            _ => return,
        };
        let Some(dec) = decide_signal(card.status, signal) else {
            return;
        };

        let written = match dec.awaiting_reason {
            Some(reason) if card.status != CardStatus::Awaiting => {
                db.pause_run_timeout_uow(card_id, reason, wall_now_ms)
            }
            Some(reason) => db.set_card_awaiting(card_id, reason),
            None if card.status == CardStatus::Awaiting => {
                db.resume_run_timeout_uow(card_id, dec.new_status, wall_now_ms)
            }
            None => db.set_card_status(card_id, dec.new_status),
        };
        if let Err(e) = written {
            tracing::warn!("apply signal to card {card_id}: {e}");
            return;
        }

        // Timeout-pause bookkeeping is committed under the same locks as the
        // status write. Entering awaiting disarms idle tracking; leaving it
        // shifts the deadline by exactly the review span.
        match (card.status, dec.new_status) {
            (before, CardStatus::Awaiting) if before != CardStatus::Awaiting => {
                active.idle_since = None;
                active.awaiting_since = Some(applied_at);
            }
            (CardStatus::Awaiting, after) if after != CardStatus::Awaiting => {
                if let Some(paused) = active.awaiting_since.take() {
                    if let Some(deadline) = &mut active.timeout_deadline {
                        *deadline += applied_at.saturating_duration_since(paused);
                    }
                }
            }
            _ => {}
        }
        dec
    };

    // Effects are deliberately outside both locks.
    let reason = if dec.new_status == CardStatus::Blocked {
        BoardChangedReason::RunBlocked
    } else {
        BoardChangedReason::CardUpdated
    };
    d.emit_changed(reason, Some(card_id), None);
    if let Some(msg) = dec.emit_notification {
        d.notify(
            format!("Card #{card_id}: {msg}"),
            None,
            NotificationSound::Request,
        );
    }
}

// -- LocalSpawner liveness poller -------------------------------------------

/// Every `local_poll_ms`: detect local child processes that exited without a
/// `board done` and finalize them per the pane-exit rule (fail, no transition).
pub async fn local_liveness_poller(d: Arc<Daemon>) {
    let mut rx = d.shutdown_rx();
    let mut iv = tokio::time::interval(Duration::from_millis(d.settings.local_poll_ms));
    loop {
        tokio::select! {
            _ = iv.tick() => poll_once(&d).await,
            _ = rx.changed() => break,
        }
        if d.is_shutdown() {
            break;
        }
    }
}

async fn poll_once(d: &Arc<Daemon>) {
    let candidates: Vec<(i64, board_core::spawn::SpawnHandle)> = {
        let s = d.sched.lock().unwrap();
        s.active
            .iter()
            .filter(|(_, a)| a.is_local)
            .map(|(id, a)| (*id, a.handle.clone()))
            .collect()
    };
    for (run_id, handle) in candidates {
        let spawner = d.spawner.clone();
        let alive = tokio::task::spawn_blocking(move || spawner.is_alive(&handle))
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(false);
        if alive {
            continue;
        }
        if run_open(d, run_id) {
            let msg = "pane exited without board done".to_string();
            if let Err(e) = finalize_run(
                d,
                run_id,
                RunOutcome::Fail,
                Some(msg.clone()),
                Some(msg),
                false,
                false,
            ) {
                tracing::warn!("liveness finalize run {run_id}: {e}");
            }
        } else {
            // Already finalized elsewhere; just drop our bookkeeping.
            d.sched.lock().unwrap().active.remove(&run_id);
            d.refresh_watch();
        }
    }
}

// -- herdr status-event thread ----------------------------------------------

trait WatchEventStream: Send {
    fn poll_event(&mut self, timeout: Duration) -> board_herdr::Result<Option<HerdrEvent>>;
}

impl WatchEventStream for HerdrEvents {
    fn poll_event(&mut self, timeout: Duration) -> board_herdr::Result<Option<HerdrEvent>> {
        HerdrEvents::poll_event(self, timeout)
    }
}

#[derive(Default)]
struct WatchSnapshot {
    panes: HashMap<String, AgentStatus>,
}

trait WatchConnector: Send + Sync {
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
        let panes = snapshot
            .panes
            .into_iter()
            .map(|pane| {
                let status = snapshot
                    .agents
                    .iter()
                    .find(|agent| agent.pane_id == pane.pane_id)
                    .map(|agent| agent.agent_status)
                    .unwrap_or(pane.agent_status);
                (pane.pane_id, status)
            })
            .collect();
        Ok(WatchSnapshot { panes })
    }
}

#[derive(Clone, Copy)]
struct WatchTiming {
    retry_initial: Duration,
    retry_max: Duration,
    reconcile: Duration,
    poll: Duration,
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
struct SocketWatch {
    panes: Vec<String>,
    generation: u64,
    events: Option<Box<dyn WatchEventStream>>,
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

struct HerdrSocketSupervisor {
    sockets: HashMap<PathBuf, SocketWatch>,
    connector: Arc<dyn WatchConnector>,
    timing: WatchTiming,
}

impl HerdrSocketSupervisor {
    fn new(connector: Arc<dyn WatchConnector>, timing: WatchTiming) -> Self {
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

    fn step(&mut self, d: &Arc<Daemon>, now: Instant) {
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
pub fn herdr_event_thread(d: Arc<Daemon>) {
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
fn handle_event_from_socket(d: &Arc<Daemon>, source_socket: &std::path::Path, ev: HerdrEvent) {
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
fn handle_event(d: &Arc<Daemon>, ev: HerdrEvent) {
    let source_socket = d.default_herdr_socket();
    handle_event_from_socket(d, &source_socket, ev);
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::{
        apply_candidate, apply_signal, check_at, classify_candidates, handle_event, AgentSignal,
        HerdrSocketSupervisor, WatchConnector, WatchEventStream, WatchSnapshot, WatchTiming,
    };
    use crate::dispatch::{finalize_run, finalize_run_timeout};
    use crate::settings::DaemonSettings;
    use crate::spawner::LocalSpawner;
    use crate::state::{ActiveRun, Daemon};
    use crate::store::Store;
    use board_core::config::Config;
    use board_core::db::Db;
    use board_core::protocol::{
        AwaitingReason, BoardChangedReason, CardCreateParams, CardStatus, Event, RunOutcome,
    };
    use board_core::spawn::SpawnHandle;
    use board_herdr::{AgentStatus, HerdrError, HerdrEvent};
    use tokio::sync::{broadcast, mpsc, watch};

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
            .create_run(
                card.id,
                card.column_id,
                "pi",
                "[\"pi\"]",
                "prompt",
                Some("session"),
                None,
            )
            .unwrap();
        db.start_run(run.id, Some("w1"), Some("p1")).unwrap();
        db.set_card_status(card.id, CardStatus::Running).unwrap();

        let (events_tx, events_rx) = broadcast::channel(16);
        let (dispatch_tx, _dispatch_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let d = Arc::new(Daemon::new(
            Store::new(db),
            config,
            DaemonSettings::default(),
            PathBuf::from("/tmp/board-watch.db"),
            PathBuf::from("/tmp/board-watch.sock"),
            Arc::new(LocalSpawner::new()),
            None,
            None,
            events_tx,
            dispatch_tx,
            shutdown_tx,
        ));
        d.sched.lock().unwrap().active.insert(
            run.id,
            ActiveRun {
                card_id: card.id,
                handle: SpawnHandle::default(),
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
            .create_run(
                card_a.id,
                card_a.column_id,
                "pi",
                "[\"pi\"]",
                "prompt A",
                Some("session-a"),
                None,
            )
            .unwrap();
        let run_b = db
            .create_run(
                card_b.id,
                card_b.column_id,
                "pi",
                "[\"pi\"]",
                "prompt B",
                Some("session-b"),
                None,
            )
            .unwrap();
        for (run, card, workspace) in [
            (&run_a, &card_a, "workspace-a"),
            (&run_b, &card_b, "workspace-b"),
        ] {
            db.start_run(run.id, Some(workspace), Some("shared-pane"))
                .unwrap();
            db.set_card_status(card.id, CardStatus::Running).unwrap();
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
                    handle: SpawnHandle {
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
        super::handle_event_from_socket(d, source_socket, event);
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

    #[test]
    fn working_restores_running_and_clears_idle_state() {
        let (d, run_id, card_id, mut events) = active_daemon();
        d.store
            .lock()
            .set_card_status(card_id, CardStatus::Blocked)
            .unwrap();
        d.sched
            .lock()
            .unwrap()
            .active
            .get_mut(&run_id)
            .unwrap()
            .idle_since = Some(Instant::now());

        handle_event(&d, status(AgentStatus::Working));

        assert_eq!(
            d.store.lock().get_card(card_id).unwrap().unwrap().status,
            CardStatus::Running
        );
        assert!(d
            .sched
            .lock()
            .unwrap()
            .active
            .get(&run_id)
            .unwrap()
            .idle_since
            .is_none());
        assert!(matches!(
            events.try_recv().unwrap(),
            Event::BoardChanged {
                reason: BoardChangedReason::CardUpdated,
                card_id: Some(id),
                ..
            } if id == card_id
        ));
    }

    #[test]
    fn blocked_marks_card_and_emits_change() {
        let (d, _run_id, card_id, mut events) = active_daemon();
        handle_event(&d, status(AgentStatus::Blocked));
        assert_eq!(
            d.store.lock().get_card(card_id).unwrap().unwrap().status,
            CardStatus::Blocked
        );
        assert!(matches!(
            events.try_recv().unwrap(),
            Event::BoardChanged {
                reason: BoardChangedReason::RunBlocked,
                card_id: Some(id),
                ..
            } if id == card_id
        ));
    }

    #[test]
    fn idle_arms_grace_then_becomes_awaiting_without_sleeping() {
        let (d, run_id, card_id, _events) = active_daemon();
        handle_event(&d, status(AgentStatus::Idle));
        let idle_since = d
            .sched
            .lock()
            .unwrap()
            .active
            .get(&run_id)
            .unwrap()
            .idle_since
            .unwrap();

        check_at(&d, idle_since + Duration::from_secs(4));
        assert!(d.store.lock().get_run(run_id).unwrap().ended_at.is_none());
        check_at(&d, idle_since + Duration::from_secs(5));

        // Idle past grace → awaiting, NOT lost: the run stays OPEN and the
        // card is never auto-failed.
        let db = d.store.lock();
        let run = db.get_run(run_id).unwrap();
        assert!(run.ended_at.is_none());
        assert_eq!(run.outcome, None);
        let card = db.get_card(card_id).unwrap().unwrap();
        assert_eq!(card.status, CardStatus::Awaiting);
        assert_eq!(card.awaiting_reason, Some(AwaitingReason::IdleExpired));
    }

    #[test]
    fn stale_idle_expiry_after_working_is_ignored_without_sleeping() {
        let (d, run_id, card_id, mut events) = active_daemon();
        handle_event(&d, status(AgentStatus::Idle));
        let idle_since = d
            .sched
            .lock()
            .unwrap()
            .active
            .get(&run_id)
            .unwrap()
            .idle_since
            .unwrap();
        let now = idle_since + Duration::from_secs(5);
        let candidate = classify_candidates(&d, now).pop().unwrap();

        // Working wins after the ticker classified the old idle period but
        // before that candidate is applied.
        handle_event(&d, status(AgentStatus::Working));
        while events.try_recv().is_ok() {}
        apply_candidate(&d, candidate, now);

        assert_eq!(
            d.store.lock().get_card(card_id).unwrap().unwrap().status,
            CardStatus::Running
        );
        assert!(d
            .sched
            .lock()
            .unwrap()
            .active
            .get(&run_id)
            .unwrap()
            .idle_since
            .is_none());
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn herdr_done_enters_awaiting_immediately_without_grace() {
        let (d, run_id, card_id, mut events) = active_daemon();
        handle_event(&d, status(AgentStatus::Done));

        let db = d.store.lock();
        let card = db.get_card(card_id).unwrap().unwrap();
        assert_eq!(card.status, CardStatus::Awaiting);
        assert_eq!(card.awaiting_reason, Some(AwaitingReason::AgentDone));
        assert!(db.get_run(run_id).unwrap().ended_at.is_none());
        drop(db);
        assert!(matches!(
            events.try_recv().unwrap(),
            Event::BoardChanged {
                reason: BoardChangedReason::CardUpdated,
                card_id: Some(id),
                ..
            } if id == card_id
        ));
        // Idle bookkeeping is disarmed while awaiting.
        let s = d.sched.lock().unwrap();
        let a = s.active.get(&run_id).unwrap();
        assert!(a.idle_since.is_none());
        assert!(a.awaiting_since.is_some());
    }

    #[test]
    fn working_resumes_running_from_awaiting_and_shifts_the_timeout() {
        let (d, run_id, card_id, _events) = active_daemon();
        let deadline = Instant::now() + Duration::from_secs(60);
        d.sched
            .lock()
            .unwrap()
            .active
            .get_mut(&run_id)
            .unwrap()
            .timeout_deadline = Some(deadline);

        handle_event(&d, status(AgentStatus::Done));
        assert_eq!(
            d.store.lock().get_card(card_id).unwrap().unwrap().status,
            CardStatus::Awaiting
        );
        // Simulate review time passing while awaiting.
        let paused = d
            .sched
            .lock()
            .unwrap()
            .active
            .get(&run_id)
            .unwrap()
            .awaiting_since
            .unwrap();
        d.sched
            .lock()
            .unwrap()
            .active
            .get_mut(&run_id)
            .unwrap()
            .awaiting_since = Some(paused - Duration::from_secs(30));

        handle_event(&d, status(AgentStatus::Working));

        let card = d.store.lock().get_card(card_id).unwrap().unwrap();
        assert_eq!(card.status, CardStatus::Running);
        assert_eq!(card.awaiting_reason, None);
        let a = d.sched.lock().unwrap().active.remove(&run_id).unwrap();
        assert!(a.awaiting_since.is_none());
        // The column timeout was paused: the deadline absorbed the review span.
        assert!(a.timeout_deadline.unwrap() >= deadline + Duration::from_secs(29));
    }

    #[test]
    fn ticker_skips_awaiting_runs_for_both_idle_and_timeout() {
        let (d, run_id, card_id, _events) = active_daemon();
        handle_event(&d, status(AgentStatus::Done));
        {
            let mut s = d.sched.lock().unwrap();
            let a = s.active.get_mut(&run_id).unwrap();
            a.idle_since = Some(Instant::now() - Duration::from_secs(3600));
            a.timeout_deadline = Some(Instant::now() - Duration::from_secs(60));
        }

        check_at(&d, Instant::now());

        let db = d.store.lock();
        let run = db.get_run(run_id).unwrap();
        assert!(run.ended_at.is_none());
        assert_eq!(run.outcome, None);
        let card = db.get_card(card_id).unwrap().unwrap();
        assert_eq!(card.status, CardStatus::Awaiting);
        assert_eq!(card.awaiting_reason, Some(AwaitingReason::AgentDone));
    }

    #[test]
    fn stale_signal_after_terminal_completion_is_ignored() {
        let (d, run_id, card_id, mut events) = active_daemon();
        finalize_run(&d, run_id, RunOutcome::Ok, None, None, false, true).unwrap();
        while events.try_recv().is_ok() {}

        apply_signal(&d, run_id, card_id, AgentSignal::Done);

        let db = d.store.lock();
        assert_eq!(db.get_run(run_id).unwrap().outcome, Some(RunOutcome::Ok));
        assert_eq!(
            db.get_card(card_id).unwrap().unwrap().status,
            CardStatus::Done
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn preclassified_timeout_is_rejected_after_done_enters_awaiting() {
        let (d, run_id, card_id, _events) = active_daemon();
        let now = Instant::now();
        d.sched
            .lock()
            .unwrap()
            .active
            .get_mut(&run_id)
            .unwrap()
            .timeout_deadline = Some(now - Duration::from_secs(1));
        assert!(d
            .sched
            .lock()
            .unwrap()
            .active
            .get(&run_id)
            .unwrap()
            .timeout_deadline
            .is_some_and(|deadline| now >= deadline));

        // This signal wins after timeout classification but before its claim.
        apply_signal(&d, run_id, card_id, AgentSignal::Done);
        let finalized = finalize_run_timeout(
            &d,
            run_id,
            now,
            RunOutcome::Fail,
            Some("stale timeout".into()),
            Some("stale timeout".into()),
            true,
            true,
        )
        .unwrap();
        assert!(finalized.is_none());

        let db = d.store.lock();
        assert!(db.get_run(run_id).unwrap().ended_at.is_none());
        assert_eq!(
            db.get_card(card_id).unwrap().unwrap().status,
            CardStatus::Awaiting
        );
    }

    #[test]
    fn timeout_still_finalizes_fail_when_not_awaiting() {
        let (d, run_id, card_id, _events) = active_daemon();
        let started = Instant::now() - Duration::from_secs(120);
        {
            let mut s = d.sched.lock().unwrap();
            let a = s.active.get_mut(&run_id).unwrap();
            a.started = started;
            a.timeout_deadline = Some(started + Duration::from_secs(60));
        }

        check_at(&d, Instant::now());

        let db = d.store.lock();
        assert_eq!(db.get_run(run_id).unwrap().outcome, Some(RunOutcome::Fail));
        assert_eq!(
            db.get_card(card_id).unwrap().unwrap().status,
            CardStatus::Failed
        );
    }

    #[test]
    fn stale_status_events_without_an_active_run_are_ignored() {
        let (d, _run_id, card_id, mut events) = active_daemon();
        handle_event(
            &d,
            HerdrEvent::AgentStatusChanged {
                pane_id: "ghost-pane".into(),
                workspace_id: Some("w1".into()),
                status: AgentStatus::Done,
                agent: Some("pi".into()),
            },
        );
        assert_eq!(
            d.store.lock().get_card(card_id).unwrap().unwrap().status,
            CardStatus::Running
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn pane_exit_becomes_fail_without_transition() {
        let (d, run_id, card_id, _events) = active_daemon();
        let original_column = d.store.lock().get_card(card_id).unwrap().unwrap().column_id;
        handle_event(
            &d,
            HerdrEvent::PaneExited {
                pane_id: "p1".into(),
                workspace_id: Some("w1".into()),
            },
        );
        let db = d.store.lock();
        assert_eq!(db.get_run(run_id).unwrap().outcome, Some(RunOutcome::Fail));
        let card = db.get_card(card_id).unwrap().unwrap();
        assert_eq!(card.status, CardStatus::Failed);
        assert_eq!(card.column_id, original_column);
    }

    #[test]
    fn protocol17_idle_after_done_does_not_rearm_an_awaiting_run() {
        let (d, run_id, card_id, _events) = active_daemon();

        // Protocol 17 may emit the terminal turn's `done` followed by `idle`.
        // Done is authoritative for board review; the trailing idle must not
        // arm a second grace period while this same run remains awaiting.
        handle_event(&d, status(AgentStatus::Done));
        handle_event(&d, status(AgentStatus::Idle));

        let db = d.store.lock();
        let card = db.get_card(card_id).unwrap().unwrap();
        assert_eq!(card.status, CardStatus::Awaiting);
        assert_eq!(card.awaiting_reason, Some(AwaitingReason::AgentDone));
        assert!(db.get_run(run_id).unwrap().ended_at.is_none());
        drop(db);
        assert!(
            d.sched
                .lock()
                .unwrap()
                .active
                .get(&run_id)
                .unwrap()
                .idle_since
                .is_none(),
            "a trailing protocol-17 idle event must not rearm idle expiry while awaiting",
        );
    }
}
