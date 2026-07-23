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
    let candidates: Vec<(i64, crate::spawner::RuntimeHandle)> = {
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
mod tests;
