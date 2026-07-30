//! Regression coverage for metadata-only, exactly-once Herdr diagnostics.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use board_herdr::{HerdrClient, HerdrError, HerdrEvents, SocketDeadlines, Subscription};
use serde_json::Value;

const METHOD_SENTINEL: &str = "HERDR_METHOD_SECRET_/tmp/watcher.sock";

#[derive(Clone, Default)]
struct TraceBuffer(Arc<Mutex<Vec<u8>>>);

struct TraceWriter(Arc<Mutex<Vec<u8>>>);

impl Write for TraceWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TraceBuffer {
    type Writer = TraceWriter;

    fn make_writer(&'a self) -> Self::Writer {
        TraceWriter(self.0.clone())
    }
}

impl TraceBuffer {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

fn listener() -> (tempfile::TempDir, PathBuf, UnixListener) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&path).unwrap();
    (dir, path, listener)
}

fn deadlines() -> SocketDeadlines {
    SocketDeadlines {
        connect: Duration::from_millis(100),
        read: Duration::from_millis(100),
        write: Duration::from_millis(100),
        handshake: Duration::from_millis(100),
        request: Duration::from_millis(100),
        method_grace: Duration::from_millis(20),
    }
}

#[test]
fn public_calls_and_all_subscription_failures_complete_once_without_secrets() {
    let captured = TraceBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(captured.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let (_call_dir, call_path, call_listener) = listener();
    let call_server = thread::spawn(move || {
        // HerdrClient::connect performs one reachability probe.
        drop(call_listener.accept().unwrap().0);
        for expected_method in [METHOD_SENTINEL, "ping"] {
            let (stream, _) = call_listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(request["method"], expected_method);
            let result = if expected_method == "ping" {
                // A valid envelope with the wrong typed result shape.
                serde_json::json!("not-a-pong")
            } else {
                serde_json::json!({"wire_method_preserved": true})
            };
            writeln!(
                writer,
                "{}",
                serde_json::json!({"id": request["id"], "result": result})
            )
            .unwrap();
        }
    });

    let mut client = HerdrClient::connect_with_deadlines(&call_path, deadlines()).unwrap();
    assert!(client
        .call(
            METHOD_SENTINEL,
            serde_json::json!({"secret": "PAYLOAD_SECRET"})
        )
        .is_ok());
    assert!(matches!(client.ping(), Err(HerdrError::Decode(_))));
    call_server.join().unwrap();

    let (_stream_dir, stream_path, stream_listener) = listener();
    let stream_server = thread::spawn(move || {
        let (stream, _) = stream_listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let request: Value = serde_json::from_str(line.trim()).unwrap();
        writeln!(
            writer,
            "{}",
            serde_json::json!({"id": request["id"], "result": {"type": "subscription_started"}})
        )
        .unwrap();

        line.clear();
        reader.read_line(&mut line).unwrap();
        let request: Value = serde_json::from_str(line.trim()).unwrap();
        writeln!(
            writer,
            "{}",
            serde_json::json!({"id": request["id"], "error": {"code": "internal_error", "message": "SUBSCRIBE_SECRET"}})
        )
        .unwrap();
    });
    let mut events = HerdrEvents::connect_with_deadlines(
        &stream_path,
        &[Subscription::pane_exited()],
        deadlines(),
    )
    .unwrap();
    assert!(matches!(
        events.add_subscriptions(&[Subscription::pane_closed()]),
        Err(HerdrError::Protocol { .. })
    ));
    stream_server.join().unwrap();

    let missing_dir = tempfile::tempdir().unwrap();
    let missing_path = missing_dir.path().join("missing.sock");
    assert!(HerdrEvents::connect_with_deadlines(
        &missing_path,
        &[Subscription::pane_exited()],
        deadlines(),
    )
    .is_err());

    let text = captured.text();
    let rpc: Vec<_> = text
        .lines()
        .filter(|line| line.contains("Herdr RPC completed"))
        .collect();
    assert_eq!(
        rpc.len(),
        2,
        "RPC completions were not exactly once: {text}"
    );
    assert!(rpc
        .iter()
        .any(|line| { line.contains("method=\"<unknown>\"") && line.contains("outcome=\"ok\"") }));
    assert!(rpc.iter().any(|line| {
        line.contains("method=\"ping\"")
            && line.contains("outcome=\"error\"")
            && line.contains("error_category=\"decode\"")
    }));
    assert!(!rpc
        .iter()
        .any(|line| { line.contains("method=\"ping\"") && line.contains("outcome=\"ok\"") }));

    let subscriptions: Vec<_> = text
        .lines()
        .filter(|line| line.contains("Herdr subscription completed"))
        .collect();
    assert_eq!(
        subscriptions.len(),
        3,
        "subscription completions were missing or duplicated: {text}"
    );
    assert_eq!(
        subscriptions
            .iter()
            .filter(|line| line.contains("outcome=\"error\""))
            .count(),
        2,
        "each failed initial/add subscription needs one error completion: {text}"
    );

    assert!(!text.contains(METHOD_SENTINEL));
    assert!(!text.contains("PAYLOAD_SECRET"));
    assert!(!text.contains("SUBSCRIBE_SECRET"));
}
