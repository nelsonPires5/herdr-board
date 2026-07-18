//! Serde round-trips for representative protocol messages.

use board_core::protocol::{
    BoardChangedReason, CardArchiveParams, CardCreateParams, Effort, Event,
    HarnessCapabilitiesParams, Request, Response, RpcError, RunOutcome, SpaceInfo, SpaceKind,
    SpaceListResult, Trigger,
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
fn as_str_matches_serde() {
    for t in [Trigger::Manual, Trigger::Auto] {
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            format!("\"{}\"", t.as_str())
        );
        assert_eq!(Trigger::parse_str(t.as_str()), Some(t));
    }
}
