//! Serde round-trips for representative protocol messages.

use board_core::model::Run;
use board_core::protocol::{
    AwaitingReason, BoardChangedReason, BoardGetParams, BoardListResult, BoardOpenParams,
    CardArchiveParams, CardCreateParams, CardListParams, CardStatus, CardUpdateParams,
    ColumnCreateParams, ColumnUpdateParams, Effort, Event, HarnessCapabilitiesParams, Patch,
    Request, Response, RpcError, RunDoneParams, RunFocusParams, RunFocusResult, RunOutcome,
    RunPaneExitedParams, SpaceInfo, SpaceKind, SpaceListResult, TemplateApplyParams, Trigger,
};
use serde_json::json;

fn roundtrip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let s = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&s).unwrap();
    assert_eq!(&back, value);
}

#[test]
fn run_system_prompt_snapshot_serde_compatibility_and_privacy() {
    let legacy: Run = serde_json::from_value(json!({
        "id": 1,
        "card_id": 2,
        "column_id": 3,
        "harness": "pi",
        "argv_json": "[]",
        "prompt_snapshot": "task",
        "herdr_workspace_id": null,
        "herdr_pane_id": null,
        "session_id": null,
        "session": null,
        "started_at": null,
        "ended_at": null,
        "outcome": null,
        "result_summary": null,
        "log_path": null
    }))
    .unwrap();
    assert_eq!(legacy.system_prompt_snapshot, None);

    let secret = "system instructions\nprivate line";
    let run = Run {
        system_prompt_snapshot: Some(secret.into()),
        ..legacy
    };
    let serialized = serde_json::to_string(&run).unwrap();
    assert!(!serialized.contains("system_prompt_snapshot"));
    assert!(!serialized.contains(secret));
}

#[test]
fn request_with_and_without_params() {
    let with = Request {
        id: "1".into(),
        method: "card.get".into(),
        params: json!({"id": 3}),
    };
    roundtrip(&with);

    // Omitted params default to Null.
    let r: Request = serde_json::from_str(r#"{"id":"2","method":"board.get"}"#).unwrap();
    assert_eq!(r.params, serde_json::Value::Null);
}

#[test]
fn run_pane_exited_params_serialize_exact_internal_wire_shape() {
    let params = RunPaneExitedParams {
        card_id: 42,
        run_id: 7,
    };
    assert_eq!(
        serde_json::to_string(&params).unwrap(),
        r#"{"card_id":42,"run_id":7}"#
    );
    roundtrip(&params);
}

#[test]
fn run_done_params_run_id_is_optional_and_serializes_when_present() {
    let missing: RunDoneParams = serde_json::from_value(json!({
        "card_id": 42,
        "outcome": "ok"
    }))
    .unwrap();
    assert_eq!(missing.run_id, None);
    assert_eq!(
        serde_json::to_value(&missing).unwrap(),
        json!({"card_id": 42, "outcome": "ok"})
    );

    let provided = RunDoneParams {
        card_id: 42,
        outcome: RunOutcome::Ok,
        summary: None,
        run_id: Some(7),
    };
    assert_eq!(
        serde_json::to_value(&provided).unwrap(),
        json!({"card_id": 42, "outcome": "ok", "run_id": 7})
    );
    roundtrip(&provided);
}

#[test]
fn response_ok_and_error_shapes() {
    let ok = Response::ok("1", json!({"deleted": true}));
    let s = serde_json::to_string(&ok).unwrap();
    assert!(s.contains("\"result\""));
    assert!(!s.contains("\"error\""));
    roundtrip(&ok);

    let err = Response::err("1", 3, "invalid state");
    let s = serde_json::to_string(&err).unwrap();
    assert!(s.contains("\"error\""));
    assert!(!s.contains("\"result\""));
    assert_eq!(
        err.error,
        Some(RpcError {
            code: 3,
            message: "invalid state".into()
        })
    );
    roundtrip(&err);
}

#[test]
fn event_tagging() {
    let ev = Event::BoardChanged {
        reason: BoardChangedReason::CardMoved,
        card_id: Some(42),
        column_id: None,
    };
    let s = serde_json::to_string(&ev).unwrap();
    assert_eq!(
        s,
        r#"{"event":"board_changed","reason":"card_moved","card_id":42}"#
    );
    roundtrip(&ev);

    let re = Event::RunEnded {
        card_id: 42,
        run_id: 7,
        outcome: RunOutcome::Ok,
    };
    let s = serde_json::to_string(&re).unwrap();
    assert!(s.contains(r#""event":"run_ended""#));
    roundtrip(&re);
}

#[test]
fn enums_serialize_lowercase() {
    assert_eq!(serde_json::to_string(&Trigger::Auto).unwrap(), "\"auto\"");
    assert_eq!(
        serde_json::to_string(&SpaceKind::NewWorkspace).unwrap(),
        "\"new_workspace\""
    );
    for effort in [Effort::Off, Effort::Minimal, Effort::Xhigh] {
        let wire = serde_json::to_string(&effort).unwrap();
        let decoded: Effort = serde_json::from_str(&wire).unwrap();
        assert_eq!(decoded, effort);
        assert_eq!(Effort::parse_str(effort.as_str()), Some(effort));
    }
    assert_eq!(
        serde_json::to_string(&RunOutcome::Cancelled).unwrap(),
        "\"cancelled\""
    );
}

#[test]
fn nullable_update_patches_distinguish_omitted_null_and_value() {
    macro_rules! column_field {
        ($field:ident, $wire:expr, $value:expr) => {{
            let omitted: ColumnUpdateParams = serde_json::from_value(json!({"id": 1})).unwrap();
            assert!(matches!(omitted.$field, Patch::Unchanged));
            assert_eq!(serde_json::to_value(&omitted).unwrap(), json!({"id": 1}));

            let cleared: ColumnUpdateParams =
                serde_json::from_value(json!({"id": 1, stringify!($field): null})).unwrap();
            assert!(matches!(cleared.$field, Patch::Clear));
            assert_eq!(
                serde_json::to_value(&cleared).unwrap(),
                json!({"id": 1, stringify!($field): null})
            );

            let set: ColumnUpdateParams =
                serde_json::from_value(json!({"id": 1, stringify!($field): $wire})).unwrap();
            assert!(matches!(set.$field, Patch::Set(v) if v == $value));
        }};
    }
    column_field!(system_prompt, "instructions", "instructions".to_string());
    column_field!(on_success_column_id, 2, 2_i64);
    column_field!(on_fail_column_id, 3, 3_i64);
    column_field!(harness_override, "pi", "pi".to_string());
    column_field!(model_override, "model", "model".to_string());
    column_field!(effort_override, "high", "high".to_string());
    column_field!(permission_override, "manual", "manual".to_string());
    column_field!(timeout_minutes, 15, 15_i64);

    macro_rules! card_field {
        ($field:ident, $wire:expr, $value:expr) => {{
            let omitted: CardUpdateParams = serde_json::from_value(json!({"id": 1})).unwrap();
            assert!(matches!(omitted.$field, Patch::Unchanged));
            assert_eq!(serde_json::to_value(&omitted).unwrap(), json!({"id": 1}));

            let cleared: CardUpdateParams =
                serde_json::from_value(json!({"id": 1, stringify!($field): null})).unwrap();
            assert!(matches!(cleared.$field, Patch::Clear));
            assert_eq!(
                serde_json::to_value(&cleared).unwrap(),
                json!({"id": 1, stringify!($field): null})
            );

            let set: CardUpdateParams =
                serde_json::from_value(json!({"id": 1, stringify!($field): $wire})).unwrap();
            assert!(matches!(set.$field, Patch::Set(v) if v == $value));
        }};
    }
    card_field!(model, "model", "model".to_string());
    card_field!(effort, "high", Effort::High);
    card_field!(permission_mode, "manual", "manual".to_string());
    card_field!(session, "session", "session".to_string());
    card_field!(space_ref, "workspace", "workspace".to_string());
    card_field!(space_cwd, "/repo", "/repo".to_string());
}

#[test]
fn patch_default_and_is_unchanged_are_explicit() {
    assert!(Patch::<String>::default().is_unchanged());
    assert!(!Patch::<String>::Clear.is_unchanged());
    assert!(!Patch::Set("x".to_string()).is_unchanged());
}

#[test]
fn card_create_params_omit_none() {
    let p = CardCreateParams {
        title: "t".into(),
        ..Default::default()
    };
    let s = serde_json::to_string(&p).unwrap();
    assert_eq!(s, r#"{"title":"t"}"#);
}

#[test]
fn card_archive_params_roundtrip() {
    let p = CardArchiveParams {
        id: 42,
        archived: true,
    };
    roundtrip(&p);
    assert_eq!(
        serde_json::to_string(&p).unwrap(),
        r#"{"id":42,"archived":true}"#
    );
}

#[test]
fn harness_and_space_methods() {
    let p = HarnessCapabilitiesParams {
        harness: "claude".into(),
    };
    roundtrip(&p);
    assert_eq!(
        serde_json::to_string(&p).unwrap(),
        r#"{"harness":"claude"}"#
    );

    let spaces = SpaceListResult {
        spaces: vec![
            SpaceInfo {
                id: "w1".into(),
                label: "main".into(),
            },
            SpaceInfo {
                id: "w2".into(),
                label: "docs".into(),
            },
        ],
    };
    roundtrip(&spaces);
    assert_eq!(
        serde_json::to_string(&spaces).unwrap(),
        r#"{"spaces":[{"id":"w1","label":"main"},{"id":"w2","label":"docs"}]}"#
    );
}

#[test]
fn scoped_board_and_run_focus_types_roundtrip_with_legacy_defaults() {
    roundtrip(&BoardOpenParams {
        scope_path: "/repo".into(),
    });
    let get: BoardGetParams = serde_json::from_value(json!({})).unwrap();
    assert_eq!(get.board_id, None);
    let list = BoardListResult { boards: vec![] };
    roundtrip(&list);

    let column: ColumnCreateParams = serde_json::from_value(json!({"name":"Todo"})).unwrap();
    assert_eq!(column.board_id, None);
    let card: CardCreateParams = serde_json::from_value(json!({"title":"T"})).unwrap();
    assert_eq!(card.board_id, None);
    let cards: CardListParams = serde_json::from_value(json!({})).unwrap();
    assert_eq!(cards.board_id, None);
    let template: TemplateApplyParams = serde_json::from_value(json!({"name":"pipeline"})).unwrap();
    assert_eq!(template.board_id, None);

    roundtrip(&RunFocusParams {
        card_id: 7,
        origin_socket: "/tmp/herdr.sock".into(),
    });
    roundtrip(&RunFocusResult {
        run_id: 9,
        pane_id: "p1".into(),
    });
}

#[test]
fn as_str_matches_serde() {
    for t in [Trigger::Manual, Trigger::Auto] {
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            format!("\"{}\"", t.as_str())
        );
        assert_eq!(Trigger::parse_str(t.as_str()), Some(t));
    }
}

#[test]
fn card_status_new_variants_wire_strings() {
    assert_eq!(
        serde_json::to_string(&CardStatus::Awaiting).unwrap(),
        "\"awaiting\""
    );
    assert_eq!(
        serde_json::to_string(&CardStatus::Done).unwrap(),
        "\"done\""
    );
    roundtrip(&CardStatus::Awaiting);
    roundtrip(&CardStatus::Done);
    assert_eq!(
        CardStatus::parse_str("awaiting"),
        Some(CardStatus::Awaiting)
    );
    assert_eq!(CardStatus::parse_str("done"), Some(CardStatus::Done));
    assert_eq!(CardStatus::Awaiting.as_str(), "awaiting");
    assert_eq!(CardStatus::Done.as_str(), "done");
}

#[test]
fn awaiting_reason_snake_case_wire_strings() {
    assert_eq!(
        serde_json::to_string(&AwaitingReason::AgentDone).unwrap(),
        "\"agent_done\""
    );
    assert_eq!(
        serde_json::to_string(&AwaitingReason::IdleExpired).unwrap(),
        "\"idle_expired\""
    );
    roundtrip(&AwaitingReason::AgentDone);
    roundtrip(&AwaitingReason::IdleExpired);
    assert_eq!(
        AwaitingReason::parse_str("agent_done"),
        Some(AwaitingReason::AgentDone)
    );
    assert_eq!(
        AwaitingReason::parse_str("idle_expired"),
        Some(AwaitingReason::IdleExpired)
    );
    assert_eq!(AwaitingReason::parse_str("bogus"), None);
}
