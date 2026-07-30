use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use board_herdr::{HerdrError, HerdrEvents, Subscription};

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

fn socket() -> (tempfile::TempDir, PathBuf, UnixListener) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&path).unwrap();
    (dir, path, listener)
}

fn request_id(reader: &mut BufReader<UnixStream>) -> String {
    let mut line = String::new();
    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
    serde_json::from_str::<serde_json::Value>(&line).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn matching_subscription_ack_requires_protocol_17_typed_result() {
    const INITIAL_SECRET: &str = "INITIAL_ACK_PAYLOAD_SECRET_53d1";
    const ADD_SECRET: &str = "ADD_ACK_PAYLOAD_SECRET_9ca2";

    let captured = TraceBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(captured.clone())
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    // A matching id is not sufficient: protocol 17 requires the typed
    // {"type":"subscription_started"} result verified from
    // `herdr api schema --json` (Herdr 0.7.5, protocol 17).
    let (_initial_dir, initial_path, initial_listener) = socket();
    let initial_peer = thread::spawn(move || {
        let (stream, _) = initial_listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let id = request_id(&mut reader);
        writeln!(
            &stream,
            "{}",
            serde_json::json!({
                "id": id,
                "result": {"type": INITIAL_SECRET, "detail": INITIAL_SECRET}
            })
        )
        .unwrap();
    });
    let initial = HerdrEvents::connect(
        &initial_path,
        &(0..11)
            .map(|_| Subscription::pane_exited())
            .collect::<Vec<_>>(),
    );
    initial_peer.join().unwrap();

    // Establish a valid stream, then require the same typed acknowledgement
    // for a later add-subscription request with its own exact matching id.
    let (_add_dir, add_path, add_listener) = socket();
    let add_peer = thread::spawn(move || {
        let (stream, _) = add_listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let initial_id = request_id(&mut reader);
        writeln!(
            &stream,
            "{}",
            serde_json::json!({
                "id": initial_id,
                "result": {"type": "subscription_started"}
            })
        )
        .unwrap();
        let add_id = request_id(&mut reader);
        writeln!(
            &stream,
            "{}",
            serde_json::json!({
                "id": add_id,
                "result": {"type": ADD_SECRET, "detail": ADD_SECRET}
            })
        )
        .unwrap();
    });
    let mut events = HerdrEvents::connect(
        &add_path,
        &(0..12)
            .map(|_| Subscription::pane_closed())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let added = events.add_subscriptions(
        &(0..13)
            .map(|_| Subscription::pane_exited())
            .collect::<Vec<_>>(),
    );
    add_peer.join().unwrap();

    assert!(
        matches!(initial, Err(HerdrError::Decode(_))),
        "malformed initial acknowledgement was accepted"
    );
    assert!(
        matches!(added, Err(HerdrError::Decode(_))),
        "malformed add-subscription acknowledgement was accepted: {added:?}"
    );

    let text = captured.text();
    for count in [11, 13] {
        let records: Vec<_> = text
            .lines()
            .filter(|line| line.contains(&format!("subscription_count={count}")))
            .collect();
        assert_eq!(records.len(), 1, "records for count {count}: {text}");
        assert!(records[0].contains("outcome=\"error\""), "{text}");
        assert!(records[0].contains("error_category=\"decode\""), "{text}");
        assert!(!records[0].contains("outcome=\"ok\""), "{text}");
    }
    assert!(!text.contains(INITIAL_SECRET), "{text}");
    assert!(!text.contains(ADD_SECRET), "{text}");
    assert!(!text.contains("result"), "{text}");
}
