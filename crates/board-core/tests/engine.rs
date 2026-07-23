//! Column-engine transition, entry, and validation tests.

use board_core::config::{Config, HarnessDef};
use board_core::engine::{
    decide_auto_hop, decide_entry, decide_lifecycle, decide_resumability, decide_signal,
    decide_transition, format_duration, validate_card_archive, validate_card_edit,
    validate_card_space, validate_column_delete, validate_column_permission_override, AgentSignal,
    AutoHopDecision, FinalizePlan, LifecycleAction, LifecycleDecision, LifecycleFacts,
    LifecycleHarness, LifecycleRejection, ResumabilityDecision, SignalDecision, ValidationError,
};
use board_core::engine::{
    merge_card_update, merge_column_update, validate_card_settings, validate_column_settings,
    validate_column_update, validate_effective_settings, PermissionContext,
};
use board_core::model::{Card, Column, Comment, Run};
use board_core::protocol::{
    AwaitingReason, CardStatus, CardUpdateParams, ColumnUpdateParams, Effort, Patch, RunOutcome,
    SpaceKind, Trigger,
};

fn card() -> Card {
    Card {
        id: 7,
        board_id: 1,
        column_id: 1,
        position: 0,
        title: "card".into(),
        description: "description".into(),
        harness: "claude".into(),
        model: Some("sonnet".into()),
        effort: Some(Effort::High),
        permission_mode: Some("manual".into()),
        session: Some("session".into()),
        space_kind: SpaceKind::NewWorkspace,
        space_ref: Some("feature".into()),
        space_cwd: Some("/repo".into()),
        status: CardStatus::Idle,
        awaiting_reason: None,
        session_id: None,
        created_at: "now".into(),
        updated_at: "now".into(),
        archived_at: None,
    }
}

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
fn merged_card_update_is_validated_before_any_field_is_applied() {
    let current = card();
    let patch = CardUpdateParams {
        id: current.id,
        space_kind: Some(SpaceKind::NewWorkspace),
        space_ref: Patch::Clear,
        space_cwd: Patch::Clear,
        ..Default::default()
    };
    let merged = merge_card_update(&current, &patch);
    assert_eq!(merged.space_kind, SpaceKind::NewWorkspace);
    assert_eq!(merged.space_ref, None);
    assert_eq!(merged.space_cwd, None);
    assert!(validate_card_settings(&merged, &Config::default()).is_err());
    assert_eq!(current.space_ref.as_deref(), Some("feature"));
    assert_eq!(current.space_cwd.as_deref(), Some("/repo"));
}

#[test]
fn merged_validation_rejects_every_incompatible_card_setting() {
    let config = Config::default();
    let mut value = card();
    value.harness = "missing".into();
    assert!(validate_card_settings(&value, &config).is_err());
    value = card();
    value.effort = Some(Effort::Off);
    assert!(validate_card_settings(&value, &config).is_err());
    value = card();
    value.permission_mode = Some("not-a-mode".into());
    assert!(validate_card_settings(&value, &config).is_err());
    value = card();
    value.harness = "pi".into();
    assert!(validate_card_settings(&value, &config).is_err());
    value = card();
    value.permission_mode = Some("bypassPermissions".into());
    assert!(validate_card_settings(&value, &config).is_ok());
}

#[test]
fn column_override_validation_rejects_bypass_and_orphaned_dependents() {
    let config = Config::default();
    let mut column = col(2, "execute", Trigger::Auto, None, None);
    column.harness_override = Some("claude".into());
    column.permission_override = Some("bypassPermissions".into());
    assert!(validate_column_settings(&column, &config, PermissionContext::ColumnOverride).is_err());

    column.harness_override = None;
    column.effort_override = Some("not-an-effort".into());
    assert!(validate_column_settings(&column, &config, PermissionContext::ColumnOverride).is_err());

    let patch = ColumnUpdateParams {
        id: column.id,
        harness_override: Patch::Clear,
        ..Default::default()
    };
    let merged = merge_column_update(&column, &patch);
    assert!(validate_column_update(&column, &merged, &patch, &config).is_err());
}

#[test]
fn effective_card_and_column_are_revalidated_at_enqueue_boundary() {
    let mut cfg = Config::default();
    cfg.harness.insert(
        "fake".into(),
        HarnessDef {
            argv: vec!["fake".into()],
            efforts: vec!["low".into()],
            permission_modes: vec!["auto".into()],
            ..Default::default()
        },
    );
    let base_card = card();
    let mut column = col(2, "execute", Trigger::Auto, None, None);
    column.harness_override = Some("fake".into());
    column.effort_override = Some("high".into());
    assert!(validate_effective_settings(&base_card, &column, &cfg).is_err());

    // A fully overridden legacy base is judged by the settings that will run,
    // while a legacy invalid value with no override is still rejected.
    let mut legacy = card();
    legacy.harness = "missing".into();
    legacy.model = Some("old-model".into());
    legacy.effort = Some(Effort::High);
    legacy.permission_mode = Some("old-permission".into());
    column.harness_override = Some("fake".into());
    column.model_override = Some("new-model".into());
    column.effort_override = Some("low".into());
    column.permission_override = Some("auto".into());
    assert!(validate_effective_settings(&legacy, &column, &cfg).is_ok());
    column.harness_override = None;
    assert!(validate_effective_settings(&legacy, &column, &cfg).is_err());
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

fn lifecycle_facts(
    started: bool,
    harness: LifecycleHarness,
    supplied_run_id: Option<i64>,
) -> LifecycleFacts {
    LifecycleFacts {
        open_run_id: Some(42),
        supplied_run_id,
        started,
        harness,
        card_status: if started {
            CardStatus::Running
        } else {
            CardStatus::Queued
        },
    }
}

#[test]
fn lifecycle_rejects_stale_and_missing_supplied_run_ids() {
    let stale = LifecycleFacts {
        open_run_id: Some(42),
        supplied_run_id: Some(41),
        started: true,
        harness: LifecycleHarness::Configured,
        card_status: CardStatus::Running,
    };
    assert_eq!(
        decide_lifecycle(
            &stale,
            LifecycleAction::Done {
                outcome: RunOutcome::Ok
            }
        ),
        LifecycleDecision::Reject(LifecycleRejection::SuppliedRunIdMismatch {
            expected: 42,
            supplied: 41,
        })
    );

    let queued_without_id = lifecycle_facts(false, LifecycleHarness::Configured, None);
    assert_eq!(
        decide_lifecycle(
            &queued_without_id,
            LifecycleAction::Done {
                outcome: RunOutcome::Ok
            }
        ),
        LifecycleDecision::Reject(LifecycleRejection::QueuedCompletionRequiresRunId)
    );
}

#[test]
fn lifecycle_allows_only_exact_queued_configured_completion() {
    let configured = lifecycle_facts(false, LifecycleHarness::Configured, Some(42));
    assert_eq!(
        decide_lifecycle(
            &configured,
            LifecycleAction::Done {
                outcome: RunOutcome::Ok
            }
        ),
        LifecycleDecision::Finalize(FinalizePlan {
            outcome: RunOutcome::Ok,
            kill: false,
            transition: true,
        })
    );

    let builtin = lifecycle_facts(false, LifecycleHarness::BuiltIn, Some(42));
    assert_eq!(
        decide_lifecycle(
            &builtin,
            LifecycleAction::Done {
                outcome: RunOutcome::Ok
            }
        ),
        LifecycleDecision::Reject(LifecycleRejection::QueuedBuiltinCompletion)
    );
}

#[test]
fn lifecycle_plans_cancel_timeout_and_pane_exit_without_herdr_types() {
    let started = lifecycle_facts(true, LifecycleHarness::Configured, Some(42));
    assert_eq!(
        decide_lifecycle(&started, LifecycleAction::Cancel),
        LifecycleDecision::Finalize(FinalizePlan {
            outcome: RunOutcome::Cancelled,
            kill: true,
            transition: false,
        })
    );
    assert_eq!(
        decide_lifecycle(&started, LifecycleAction::Timeout),
        LifecycleDecision::Finalize(FinalizePlan {
            outcome: RunOutcome::Fail,
            kill: true,
            transition: true,
        })
    );
    assert_eq!(
        decide_lifecycle(&started, LifecycleAction::PaneExited),
        LifecycleDecision::Finalize(FinalizePlan {
            outcome: RunOutcome::Fail,
            kill: false,
            transition: false,
        })
    );
    let awaiting = LifecycleFacts {
        card_status: CardStatus::Awaiting,
        ..started
    };
    assert_eq!(
        decide_lifecycle(&awaiting, LifecycleAction::Timeout),
        LifecycleDecision::Reject(LifecycleRejection::TimeoutPaused)
    );

    let builtin = lifecycle_facts(true, LifecycleHarness::BuiltIn, Some(42));
    assert_eq!(
        decide_lifecycle(&builtin, LifecycleAction::PaneExited),
        LifecycleDecision::Reject(LifecycleRejection::PaneExitBuiltin)
    );
}

#[test]
fn lifecycle_rejects_timeout_before_start_and_no_open_run() {
    let queued = lifecycle_facts(false, LifecycleHarness::Configured, Some(42));
    assert_eq!(
        decide_lifecycle(&queued, LifecycleAction::Timeout),
        LifecycleDecision::Reject(LifecycleRejection::TimeoutBeforeStart)
    );
    let missing = LifecycleFacts {
        open_run_id: None,
        supplied_run_id: None,
        started: false,
        harness: LifecycleHarness::Configured,
        card_status: CardStatus::Queued,
    };
    assert_eq!(
        decide_lifecycle(&missing, LifecycleAction::Cancel),
        LifecycleDecision::Reject(LifecycleRejection::NoOpenRun)
    );
}

#[test]
fn auto_hop_policy_preserves_target_and_stops_at_limit() {
    let cols = pipeline();
    let transition = decide_transition(&cols[1], &cols, RunOutcome::Ok, Some(1));
    assert_eq!(
        decide_auto_hop(7, &transition),
        AutoHopDecision::Continue { hop: 8 }
    );
    assert_eq!(
        decide_auto_hop(8, &transition),
        AutoHopDecision::Stop {
            message: "auto-chain limit (8) reached without human action; stopping".into()
        }
    );

    let terminal = decide_transition(&cols[0], &cols, RunOutcome::Ok, Some(1));
    assert_eq!(decide_auto_hop(8, &terminal), AutoHopDecision::Reset);
}

fn run_with_session(id: i64, started: bool, session_id: Option<&str>) -> Run {
    Run {
        id,
        card_id: 7,
        column_id: 1,
        harness: "pi".into(),
        argv_json: "[]".into(),
        prompt_snapshot: "prompt".into(),
        system_prompt_snapshot: None,
        launch_spec: None,
        herdr_workspace_id: None,
        herdr_pane_id: None,
        session_id: session_id.map(str::to_owned),
        session: None,
        started_at: started.then(|| "started".into()),
        timeout_deadline_at_ms: None,
        timeout_paused_at_ms: None,
        ended_at: None,
        outcome: None,
        result_summary: None,
        log_path: None,
    }
}

fn comment(author: &str) -> Comment {
    Comment {
        id: 1,
        card_id: 7,
        author: author.into(),
        body: "finished".into(),
        created_at: "now".into(),
    }
}

#[test]
fn resumability_requires_started_run_and_matching_agent_comment() {
    let run = run_with_session(9, true, Some("session-1"));
    assert_eq!(
        decide_resumability(
            Some("session-1"),
            std::slice::from_ref(&run),
            &[comment("agent:9")],
        ),
        ResumabilityDecision::Resumable
    );
    for (runs, comments) in [
        (
            vec![run_with_session(9, false, Some("session-1"))],
            vec![comment("agent:9")],
        ),
        (
            vec![run_with_session(9, true, Some("other"))],
            vec![comment("agent:9")],
        ),
        (vec![run], vec![comment("agent:8")]),
    ] {
        assert_eq!(
            decide_resumability(Some("session-1"), &runs, &comments),
            ResumabilityDecision::Fresh
        );
    }
    assert_eq!(
        decide_resumability(None, &[], &[]),
        ResumabilityDecision::Fresh
    );
}
