//! Public-API coverage for the protocol-19 `agent_session` DTO on
//! [`board_herdr::AgentInfo`].
//!
//! Shapes verified against `tests/fixtures/schema.json` (Herdr 0.8.0,
//! protocol 19): `success_response/$defs/AgentInfo.properties.agent_session`
//! is `AgentSessionInfo | null`, where `AgentSessionInfo` is the
//! `{agent, kind, source, value}` object with `kind` ∈ {`id`, `path`}.
//! `agent_session` is not in `AgentInfo`'s `required` list, so a pane without
//! a reported agent session omits the field entirely.

use board_herdr::AgentInfo;
use serde_json::json;

/// Minimal protocol-19 `AgentInfo` (schema `required` list) plus the extra
/// `agent`/`name` fields herdr sends, with `agent_session` present
/// (`kind: "id"`).
#[test]
fn decodes_agent_session_present_with_id_kind() {
    let info: AgentInfo = serde_json::from_value(json!({
        "terminal_id": "term-2",
        "agent_status": "working",
        "workspace_id": "w1",
        "tab_id": "w1:t1",
        "pane_id": "w1:p2",
        "focused": false,
        "revision": 3,
        "agent": "pi",
        "name": "card-42-execute",
        "agent_session": {
            "agent": "pi",
            "kind": "id",
            "source": "session",
            "value": "p19-session"
        }
    }))
    .unwrap();

    let session = info.agent_session.expect("agent_session should decode");
    assert_eq!(session.agent, "pi");
    assert_eq!(session.kind, "id");
    assert_eq!(session.source, "session");
    assert_eq!(session.value, "p19-session");
}

/// The same DTO with `kind: "path"` (a session reference by path).
#[test]
fn decodes_agent_session_present_with_path_kind() {
    let info: AgentInfo = serde_json::from_value(json!({
        "terminal_id": "term-2",
        "agent_status": "done",
        "workspace_id": "w1",
        "tab_id": "w1:t1",
        "pane_id": "w1:p2",
        "focused": false,
        "revision": 4,
        "agent_session": {
            "agent": "pi",
            "kind": "path",
            "source": "pane",
            "value": "/home/user/.herdr/sessions/p19-session.json"
        }
    }))
    .unwrap();

    let session = info.agent_session.expect("agent_session should decode");
    assert_eq!(session.agent, "pi");
    assert_eq!(session.kind, "path");
    assert_eq!(session.source, "pane");
    assert_eq!(session.value, "/home/user/.herdr/sessions/p19-session.json");
}

/// `agent_session` omitted (the protocol-19 default for panes without a
/// reported session) decodes as `None`.
#[test]
fn decodes_agent_session_absent_as_none() {
    let info: AgentInfo = serde_json::from_value(json!({
        "terminal_id": "term-1",
        "agent_status": "idle",
        "workspace_id": "w1",
        "tab_id": "w1:t1",
        "pane_id": "w1:p1",
        "focused": true,
        "revision": 2
    }))
    .unwrap();

    assert_eq!(info.agent_session, None);
}

/// An explicit `null` (allowed by the schema's `AgentSessionInfo | null`
/// anyOf) also decodes as `None`.
#[test]
fn decodes_agent_session_null_as_none() {
    let info: AgentInfo = serde_json::from_value(json!({
        "terminal_id": "term-1",
        "agent_status": "idle",
        "workspace_id": "w1",
        "tab_id": "w1:t1",
        "pane_id": "w1:p1",
        "focused": true,
        "revision": 2,
        "agent_session": null
    }))
    .unwrap();

    assert_eq!(info.agent_session, None);
}
