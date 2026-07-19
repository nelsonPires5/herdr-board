//! Column-engine transition, entry, and validation tests.

use board_core::engine::{
    decide_entry, decide_signal, decide_transition, format_duration, validate_card_archive,
    validate_card_edit, validate_card_space, validate_column_delete,
    validate_column_permission_override, AgentSignal, SignalDecision, ValidationError,
};
use board_core::model::Column;
use board_core::protocol::{AwaitingReason, CardStatus, RunOutcome, SpaceKind, Trigger};

fn col(id: i64, name: &str, trigger: Trigger, on_ok: Option<i64>, on_fail: Option<i64>) -> Column {
    Column {
        id,
        board_id: 1,
        name: name.to_string(),
        position: id,
        system_prompt: None,
        trigger,
        on_success_column_id: on_ok,
        on_fail_column_id: on_fail,
        fresh_session: false,
        harness_override: None,
        model_override: None,
        effort_override: None,
        permission_override: None,
        timeout_minutes: None,
    }
}

fn pipeline() -> Vec<Column> {
    vec![
        col(1, "Todo", Trigger::Manual, None, None),
        col(2, "Plan", Trigger::Auto, Some(3), Some(1)),
        col(3, "Execute", Trigger::Auto, Some(4), None),
        col(4, "Human Review", Trigger::Manual, None, None),
    ]
}

#[test]
fn transition_ok_into_auto_column_enqueues() {
    let cols = pipeline();
    let plan = &cols[1];
    let d = decide_transition(plan, &cols, RunOutcome::Ok, Some(252));
    assert_eq!(d.target_column_id, Some(3));
    assert_eq!(d.new_status, CardStatus::Queued);
    assert!(d.enqueue);
    assert_eq!(d.system_comment, "Plan ok in 4m12s → Execute");
}

#[test]
fn transition_ok_into_manual_column_is_idle_no_enqueue() {
    let cols = pipeline();
    let execute = &cols[2];
    let d = decide_transition(execute, &cols, RunOutcome::Ok, Some(60));
    assert_eq!(d.target_column_id, Some(4));
    assert_eq!(d.new_status, CardStatus::Idle);
    assert!(!d.enqueue);
    assert_eq!(d.system_comment, "Execute ok in 1m0s → Human Review");
}

#[test]
fn transition_fail_follows_on_fail() {
    let cols = pipeline();
    let plan = &cols[1];
    let d = decide_transition(plan, &cols, RunOutcome::Fail, Some(30));
    assert_eq!(d.target_column_id, Some(1));
    assert!(!d.enqueue); // Todo is manual
    assert_eq!(d.new_status, CardStatus::Idle);
    assert_eq!(d.system_comment, "Plan failed in 30s → Todo");
}

#[test]
fn transition_no_target_ok_is_done() {
    let cols = pipeline();
    let todo = &cols[0]; // no on_success
    let d = decide_transition(todo, &cols, RunOutcome::Ok, Some(10));
    assert_eq!(d.target_column_id, None);
    assert_eq!(d.new_status, CardStatus::Done);
    assert!(!d.enqueue);
    assert_eq!(
        d.system_comment,
        "Todo ok in 10s (no target column, staying)"
    );
}

#[test]
fn transition_no_target_fail_stays_failed() {
    let cols = pipeline();
    let execute = &cols[2]; // on_fail = None
    let d = decide_transition(execute, &cols, RunOutcome::Fail, Some(10));
    assert_eq!(d.target_column_id, None);
    assert_eq!(d.new_status, CardStatus::Failed);
    assert!(!d.enqueue);
    assert_eq!(
        d.system_comment,
        "Execute failed in 10s (no target column, staying)"
    );
}

#[test]
fn transition_cancel_never_moves() {
    let cols = pipeline();
    let plan = &cols[1];
    let d = decide_transition(plan, &cols, RunOutcome::Cancelled, Some(5));
    assert_eq!(d.target_column_id, None);
    assert_eq!(d.new_status, CardStatus::Failed);
    assert!(!d.enqueue);
}

#[test]
fn manual_entry_notifies_only_on_auto_transition() {
    let cols = pipeline();
    let human = &cols[3];
    let via_auto = decide_entry(human, CardStatus::Idle, true);
    assert_eq!(via_auto.new_status, CardStatus::Idle);
    assert!(!via_auto.enqueue);
    assert!(via_auto.notify);

    let via_human = decide_entry(human, CardStatus::Idle, false);
    assert!(!via_human.notify);
}

#[test]
fn auto_entry_enqueues_idle_failed_and_done_only() {
    let cols = pipeline();
    let plan = &cols[1];
    for status in [CardStatus::Idle, CardStatus::Failed, CardStatus::Done] {
        let d = decide_entry(plan, status, false);
        assert!(d.enqueue, "{status} should be dispatchable");
        assert_eq!(d.new_status, CardStatus::Queued);
    }
    for status in [CardStatus::Running, CardStatus::Awaiting] {
        let busy = decide_entry(plan, status, false);
        assert!(!busy.enqueue);
        assert_eq!(busy.new_status, status);
    }
}

#[test]
fn validate_delete_rules() {
    assert!(validate_column_delete(false, false, None).is_ok());
    assert!(validate_column_delete(true, false, Some(2)).is_ok());
    assert_eq!(
        validate_column_delete(true, false, None),
        Err(ValidationError::ColumnHasCards)
    );
    assert_eq!(
        validate_column_delete(true, true, Some(2)),
        Err(ValidationError::ColumnHasActiveCard)
    );
}

#[test]
fn validate_card_edit_rules() {
    assert!(validate_card_edit(CardStatus::Idle, true).is_ok());
    assert!(validate_card_edit(CardStatus::Running, false).is_ok());
    assert_eq!(
        validate_card_edit(CardStatus::Running, true),
        Err(ValidationError::CardBusy)
    );
    for status in [
        CardStatus::Queued,
        CardStatus::Blocked,
        CardStatus::Awaiting,
    ] {
        assert_eq!(
            validate_card_edit(status, true),
            Err(ValidationError::CardBusy)
        );
    }
    assert_eq!(
        ValidationError::CardBusy.to_string(),
        "card has an open run; cannot edit harness/space fields"
    );
}

#[test]
fn validate_card_archive_rules() {
    assert!(validate_card_archive(CardStatus::Idle).is_ok());
    assert!(validate_card_archive(CardStatus::Failed).is_ok());
    assert!(validate_card_archive(CardStatus::Done).is_ok());
    for status in [
        CardStatus::Queued,
        CardStatus::Running,
        CardStatus::Blocked,
        CardStatus::Awaiting,
    ] {
        assert_eq!(
            validate_card_archive(status),
            Err(ValidationError::CardHasActiveRun)
        );
    }
    assert_eq!(
        ValidationError::CardHasActiveRun.to_string(),
        "card has an open run; cancel it before archiving"
    );
}

#[test]
fn validate_bypass_override_refused() {
    assert!(validate_column_permission_override(Some("acceptEdits")).is_ok());
    assert!(validate_column_permission_override(None).is_ok());
    assert_eq!(
        validate_column_permission_override(Some("bypassPermissions")),
        Err(ValidationError::BypassNotAllowed)
    );
}

#[test]
fn validate_new_workspace_requires_ref_and_cwd() {
    // workspace kind: no ref/cwd requirement here.
    assert!(validate_card_space(SpaceKind::Workspace, None, None).is_ok());
    assert!(validate_card_space(SpaceKind::Workspace, Some("w4"), None).is_ok());

    // new_workspace needs BOTH a non-empty label and cwd.
    assert!(validate_card_space(SpaceKind::NewWorkspace, Some("feat"), Some("/repo")).is_ok());
    assert_eq!(
        validate_card_space(SpaceKind::NewWorkspace, Some("feat"), None),
        Err(ValidationError::NewWorkspaceIncomplete)
    );
    assert_eq!(
        validate_card_space(SpaceKind::NewWorkspace, None, Some("/repo")),
        Err(ValidationError::NewWorkspaceIncomplete)
    );
    assert_eq!(
        validate_card_space(SpaceKind::NewWorkspace, Some("  "), Some("/repo")),
        Err(ValidationError::NewWorkspaceIncomplete)
    );
}

#[test]
fn signal_working_resumes_running_and_clears_awaiting() {
    for from in [CardStatus::Blocked, CardStatus::Awaiting] {
        let d = decide_signal(from, AgentSignal::Working).unwrap();
        assert_eq!(d.new_status, CardStatus::Running);
        assert_eq!(d.awaiting_reason, None);
        assert_eq!(d.emit_notification, None);
    }
    // No-op on running; stale otherwise.
    assert_eq!(
        decide_signal(CardStatus::Running, AgentSignal::Working),
        None
    );
    for stale in [
        CardStatus::Idle,
        CardStatus::Queued,
        CardStatus::Failed,
        CardStatus::Done,
    ] {
        assert_eq!(decide_signal(stale, AgentSignal::Working), None);
    }
}

#[test]
fn signal_blocked_marks_blocked_and_leaves_awaiting() {
    for from in [CardStatus::Running, CardStatus::Awaiting] {
        let d = decide_signal(from, AgentSignal::Blocked).unwrap();
        assert_eq!(d.new_status, CardStatus::Blocked);
        assert_eq!(d.awaiting_reason, None);
        assert_eq!(
            d.emit_notification.as_deref(),
            Some("is blocked (needs input)")
        );
    }
    assert_eq!(
        decide_signal(CardStatus::Blocked, AgentSignal::Blocked),
        None
    );
    assert_eq!(decide_signal(CardStatus::Done, AgentSignal::Blocked), None);
}

#[test]
fn signal_done_enters_awaiting_with_agent_done_reason() {
    for from in [CardStatus::Running, CardStatus::Blocked] {
        let d = decide_signal(from, AgentSignal::Done).unwrap();
        assert_eq!(d.new_status, CardStatus::Awaiting);
        assert_eq!(d.awaiting_reason, Some(AwaitingReason::AgentDone));
        assert!(d.emit_notification.is_some());
    }
    // Already awaiting: stay, but refresh the reason (explicit done
    // supersedes idle_expired) without re-notifying.
    let d = decide_signal(CardStatus::Awaiting, AgentSignal::Done).unwrap();
    assert_eq!(d.new_status, CardStatus::Awaiting);
    assert_eq!(d.awaiting_reason, Some(AwaitingReason::AgentDone));
    assert_eq!(d.emit_notification, None);
    // Stale when no run can be active.
    assert_eq!(decide_signal(CardStatus::Done, AgentSignal::Done), None);
    assert_eq!(decide_signal(CardStatus::Idle, AgentSignal::Done), None);
}

#[test]
fn signal_idle_expired_enters_awaiting_but_keeps_existing_reason() {
    for from in [CardStatus::Running, CardStatus::Blocked] {
        let d = decide_signal(from, AgentSignal::IdleExpired).unwrap();
        assert_eq!(d.new_status, CardStatus::Awaiting);
        assert_eq!(d.awaiting_reason, Some(AwaitingReason::IdleExpired));
        assert!(d.emit_notification.is_some());
    }
    // Already awaiting: no-op, the existing (more specific) reason wins.
    assert_eq!(
        decide_signal(CardStatus::Awaiting, AgentSignal::IdleExpired),
        None
    );
    assert_eq!(
        decide_signal(CardStatus::Failed, AgentSignal::IdleExpired),
        None
    );
}

#[test]
fn signal_decision_is_appliable_shape() {
    // Entering awaiting carries a reason; every other decision clears it.
    let d = decide_signal(CardStatus::Running, AgentSignal::Done).unwrap();
    assert_eq!(
        d,
        SignalDecision {
            new_status: CardStatus::Awaiting,
            awaiting_reason: Some(AwaitingReason::AgentDone),
            emit_notification: d.emit_notification.clone(),
        }
    );
}

#[test]
fn duration_formatting() {
    assert_eq!(format_duration(None), "unknown");
    assert_eq!(format_duration(Some(0)), "0s");
    assert_eq!(format_duration(Some(42)), "42s");
    assert_eq!(format_duration(Some(252)), "4m12s");
    assert_eq!(format_duration(Some(3720)), "1h2m");
}
