use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use board_core::client::{is_daemon_absent, BoardClient, LocalClient, RpcClientError, UnixClient};
use board_core::protocol::{BoardChangedReason, Event};
use board_ipc::LocalListener;
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
    let listener = LocalListener::bind(&socket).unwrap();

    let (ack_release_tx, ack_release_rx) = mpsc::channel::<()>();
    let (returned_tx, returned_rx) =
        mpsc::channel::<anyhow::Result<Box<dyn Iterator<Item = Event> + Send>>>();

    // Daemon side: accept the client's handshake connection first, then the
    // dedicated subscription socket, hold the acknowledgement until the test
    // releases it, and stream one event afterwards.
    let daemon = std::thread::spawn(move || {
        let _handshake = listener.accept().unwrap();
        let mut stream = listener.accept().unwrap();
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
    let listener = LocalListener::bind(&socket).unwrap();

    let daemon = std::thread::spawn(move || {
        let _handshake = listener.accept().unwrap();
        let mut stream = listener.accept().unwrap();
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
        // The client rejects the subscription as soon as it sees the non-ack
        // first line and drops the stream, so this ack write can race the
        // disconnect (EPIPE) on a loaded machine. Best-effort is enough: the
        // contract under test is the client-side rejection.
        let _ = writeln!(stream, r#"{{"id":"sub","result":{{"subscribed":true}}}}"#);
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
        let listener = LocalListener::bind(&socket).unwrap();

        let daemon = std::thread::spawn(move || {
            let _handshake = listener.accept().unwrap();
            let mut stream = listener.accept().unwrap();
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

#[test]
fn unix_client_alias_names_the_local_client() {
    // board-cli and board-tui still spell the client `UnixClient`; the alias
    // must stay the same type as the platform-neutral `LocalClient`.
    fn same(_: fn(&std::path::Path) -> anyhow::Result<LocalClient>) {}
    same(UnixClient::connect);
}

#[test]
fn connect_to_a_missing_endpoint_reports_the_daemon_absent() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("boardd.sock");
    let error = LocalClient::connect(&socket)
        .err()
        .expect("nothing listens at a fresh path");
    assert!(
        is_daemon_absent(&error),
        "a missing endpoint must classify as absent, got: {error:?}"
    );
}

#[test]
fn connect_after_the_listener_is_dropped_reports_the_daemon_absent() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("boardd.sock");
    drop(LocalListener::bind(&socket).unwrap());
    let error = LocalClient::connect(&socket)
        .err()
        .expect("a dropped listener accepts no connections");
    assert!(
        is_daemon_absent(&error),
        "a dead endpoint must classify as absent, got: {error:?}"
    );
}

#[test]
fn non_io_and_timeout_errors_are_not_daemon_absent() {
    assert!(!is_daemon_absent(&anyhow::anyhow!("boardd error")));
    let timed_out = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::TimedOut));
    assert!(!is_daemon_absent(&timed_out));
    let not_found = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::NotFound))
        .context("connecting to boardd");
    assert!(
        is_daemon_absent(&not_found),
        "the io cause is found through added context"
    );
}

#[test]
fn call_round_trips_results_and_structured_errors_over_the_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("boardd.sock");
    let listener = LocalListener::bind(&socket).unwrap();

    let daemon = std::thread::spawn(move || {
        let mut stream = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        for _ in 0..2 {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(line.trim_end()).unwrap();
            let id = request["id"].as_str().unwrap().to_string();
            // A stray non-response line before the reply must be skipped.
            writeln!(stream, r#"{{"event":"noise"}}"#).unwrap();
            let reply = if request["method"] == "ping" {
                json!({ "id": id, "result": { "pong": true } })
            } else {
                json!({
                    "id": id,
                    "error": { "code": -32601, "kind": "not_found", "message": "no such method" }
                })
            };
            writeln!(stream, "{reply}").unwrap();
        }
    });

    let mut client = LocalClient::connect(&socket).unwrap();
    assert_eq!(
        client.call("ping", json!({})).unwrap(),
        json!({ "pong": true })
    );
    let error = client.call("nope", json!({})).unwrap_err();
    let rpc = error
        .downcast_ref::<RpcClientError>()
        .expect("structured boardd errors downcast");
    assert_eq!(rpc.code, -32601);
    assert_eq!(rpc.kind(), Some("not_found"));
    assert_eq!(client.reconnect_path(), Some(socket.clone()));
    daemon.join().unwrap();
}

#[test]
fn subscribe_times_out_when_the_daemon_never_acknowledges() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("boardd.sock");
    let listener = LocalListener::bind(&socket).unwrap();
    let (done_tx, done_rx) = mpsc::channel::<()>();

    // Accept both connections and read the subscribe request, but never
    // answer, holding the stream open until the client has given up.
    let daemon = std::thread::spawn(move || {
        let _handshake = listener.accept().unwrap();
        let stream = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        done_rx.recv().unwrap();
    });

    let mut client = LocalClient::connect(&socket).unwrap();
    let started = Instant::now();
    let error = client
        .subscribe()
        .err()
        .expect("a silent daemon must fail the subscription");
    let elapsed = started.elapsed();
    done_tx.send(()).unwrap();
    assert!(
        error.to_string().contains("timed out"),
        "a missing ack must surface as a timeout, got: {error}"
    );
    assert!(
        elapsed >= Duration::from_secs(4) && elapsed < Duration::from_secs(20),
        "the ack wait is bounded by the 5s subscribe timeout, took {elapsed:?}"
    );
    daemon.join().unwrap();
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
