use super::super::{apply_signal, herdr::handle_event};
use super::{active_daemon, status};
use crate::dispatch::finalize_run;
use board_core::engine::AgentSignal;
use board_core::protocol::{AwaitingReason, CardStatus, RunOutcome};
use board_herdr::{AgentStatus, HerdrEvent};

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
