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
use board_herdr::{watch_subscriptions, Backoff, HerdrEvent, HerdrEvents, NotificationSound};

use crate::dispatch::finalize_run;
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

/// Deterministic timeout/idle pass. Tests inject `now`; the ticker uses the
/// current monotonic instant.
fn check_at(d: &Arc<Daemon>, now: Instant) {
    let idle_grace = Duration::from_secs(d.config.idle_grace_seconds);

    // Snapshot the active runs, then classify. Cards already `awaiting` are
    // skipped: their run stays open and the column timeout is paused.
    struct Candidate {
        run_id: i64,
        card_id: i64,
        elapsed: Duration,
        timed_out: bool,
        idle_expired: bool,
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    {
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
            let idle_expired = !timed_out
                && a.idle_since
                    .is_some_and(|idle| now.duration_since(idle) >= idle_grace);
            if timed_out || idle_expired {
                candidates.push(Candidate {
                    run_id: *run_id,
                    card_id: a.card_id,
                    elapsed: now.saturating_duration_since(a.started),
                    timed_out,
                    idle_expired,
                });
            }
        }
    }

    for c in candidates {
        if !run_open(d, c.run_id) {
            continue;
        }
        if c.timed_out {
            let msg = format!(
                "run timed out after {}; applying on_fail",
                format_duration(Some(c.elapsed.as_secs() as i64))
            );
            if let Err(e) = finalize_run(
                d,
                c.run_id,
                RunOutcome::Fail,
                Some(msg.clone()),
                Some(msg),
                true,
                true,
            ) {
                tracing::warn!("timeout finalize run {}: {e}", c.run_id);
            }
        } else if c.idle_expired {
            // Idle past the grace period without `board done`: the agent may
            // have finished silently. Awaiting (run open) — never a failure.
            apply_signal(d, c.run_id, c.card_id, AgentSignal::IdleExpired);
        }
    }
}

// -- signal application ------------------------------------------------------

/// The single application point for engine signal decisions: watchers/ticker
/// emit [`AgentSignal`]s, [`decide_signal`] decides, this writes the decision
/// to the DB, maintains timeout-pause bookkeeping, and emits the board event
/// plus any notification. No-op for stale/no-op signals (engine `None`).
fn apply_signal(d: &Arc<Daemon>, run_id: i64, card_id: i64, signal: AgentSignal) {
    let card = match d.store.lock().get_card(card_id) {
        Ok(Some(c)) => c,
        _ => return,
    };
    let Some(dec) = decide_signal(card.status, signal) else {
        return;
    };

    let written = match dec.awaiting_reason {
        Some(reason) => d.store.lock().set_card_awaiting(card_id, reason),
        None => d.store.lock().set_card_status(card_id, dec.new_status),
    };
    if let Err(e) = written {
        tracing::warn!("apply signal to card {card_id}: {e}");
        return;
    }

    // Timeout-pause bookkeeping: entering `awaiting` disarms idle tracking and
    // stamps the pause start; leaving it shifts the column deadline forward by
    // the awaiting span so review time never counts against the timeout.
    {
        let mut s = d.sched.lock().unwrap();
        if let Some(a) = s.active.get_mut(&run_id) {
            match (card.status, dec.new_status) {
                (before, CardStatus::Awaiting) if before != CardStatus::Awaiting => {
                    a.idle_since = None;
                    a.awaiting_since = Some(Instant::now());
                }
                (CardStatus::Awaiting, after) if after != CardStatus::Awaiting => {
                    if let Some(paused) = a.awaiting_since.take() {
                        if let Some(dl) = &mut a.timeout_deadline {
                            *dl += paused.elapsed();
                        }
                    }
                }
                _ => {}
            }
        }
    }

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
    if dec.new_status == CardStatus::Blocked {
        d.notify(
            format!("Card #{card_id} is blocked (needs input)"),
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

/// Blocking thread multiplexing one event stream **per session socket** that
/// has active panes (the fix for the multi-session bug: `agent.start`'s
/// `pane.agent_status_changed` subscription is validated per socket, so each
/// session needs its own stream). No-op without herdr.
///
/// Design (deliberately simple, no per-session thread lifecycle): a single
/// thread holds a `socket → HerdrEvents` map. Each pass it reads the watch set
/// (`panes_by_socket` + generation). On a generation change it drops every
/// connection and rebuilds (subscriptions must reflect the new pane sets);
/// between changes it (re)connects any watched socket missing a live stream
/// (covers first-connect and reconnect-after-disconnect) and polls each stream
/// with a short deadline so shutdown and watch changes are honored promptly.
pub fn herdr_event_thread(d: Arc<Daemon>) {
    if d.herdr.is_none() {
        return;
    }
    let mut conns: HashMap<PathBuf, HerdrEvents> = HashMap::new();
    let mut current_gen: Option<u64> = None;

    while !d.is_shutdown() {
        let (panes_by_socket, generation) = {
            let w = d.watch.lock().unwrap();
            (w.panes_by_socket.clone(), w.generation)
        };

        // Watch set changed → drop all streams so they resubscribe fresh.
        if current_gen != Some(generation) {
            current_gen = Some(generation);
            conns.clear();
        }
        // Forget streams for sockets no longer watched.
        conns.retain(|sock, _| panes_by_socket.contains_key(sock));
        // (Re)connect any watched socket missing a live stream.
        for (sock, panes) in &panes_by_socket {
            if conns.contains_key(sock) {
                continue;
            }
            let subs = watch_subscriptions(panes);
            match HerdrEvents::connect_with_retry(sock, &subs, &Backoff::bounded(4)) {
                Ok(ev) => {
                    conns.insert(sock.clone(), ev);
                }
                Err(e) => tracing::debug!("herdr events connect {sock:?} failed: {e}"),
            }
        }

        if conns.is_empty() {
            // Nothing to watch yet; wait for the next spawn.
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }

        // Poll each stream once with a short deadline.
        let mut broken: Vec<PathBuf> = Vec::new();
        for (sock, ev) in conns.iter_mut() {
            match ev.poll_event(Duration::from_millis(200)) {
                Ok(Some(event)) => handle_event(&d, event),
                Ok(None) => {}
                Err(e) => {
                    tracing::debug!("herdr events {sock:?} ended: {e}");
                    broken.push(sock.clone());
                }
            }
            if d.is_shutdown() {
                return;
            }
        }
        for sock in broken {
            conns.remove(&sock); // reconnected on a later pass if still watched
        }
    }
}

fn find_run_by_pane(d: &Arc<Daemon>, pane_id: &str) -> Option<i64> {
    let s = d.sched.lock().unwrap();
    s.active
        .iter()
        .find(|(_, a)| a.pane_id.as_deref() == Some(pane_id))
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
fn handle_event(d: &Arc<Daemon>, ev: HerdrEvent) {
    use board_herdr::AgentStatus;
    match ev {
        HerdrEvent::AgentStatusChanged {
            pane_id, status, ..
        } => {
            let Some(run_id) = find_run_by_pane(d, &pane_id) else {
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
                AgentStatus::Idle => {
                    let mut s = d.sched.lock().unwrap();
                    if let Some(a) = s.active.get_mut(&run_id) {
                        if a.idle_since.is_none() {
                            a.idle_since = Some(Instant::now());
                        }
                    }
                }
                AgentStatus::Unknown => {}
            }
        }
        HerdrEvent::PaneExited { pane_id, .. } => {
            if let Some(run_id) = find_run_by_pane(d, &pane_id) {
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{check_at, handle_event};
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
    use board_herdr::{AgentStatus, HerdrEvent};
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
}
