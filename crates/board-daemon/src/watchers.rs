//! Background watchers: the timeout/idle ticker, the LocalSpawner liveness
//! poller, and the herdr status-event thread. Each maps a completion signal
//! onto a `finalize_run` per `docs/protocol.md` §4.

use std::sync::Arc;
use std::time::{Duration, Instant};

use board_core::engine::format_duration;
use board_core::protocol::RunOutcome;
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
/// mark runs idle beyond `idle_grace_seconds` as `lost`.
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
    let now = Instant::now();
    let idle_grace = Duration::from_secs(d.config.idle_grace_seconds);

    let mut timeouts: Vec<(i64, Duration)> = Vec::new();
    let mut losts: Vec<i64> = Vec::new();
    {
        let s = d.sched.lock().unwrap();
        for (run_id, a) in &s.active {
            if let Some(dl) = a.timeout_deadline {
                if now >= dl {
                    timeouts.push((*run_id, a.started.elapsed()));
                    continue;
                }
            }
            if let Some(idle) = a.idle_since {
                if now.duration_since(idle) >= idle_grace {
                    losts.push(*run_id);
                }
            }
        }
    }

    for (run_id, elapsed) in timeouts {
        if !run_open(d, run_id) {
            continue;
        }
        let msg = format!(
            "run timed out after {}; applying on_fail",
            format_duration(Some(elapsed.as_secs() as i64))
        );
        if let Err(e) = finalize_run(
            d,
            run_id,
            RunOutcome::Fail,
            Some(msg.clone()),
            Some(msg),
            true,
            true,
        ) {
            tracing::warn!("timeout finalize run {run_id}: {e}");
        }
    }

    for run_id in losts {
        if !run_open(d, run_id) {
            continue;
        }
        let msg = "agent went idle without calling `board done`; marking run lost".to_string();
        let card_id = match finalize_run(
            d,
            run_id,
            RunOutcome::Lost,
            Some(msg.clone()),
            Some(msg),
            false,
            true,
        ) {
            Ok((_, card)) => Some(card.id),
            Err(e) => {
                tracing::warn!("lost finalize run {run_id}: {e}");
                None
            }
        };
        if let Some(cid) = card_id {
            d.notify(
                format!("Card #{cid}: agent went idle without finishing"),
                None,
                NotificationSound::Request,
            );
        }
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

/// Blocking thread: subscribes to herdr status/exit events for active panes and
/// applies status → card effects. Reconnects when the watched pane set changes
/// (checked after each received event) or on disconnect. No-op without herdr.
pub fn herdr_event_thread(d: Arc<Daemon>) {
    let Some(herdr) = d.herdr.clone() else {
        return;
    };
    let sock = herdr.socket_path().to_path_buf();

    while !d.is_shutdown() {
        let (panes, generation) = {
            let w = d.watch.lock().unwrap();
            (w.panes.clone(), w.generation)
        };
        let subs = watch_subscriptions(&panes);
        let mut events = match HerdrEvents::connect_with_retry(&sock, &subs, &Backoff::bounded(4)) {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("herdr events connect failed: {e}");
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        loop {
            // Bounded wait so shutdown and watch-set changes are honored even
            // when no events flow (a freshly spawned pane must get its
            // agent-status subscription promptly, not after the next event).
            match events.poll_event(Duration::from_millis(500)) {
                Ok(Some(ev)) => handle_event(&d, ev),
                Ok(None) => {}
                Err(e) => {
                    tracing::debug!("herdr events stream ended: {e}");
                    break;
                }
            }
            if d.is_shutdown() {
                return;
            }
            // Reconnect with the new pane set if it changed.
            if d.watch.lock().unwrap().generation != generation {
                break;
            }
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
                AgentStatus::Blocked => {
                    {
                        let mut s = d.sched.lock().unwrap();
                        if let Some(a) = s.active.get_mut(&run_id) {
                            a.idle_since = None;
                        }
                    }
                    let _ = d
                        .store
                        .lock()
                        .set_card_status(card_id, board_core::protocol::CardStatus::Blocked);
                    d.emit_changed(
                        board_core::protocol::BoardChangedReason::RunBlocked,
                        Some(card_id),
                        None,
                    );
                    d.notify(
                        format!("Card #{card_id} is blocked (needs input)"),
                        None,
                        NotificationSound::Request,
                    );
                }
                AgentStatus::Idle | AgentStatus::Done => {
                    let mut s = d.sched.lock().unwrap();
                    if let Some(a) = s.active.get_mut(&run_id) {
                        if a.idle_since.is_none() {
                            a.idle_since = Some(Instant::now());
                        }
                    }
                }
                AgentStatus::Working => {
                    {
                        let mut s = d.sched.lock().unwrap();
                        if let Some(a) = s.active.get_mut(&run_id) {
                            a.idle_since = None;
                        }
                    }
                    let _ = d
                        .store
                        .lock()
                        .set_card_status(card_id, board_core::protocol::CardStatus::Running);
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
