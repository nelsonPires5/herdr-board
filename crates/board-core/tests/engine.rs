//! Column-engine transition, entry, and validation tests.

use board_core::engine::{
    decide_entry, decide_transition, format_duration, validate_card_edit, validate_card_space,
    validate_column_delete, validate_column_permission_override, ValidationError,
};
use board_core::model::Column;
use board_core::protocol::{CardStatus, RunOutcome, SpaceKind, Trigger};

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
fn transition_no_target_ok_stays_idle() {
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
fn auto_entry_enqueues_idle_and_failed_only() {
    let cols = pipeline();
    let plan = &cols[1];
    for status in [CardStatus::Idle, CardStatus::Failed] {
        let d = decide_entry(plan, status, false);
        assert!(d.enqueue);
        assert_eq!(d.new_status, CardStatus::Queued);
    }
    let busy = decide_entry(plan, CardStatus::Running, false);
    assert!(!busy.enqueue);
    assert_eq!(busy.new_status, CardStatus::Running);
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
    assert_eq!(
        validate_card_edit(CardStatus::Queued, true),
        Err(ValidationError::CardBusy)
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
fn duration_formatting() {
    assert_eq!(format_duration(None), "unknown");
    assert_eq!(format_duration(Some(0)), "0s");
    assert_eq!(format_duration(Some(42)), "42s");
    assert_eq!(format_duration(Some(252)), "4m12s");
    assert_eq!(format_duration(Some(3720)), "1h2m");
}
