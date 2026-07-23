use std::time::{Duration, Instant};

use super::super::herdr::handle_event;
use super::{active_daemon, status};
use board_core::protocol::{AwaitingReason, BoardChangedReason, CardStatus, Event};
use board_herdr::AgentStatus;

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
