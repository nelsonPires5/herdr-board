use std::time::{Duration, Instant};

use super::super::{
    apply_signal,
    herdr::handle_event,
    timeout::{apply_candidate, check_at, classify_candidates},
};
use super::{active_daemon, status};
use crate::dispatch::finalize_run_timeout;
use board_core::engine::AgentSignal;
use board_core::protocol::{AwaitingReason, CardStatus, RunOutcome};
use board_herdr::AgentStatus;

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
