//! Socket-level tests against an in-process fake herdr server on a temp unix
//! socket. Covers the request/response happy path, error mapping, mid-call
//! disconnect, and event streaming.
//!
//! Like real herdr, the fake server serves **one request per connection**:
//! `serve_calls` loops accepting connections and answers each with a single
//! reply (or closes it to simulate a disconnect). `serve_stream` hands the raw
//! stream to a closure for the persistent event-subscription case.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;

use board_herdr::{
    HerdrClient, HerdrError, HerdrEvent, HerdrEvents, ReadSource, Subscription,
    WorkspaceCreateParams,
};
use serde_json::Value;

/// What the fake server does with one request.
enum Action {
    Reply(String),
    Close,
}

fn temp_sock() -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr.sock");
    std::mem::forget(dir); // reclaimed at process exit
    path
}

/// Serve one reply per connection; `handler` maps a request to an [`Action`].
fn serve_calls<F>(handler: F) -> PathBuf
where
    F: Fn(&Value) -> Action + Send + Sync + 'static,
{
    let path = temp_sock();
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else { break };
            let mut w = stream.try_clone().unwrap();
            let mut r = BufReader::new(stream);
            let mut line = String::new();
            // A no-request probe connection yields Ok(0): just drop it.
            match r.read_line(&mut line) {
                Ok(0) | Err(_) => continue,
                Ok(_) => {}
            }
            let Ok(req) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            match handler(&req) {
                Action::Reply(s) => {
                    let _ = w.write_all(s.as_bytes());
                    let _ = w.write_all(b"\n");
                    let _ = w.flush();
                }
                Action::Close => { /* drop without replying */ }
            }
        }
    });
    path
}

/// Serve a persistent connection by handing the raw stream to `handler`.
fn serve_stream<F>(handler: F) -> PathBuf
where
    F: Fn(UnixStream) + Send + Sync + 'static,
{
    let path = temp_sock();
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else { break };
            handler(stream);
        }
    });
    path
}

fn reply_for(req: &Value, result_json: &str) -> Action {
    let id = req["id"].as_str().unwrap_or("");
    Action::Reply(format!(r#"{{"id":"{id}","result":{result_json}}}"#))
}

#[test]
fn call_happy_path_ping_and_workspace_list() {
    let path = serve_calls(|req| match req["method"].as_str().unwrap() {
        "ping" => reply_for(
            req,
            r#"{"type":"pong","version":"9.9.9","protocol":16,"capabilities":{}}"#,
        ),
        "workspace.list" => reply_for(
            req,
            r#"{"type":"workspace_list","workspaces":[{"workspace_id":"w1","label":"main","number":1,"focused":true,"active_tab_id":"w1:t1","agent_status":"idle"}]}"#,
        ),
        other => panic!("unexpected method {other}"),
    });

    let mut c = HerdrClient::connect(&path).unwrap();
    let pong = c.ping().unwrap();
    assert_eq!(pong.version, "9.9.9");

    let ws = c.workspace_list().unwrap();
    assert_eq!(ws.len(), 1);
    assert_eq!(ws[0].workspace_id, "w1");
    assert_eq!(ws[0].label, "main");
}

#[test]
fn is_live_true_on_pong() {
    let path = serve_calls(|req| reply_for(req, r#"{"type":"pong","version":"1","protocol":16}"#));
    let mut c = HerdrClient::connect(&path).unwrap();
    assert!(c.is_live());
    // Second call proves per-call reconnection works.
    assert!(c.is_live());
}

#[test]
fn typed_result_extraction_workspace_create() {
    let path = serve_calls(|req| {
        assert_eq!(req["method"], "workspace.create");
        assert_eq!(req["params"]["label"], "card-42");
        reply_for(
            req,
            r#"{"type":"workspace_created","workspace":{"workspace_id":"w7","label":"card-42","number":7,"focused":false,"active_tab_id":"w7:t1","agent_status":"unknown"},"tab":{"tab_id":"w7:t1","workspace_id":"w7","label":"tab","agent_status":"unknown"},"root_pane":{"pane_id":"w7:p1","terminal_id":"term-9","workspace_id":"w7","tab_id":"w7:t1","agent_status":"unknown"}}"#,
        )
    });

    let mut c = HerdrClient::connect(&path).unwrap();
    let p = WorkspaceCreateParams {
        label: Some("card-42".into()),
        ..Default::default()
    };
    let created = c.workspace_create(&p).unwrap();
    assert_eq!(created.workspace_id(), "w7");
    assert_eq!(created.root_pane_id(), "w7:p1");
    assert_eq!(created.root_pane.terminal_id, "term-9");
}

#[test]
fn error_response_maps_to_protocol_error() {
    let path = serve_calls(|req| {
        let id = req["id"].as_str().unwrap_or("");
        Action::Reply(format!(
            r#"{{"id":"{id}","error":{{"code":"invalid_request","message":"missing field pane_id"}}}}"#
        ))
    });

    let mut c = HerdrClient::connect(&path).unwrap();
    let err = c
        .pane_read("bogus", ReadSource::Recent, Some(50))
        .unwrap_err();
    match err {
        HerdrError::Protocol { code, message } => {
            assert_eq!(code, "invalid_request");
            assert!(message.contains("pane_id"));
        }
        other => panic!("expected Protocol, got {other:?}"),
    }
}

#[test]
fn disconnect_mid_call_maps_to_disconnected() {
    // Server reads the request, then closes without replying.
    let path = serve_calls(|_req| Action::Close);

    let mut c = HerdrClient::connect(&path).unwrap();
    let err = c.workspace_list().unwrap_err();
    assert!(matches!(err, HerdrError::Disconnected), "got {err:?}");
}

#[test]
fn event_stream_yields_events_then_ends() {
    let path = serve_stream(|stream| {
        let mut w = stream.try_clone().unwrap();
        let mut r = BufReader::new(stream);
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return; // probe connection
        }
        let req: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(req["method"], "events.subscribe");
        let ack = r#"{"id":"subscribe","result":{"type":"subscription_started"}}"#;
        let e1 = r#"{"event":"pane_agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"w1:p1","workspace_id":"w1","agent_status":"working","agent":"claude"}}"#;
        let e2 = r#"{"event":"pane_exited","data":{"type":"pane_exited","pane_id":"w1:p1","workspace_id":"w1"}}"#;
        for l in [ack, e1, e2] {
            let _ = w.write_all(l.as_bytes());
            let _ = w.write_all(b"\n");
        }
        let _ = w.flush();
        // dropping stream closes the connection => iterator ends.
    });

    let subs = vec![
        Subscription::agent_status("w1:p1"),
        Subscription::pane_exited(),
    ];
    let events = HerdrEvents::connect(&path, &subs).unwrap();
    let collected: Vec<HerdrEvent> = events.collect();
    assert_eq!(collected.len(), 2);
    assert!(matches!(
        collected[0],
        HerdrEvent::AgentStatusChanged { .. }
    ));
    assert!(matches!(collected[1], HerdrEvent::PaneExited { .. }));
}

#[test]
fn events_subscribe_error_ack_is_surfaced() {
    let path = serve_stream(|stream| {
        let mut w = stream.try_clone().unwrap();
        let mut r = BufReader::new(stream);
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let _ = w.write_all(
            br#"{"id":"subscribe","error":{"code":"internal_error","message":"bad pane"}}"#,
        );
        let _ = w.write_all(b"\n");
        let _ = w.flush();
    });

    match HerdrEvents::connect(&path, &[Subscription::agent_status("nope")]) {
        Err(HerdrError::Protocol { code, .. }) => assert_eq!(code, "internal_error"),
        Err(other) => panic!("expected Protocol, got {other:?}"),
        Ok(_) => panic!("expected error ack to fail connect"),
    }
}
