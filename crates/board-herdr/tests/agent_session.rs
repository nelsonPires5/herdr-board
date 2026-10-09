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
use serde_json::{json, Value};

/// Wire distinction under test for one named case.
struct SessionCase {
    /// Case name (kept distinct: present-id vs present-path vs absent vs null).
    name: &'static str,
    /// Full `AgentInfo` payload to decode.
    payload: Value,
    /// Expected decoded `(agent, kind, source, value)`; `None` means the
    /// field must decode as `None` (absent or explicit null).
    expected: Option<(&'static str, &'static str, &'static str, &'static str)>,
}

#[test]
fn decodes_agent_session_wire_shapes() {
    let cases = [
        SessionCase {
            name: "present-id",
            payload: json!({
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
            }),
            expected: Some(("pi", "id", "session", "p19-session")),
        },
        SessionCase {
            name: "present-path",
            payload: json!({
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
            }),
            expected: Some((
                "pi",
                "path",
                "pane",
                "/home/user/.herdr/sessions/p19-session.json",
            )),
        },
        SessionCase {
            name: "absent-none",
            payload: json!({
                "terminal_id": "term-1",
                "agent_status": "idle",
                "workspace_id": "w1",
                "tab_id": "w1:t1",
                "pane_id": "w1:p1",
                "focused": true,
                "revision": 2
            }),
            expected: None,
        },
        SessionCase {
            name: "null-none",
            payload: json!({
                "terminal_id": "term-1",
                "agent_status": "idle",
                "workspace_id": "w1",
                "tab_id": "w1:t1",
                "pane_id": "w1:p1",
                "focused": true,
                "revision": 2,
                "agent_session": null
            }),
            expected: None,
        },
    ];

    for case in cases {
        let info: AgentInfo = serde_json::from_value(case.payload).unwrap_or_else(|e| {
            panic!("case `{}` should decode: {e}", case.name);
        });
        match case.expected {
            Some((agent, kind, source, value)) => {
                let session = info
                    .agent_session
                    .unwrap_or_else(|| panic!("case `{}` should decode to Some", case.name));
                assert_eq!(session.agent, agent, "case `{}` agent", case.name);
                assert_eq!(session.kind, kind, "case `{}` kind", case.name);
                assert_eq!(session.source, source, "case `{}` source", case.name);
                assert_eq!(session.value, value, "case `{}` value", case.name);
            }
            None => {
                assert_eq!(
                    info.agent_session, None,
                    "case `{}` should decode as None",
                    case.name
                );
            }
        }
    }
}
