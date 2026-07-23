use std::collections::HashMap;

use board_core::client::BoardClient;
use board_core::protocol::Event;
use serde_json::{json, Value};

struct RecordingClient {
    calls: Vec<(String, Value)>,
    responses: HashMap<String, Value>,
}

impl RecordingClient {
    fn new() -> RecordingClient {
        RecordingClient {
            calls: Vec::new(),
            responses: HashMap::from([
                (
                    "harness.capabilities".into(),
                    json!({
                        "harness": "pi",
                        "models": [],
                        "model_freeform": true,
                        "default_efforts": [],
                        "permission_modes": []
                    }),
                ),
                (
                    "harness.list".into(),
                    json!({ "harnesses": ["claude", "pi"] }),
                ),
                (
                    "space.list".into(),
                    json!({
                        "spaces": [{ "id": "w1", "label": "Workspace" }]
                    }),
                ),
                (
                    "session.list".into(),
                    json!({
                        "sessions": [{
                            "name": "default",
                            "default": true,
                            "running": true
                        }]
                    }),
                ),
                ("run.cancel".into(), action_result()),
                ("run.retry".into(), action_result()),
            ]),
        }
    }
}

impl BoardClient for RecordingClient {
    fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.calls.push((method.to_string(), params));
        self.responses
            .get(method)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unexpected method {method}"))
    }

    fn subscribe(&mut self) -> anyhow::Result<Box<dyn Iterator<Item = Event> + Send>> {
        Ok(Box::new(std::iter::empty()))
    }
}

#[test]
fn typed_catalog_and_run_methods_preserve_wire_v1_params_and_results() {
    let mut client = RecordingClient::new();

    let capabilities = client.harness_capabilities("pi").unwrap();
    assert_eq!(capabilities.harness, "pi");
    let harnesses = client.harness_list().unwrap();
    assert_eq!(harnesses.harnesses, ["claude", "pi"]);
    let default_spaces = client.space_list(None).unwrap();
    assert_eq!(default_spaces.spaces[0].id, "w1");
    let named_spaces = client.space_list(Some("feature")).unwrap();
    assert_eq!(named_spaces.spaces[0].label, "Workspace");
    let sessions = client.session_list().unwrap();
    assert_eq!(sessions.sessions[0].name, "default");

    let cancelled = client.run_cancel(42).unwrap();
    assert_eq!(cancelled.run.id, 7);
    assert_eq!(cancelled.card.id, 42);
    let retried = client.run_retry(42).unwrap();
    assert_eq!(retried.run.id, 7);
    assert_eq!(retried.card.id, 42);

    assert_eq!(
        client.calls,
        vec![
            ("harness.capabilities".into(), json!({ "harness": "pi" })),
            ("harness.list".into(), json!({})),
            ("space.list".into(), json!({})),
            ("space.list".into(), json!({ "session": "feature" })),
            ("session.list".into(), json!({})),
            ("run.cancel".into(), json!({ "card_id": 42 })),
            ("run.retry".into(), json!({ "card_id": 42 })),
        ]
    );
}

fn action_result() -> Value {
    json!({
        "run": {
            "id": 7,
            "card_id": 42,
            "column_id": 3,
            "harness": "pi",
            "argv_json": "[]",
            "prompt_snapshot": "",
            "herdr_workspace_id": null,
            "herdr_pane_id": null,
            "session_id": null,
            "session": null,
            "started_at": null,
            "ended_at": null,
            "outcome": null,
            "result_summary": null,
            "log_path": null
        },
        "card": {
            "id": 42,
            "board_id": 1,
            "column_id": 3,
            "position": 0,
            "title": "Card",
            "description": "",
            "harness": "pi",
            "model": null,
            "effort": null,
            "permission_mode": null,
            "session": null,
            "space_kind": "workspace",
            "space_ref": null,
            "space_cwd": null,
            "status": "running",
            "awaiting_reason": null,
            "session_id": null,
            "created_at": "",
            "updated_at": "",
            "archived_at": null
        }
    })
}
