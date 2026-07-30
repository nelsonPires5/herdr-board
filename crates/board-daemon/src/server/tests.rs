use super::*;
use board_core::protocol::RunOutcome;

use std::io;
use std::path::PathBuf;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

#[derive(Clone, Default)]
struct TraceBuffer(Arc<StdMutex<Vec<u8>>>);

struct TraceGuard(Arc<StdMutex<Vec<u8>>>);

impl io::Write for TraceGuard {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TraceBuffer {
    type Writer = TraceGuard;
    fn make_writer(&'a self) -> Self::Writer {
        TraceGuard(self.0.clone())
    }
}

impl TraceBuffer {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

use crate::state::Daemon;
use crate::testkit;

fn changed(card_id: i64) -> Event {
    Event::BoardChanged {
        reason: BoardChangedReason::CardUpdated,
        board_id: None,
        card_id: Some(card_id),
        column_id: None,
    }
}
fn ended(run_id: i64) -> Event {
    Event::RunEnded {
        card_id: 1,
        run_id,
        outcome: RunOutcome::Ok,
    }
}

#[test]
fn consecutive_board_changes_coalesce_to_latest() {
    let mut b = Buffer::new(2);
    assert!(b.push_event(changed(1)));
    assert!(b.push_event(changed(2)));
    assert_eq!(b.entries.len(), 1);
    assert!(matches!(
        b.pop(),
        Some(Outbound::Event(Event::BoardChanged {
            card_id: Some(2),
            ..
        }))
    ));
}

#[test]
fn terminal_event_is_not_overwritten_and_order_is_preserved() {
    let mut b = Buffer::new(3);
    assert!(b.push_event(changed(1)));
    assert!(b.push_event(ended(7)));
    assert!(b.push_event(changed(2)));
    assert!(matches!(
        b.pop(),
        Some(Outbound::Event(Event::BoardChanged {
            card_id: Some(1),
            ..
        }))
    ));
    assert!(matches!(
        b.pop(),
        Some(Outbound::Event(Event::RunEnded { run_id: 7, .. }))
    ));
    assert!(matches!(
        b.pop(),
        Some(Outbound::Event(Event::BoardChanged {
            card_id: Some(2),
            ..
        }))
    ));
}

#[test]
fn terminal_event_on_full_buffer_disconnects() {
    let mut b = Buffer::new(1);
    assert!(b.push_event(ended(1)));
    assert!(!b.push_event(ended(2)));
    assert!(b.closed);
}

#[tokio::test]
async fn capacity_one_flood_stays_bounded() {
    let out = Outbox::new(1);
    for id in 0..100 {
        assert!(out.event(changed(id)).await);
    }
    assert_eq!(out.buffer.lock().await.entries.len(), 1);
}

// ---------------------------------------------------------------------------
// Blocking-handler offload: ordering per connection, isolation across them
// ---------------------------------------------------------------------------

fn test_daemon() -> Arc<Daemon> {
    testkit::daemon()
        .db_path(PathBuf::from("/tmp/board-server-test.db"))
        .build_daemon()
}

/// A served socket in a short `/tmp` dir (AF_UNIX paths cap at 108 bytes).
fn serve_on_tempdir(rt: &tokio::runtime::Runtime, d: Arc<Daemon>) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("b.sock");
    let guard = rt.enter();
    let listener = UnixListener::bind(&socket).unwrap();
    drop(guard);
    rt.spawn(serve(d, listener));
    (dir, socket)
}

/// A deliberately *blocking* std client, driven from the test thread.
///
/// These tests wedge the runtime's worker threads on purpose, and a
/// `tokio::time::timeout` cannot fire while the time driver's workers are the
/// thing being starved — a broken server would hang the suite instead of
/// failing it. A socket read timeout is enforced by the kernel, so the
/// regression fails fast and loudly.
struct Client {
    reader: std::io::BufReader<std::os::unix::net::UnixStream>,
    write: std::os::unix::net::UnixStream,
}

impl Client {
    fn connect(socket: &std::path::Path) -> Client {
        let write = std::os::unix::net::UnixStream::connect(socket).unwrap();
        let read = write.try_clone().unwrap();
        read.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        Client {
            reader: std::io::BufReader::new(read),
            write,
        }
    }

    fn send(&mut self, id: &str, method: &str) {
        use std::io::Write;
        writeln!(self.write, "{{\"id\":\"{id}\",\"method\":\"{method}\"}}").unwrap();
        self.write.flush().unwrap();
    }

    /// The `id` of the next response line, or a clear failure if the server
    /// never answered within the socket read timeout.
    fn response_id(&mut self, what: &str) -> String {
        use std::io::BufRead;
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => panic!("connection closed while waiting for {what}"),
            Ok(_) => {}
            Err(e) => panic!("timed out waiting for {what}: {e}"),
        }
        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        value["id"].as_str().unwrap().to_string()
    }
}

/// Pipelined requests answer in request order even though every handler now
/// runs on the blocking pool. The store lock is held across the whole batch, so
/// the store-touching requests cannot overtake the store-free ones unless the
/// server started answering a connection concurrently.
#[test]
fn board_rpc_success_and_error_emit_redacted_completion_metadata() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let d = test_daemon();
    let captured = TraceBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(captured.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();
    // Use one process-global capture for this unit-test binary. A scoped
    // dispatcher can race tracing's global callsite-interest cache with the
    // many parallel server/ops tests; conn=17 uniquely selects these records.
    tracing::subscriber::set_global_default(subscriber).expect("install test subscriber once");
    let success = rt.block_on(dispatch_request(
        &d,
        17,
        1,
        Request {
            id: "credential-in-request-id-S3CR3T".into(),
            method: "board.list".into(),
            params: json!({"secret": "BOARD_PARAMS_SENTINEL_9fd1"}),
        },
    ));
    assert!(success.error.is_none());
    assert_eq!(success.id, "credential-in-request-id-S3CR3T");
    let failure = rt.block_on(dispatch_request(
        &d,
        17,
        2,
        Request {
            id: "error-request".into(),
            method: "not.a.method".into(),
            params: json!({"secret": "BOARD_RESULT_SENTINEL_531a"}),
        },
    ));
    assert!(failure.error.is_some());

    let text = captured.text();
    let records: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("board_rpc") && line.contains("conn=17"))
        .collect();
    assert_eq!(records.len(), 2, "completion records: {text}");
    for record in &records {
        assert!(
            record.contains("operation_family=\"board_rpc\""),
            "{record}"
        );
        assert!(record.contains("conn=17"), "{record}");
        assert!(record.contains("duration_ms="), "{record}");
        assert!(record.contains("req_id="), "{record}");
        assert!(record.contains("method="), "{record}");
        assert!(record.contains("outcome=\"ok\"") || record.contains("outcome=\"error\""));
    }
    let error = records
        .iter()
        .find(|record| record.contains("outcome=\"error\""));
    assert!(error.is_some_and(|record| record.contains("error_code=")));
    assert!(!text.contains("credential-in-request-id-S3CR3T"));
    assert!(!text.contains("BOARD_PARAMS_SENTINEL_9fd1"));
    assert!(!text.contains("BOARD_RESULT_SENTINEL_531a"));
    assert!(!text.contains("params"));
    assert!(!text.contains("result"));
}

#[test]
fn pipelined_requests_answer_in_request_order() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let d = test_daemon();
    let (_dir, socket) = serve_on_tempdir(&rt, d.clone());
    let mut client = Client::connect(&socket);

    let methods = [
        "board.list",
        "nope.one",
        "nope.two",
        "board.list",
        "nope.three",
    ];
    let held = d.store.lock();
    for (index, method) in methods.iter().enumerate() {
        client.send(&format!("r{index}"), method);
    }
    // Long enough for the server to have read the whole batch while the store
    // is still wedged; the ordering assertion below holds either way.
    std::thread::sleep(Duration::from_millis(50));
    drop(held);

    for index in 0..methods.len() {
        assert_eq!(
            client.response_id("a pipelined response"),
            format!("r{index}")
        );
    }
}

/// A request wedged inside `handle_request` must not starve another
/// connection. A single worker thread makes the regression deterministic: with
/// the handler called inline on the async task, the blocked request owns the
/// only worker and the second connection is never even accepted.
#[test]
fn a_blocked_request_does_not_starve_other_connections() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let d = test_daemon();
    let (_dir, socket) = serve_on_tempdir(&rt, d.clone());

    // Warm the first connection up so its per-connection task is provably
    // accepted and parked in `read_line` before anything is wedged.
    let mut blocked = Client::connect(&socket);
    blocked.send("warmup", "nope.method");
    assert_eq!(blocked.response_id("the warmup response"), "warmup");

    // Wedge the single SQLite writer: `board.list` now blocks inside the
    // handler for exactly as long as this guard lives.
    let held = d.store.lock();
    blocked.send("blocked", "board.list");
    // Give the already-running task time to pick the request up and enter the
    // handler. Only the setup depends on this; the assertion below does not.
    std::thread::sleep(Duration::from_millis(100));

    let mut other = Client::connect(&socket);
    other.send("other", "nope.method");
    assert_eq!(other.response_id("the second connection"), "other");

    drop(held);
    assert_eq!(blocked.response_id("the unblocked connection"), "blocked");
}

/// A handler task that dies (panic) still produces exactly one response, with
/// the internal protocol code, instead of silently dropping the request.
#[tokio::test]
async fn a_dead_handler_task_still_answers_with_an_internal_error() {
    let join_error = tokio::task::spawn_blocking(|| panic!("deliberate handler panic"))
        .await
        .expect_err("the task must have panicked");
    let response = join_error_response("boom".into(), "board.list", 7, join_error);
    assert_eq!(response.id, "boom");
    assert!(response.result.is_none());
    let error = response.error.expect("a dead handler must still answer");
    assert_eq!(error.code, CODE_INTERNAL);
    assert!(
        error.message.contains("request handler failed"),
        "{}",
        error.message
    );
}
