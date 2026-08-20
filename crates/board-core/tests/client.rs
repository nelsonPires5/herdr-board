use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::sync::mpsc;

use board_core::client::{BoardClient, UnixClient};
use board_core::protocol::{BoardChangedReason, Event};
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

#[test]
fn subscribe_waits_for_the_daemon_acknowledgement_before_returning() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("boardd.sock");
    let listener = UnixListener::bind(&socket).unwrap();

    let (ack_release_tx, ack_release_rx) = mpsc::channel::<()>();
    let (returned_tx, returned_rx) =
        mpsc::channel::<anyhow::Result<Box<dyn Iterator<Item = Event> + Send>>>();

    // Daemon side: accept the client's handshake connection first, then the
    // dedicated subscription socket, hold the acknowledgement until the test
    // releases it, and stream one event afterwards.
    let daemon = std::thread::spawn(move || {
        let (_handshake, _) = listener.accept().unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(
            line.contains("events.subscribe"),
            "expected the subscribe request, got: {line}"
        );
        ack_release_rx.recv().unwrap();
        writeln!(stream, r#"{{"id":"sub","result":{{"subscribed":true}}}}"#).unwrap();
        writeln!(
            stream,
            "{}",
            serde_json::to_string(&Event::BoardChanged {
                reason: BoardChangedReason::CardUpdated,
                board_id: Some(2),
                card_id: Some(7),
                column_id: Some(3),
            })
            .unwrap()
        )
        .unwrap();
    });

    let client = UnixClient::connect(&socket).unwrap();
    let subscriber = std::thread::spawn(move || {
        let mut client = client;
        returned_tx.send(client.subscribe()).unwrap();
    });

    // The subscription must not be observable before the daemon acknowledges
    // it: the reconnect path refetches the snapshot as soon as subscribe
    // returns, so an early return would open a missed-mutation window.
    assert!(
        returned_rx.try_recv().is_err(),
        "subscribe returned before the daemon acknowledged the subscription"
    );

    ack_release_tx.send(()).unwrap();
    let mut events = returned_rx
        .recv()
        .expect("subscribe must return after the ack")
        .expect("subscribe must succeed after the ack");
    assert_eq!(
        events.next(),
        Some(Event::BoardChanged {
            reason: BoardChangedReason::CardUpdated,
            board_id: Some(2),
            card_id: Some(7),
            column_id: Some(3),
        }),
        "events written after the ack must be delivered"
    );

    daemon.join().unwrap();
    subscriber.join().unwrap();
}

#[test]
fn subscribe_rejects_an_event_streamed_before_the_ack() {
    // A daemon that streams an event before acknowledging must not satisfy the
    // "wait for the ack" contract: a naive implementation that returns on ANY
    // first line would accept this fixture, so this case pins the difference
    // between "waits for an ack" and "waits for any line".
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("boardd.sock");
    let listener = UnixListener::bind(&socket).unwrap();

    let daemon = std::thread::spawn(move || {
        let (_handshake, _) = listener.accept().unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        writeln!(
            stream,
            "{}",
            serde_json::to_string(&Event::BoardChanged {
                reason: BoardChangedReason::CardUpdated,
                board_id: Some(2),
                card_id: Some(7),
                column_id: Some(3),
            })
            .unwrap()
        )
        .unwrap();
        writeln!(stream, r#"{{"id":"sub","result":{{"subscribed":true}}}}"#).unwrap();
    });

    let mut client = UnixClient::connect(&socket).unwrap();
    let error = client
        .subscribe()
        .err()
        .expect("an event before the ack must reject the subscription");
    assert!(
        error
            .to_string()
            .contains("invalid subscription acknowledgement"),
        "an event before the ack must be rejected as a non-ack, got: {error}"
    );
    daemon.join().unwrap();
}

#[test]
fn subscribe_rejects_a_negative_or_error_ack() {
    for ack_line in [
        r#"{"id":"sub","result":{"subscribed":false}}"#,
        r#"{"id":"sub","error":{"code":1,"message":"subscription refused"}}"#,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("boardd.sock");
        let listener = UnixListener::bind(&socket).unwrap();

        let daemon = std::thread::spawn(move || {
            let (_handshake, _) = listener.accept().unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            writeln!(stream, "{ack_line}").unwrap();
        });

        let mut client = UnixClient::connect(&socket).unwrap();
        let error = client
            .subscribe()
            .err()
            .expect("a negative/error ack must reject the subscription");
        assert!(
            error
                .to_string()
                .contains("did not acknowledge the subscription"),
            "a negative/error ack must reject the subscription, got: {error}"
        );
        daemon.join().unwrap();
    }
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
