//! Background watchers: the timeout/idle ticker, the LocalSpawner liveness
//! poller, and the herdr status-event thread. Each maps a completion signal
//! onto a `finalize_run` per `docs/protocol.md` §4.

use std::collections::HashMap;
use std::path::PathBuf;
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
    check_at(d, Instant::now());
}

/// Deterministic timeout/idle pass. Tests inject `now`; the ticker uses the
/// current monotonic instant.
fn check_at(d: &Arc<Daemon>, now: Instant) {
    let idle_grace = Duration::from_secs(d.config.idle_grace_seconds);

    let mut timeouts: Vec<(i64, Duration)> = Vec::new();
    let mut losts: Vec<i64> = Vec::new();
    {
        let s = d.sched.lock().unwrap();
        for (run_id, a) in &s.active {
            if let Some(dl) = a.timeout_deadline {
                if now >= dl {
                    timeouts.push((*run_id, now.saturating_duration_since(a.started)));
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
                    if d.store
                        .lock()
                        .set_card_status(card_id, board_core::protocol::CardStatus::Running)
                        .is_ok()
                    {
                        d.emit_changed(
                            board_core::protocol::BoardChangedReason::CardUpdated,
                            Some(card_id),
                            None,
                        );
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
        BoardChangedReason, CardCreateParams, CardStatus, Event, RunOutcome,
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
    fn idle_arms_grace_then_becomes_lost_without_sleeping() {
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

        assert_eq!(
            d.store.lock().get_run(run_id).unwrap().outcome,
            Some(RunOutcome::Lost)
        );
        assert_eq!(
            d.store.lock().get_card(card_id).unwrap().unwrap().status,
            CardStatus::Failed
        );
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
