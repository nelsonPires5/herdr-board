//! The NDJSON Unix-socket server: accept loop, per-connection request handling,
//! and `events.subscribe` fan-out.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use board_core::protocol::{BoardChangedReason, Event, Request, Response, SubscribeResult};
use serde::Serialize;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Mutex, Notify};

use crate::ops;
use crate::state::Daemon;

const OUTBOUND_CAPACITY: usize = 64;

/// Protocol code 5 ("internal"), the same bucket `docs/protocol.md` gives
/// sqlite/json/io failures. Used when the blocking handler task itself dies.
const CODE_INTERNAL: i32 = 5;

/// Monotonic per-process connection id. Only used to correlate a request span
/// with its connection in the log; never persisted or sent on the wire.
static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

fn to_line<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v)
        .unwrap_or_else(|_| "{\"error\":{\"code\":5,\"message\":\"encode\"}}".into())
}

#[derive(Debug)]
enum Outbound {
    Response(String),
    Event(Event),
}

#[derive(Debug)]
struct Buffer {
    entries: VecDeque<Outbound>,
    capacity: usize,
    closed: bool,
    pressure_logged: bool,
}

impl Buffer {
    fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
            closed: false,
            pressure_logged: false,
        }
    }

    fn push_response(&mut self, line: String) -> Result<(), String> {
        if self.closed || self.entries.len() == self.capacity {
            return Err(line);
        }
        self.entries.push_back(Outbound::Response(line));
        Ok(())
    }

    fn push_event(&mut self, event: Event) -> bool {
        if self.closed {
            return false;
        }
        if matches!(event, Event::BoardChanged { .. })
            && matches!(
                self.entries.back(),
                Some(Outbound::Event(Event::BoardChanged { .. }))
            )
        {
            self.entries.pop_back();
            self.entries.push_back(Outbound::Event(event));
            self.log_pressure();
            return true;
        }
        if self.entries.len() == self.capacity {
            self.log_pressure();
            self.closed = true;
            return false;
        }
        self.entries.push_back(Outbound::Event(event));
        true
    }

    fn pop(&mut self) -> Option<Outbound> {
        let item = self.entries.pop_front();
        if self.entries.is_empty() {
            self.pressure_logged = false;
        }
        item
    }

    fn log_pressure(&mut self) {
        if !self.pressure_logged {
            tracing::warn!("subscriber outbound queue under pressure");
            self.pressure_logged = true;
        }
    }
}

struct Outbox {
    buffer: Mutex<Buffer>,
    changed: Notify,
}

impl Outbox {
    fn new(capacity: usize) -> Self {
        Self {
            buffer: Mutex::new(Buffer::new(capacity)),
            changed: Notify::new(),
        }
    }

    async fn response(&self, mut line: String) -> bool {
        loop {
            let notified = self.changed.notified();
            match self.buffer.lock().await.push_response(line) {
                Ok(()) => {
                    self.changed.notify_one();
                    return true;
                }
                Err(returned) => {
                    line = returned;
                    if self.buffer.lock().await.closed {
                        return false;
                    }
                }
            }
            notified.await;
        }
    }

    async fn event(&self, event: Event) -> bool {
        let accepted = self.buffer.lock().await.push_event(event);
        self.changed.notify_waiters();
        accepted
    }

    async fn next(&self) -> Option<Outbound> {
        loop {
            let notified = self.changed.notified();
            let mut buffer = self.buffer.lock().await;
            if let Some(item) = buffer.pop() {
                self.changed.notify_waiters();
                return Some(item);
            }
            if buffer.closed {
                return None;
            }
            drop(buffer);
            notified.await;
        }
    }

    async fn close(&self) {
        self.buffer.lock().await.closed = true;
        self.changed.notify_waiters();
    }
}

/// Accept connections until shutdown.
pub async fn serve(d: Arc<Daemon>, listener: UnixListener) {
    let mut rx = d.shutdown_rx();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
                    tokio::spawn(handle_conn(d.clone(), stream, conn_id));
                }
                Err(_) => tracing::warn!(error_category = "transport", "accept failed"),
            },
            _ = rx.changed() => break,
        }
        if d.is_shutdown() {
            break;
        }
    }
    tracing::info!("server: shutting down accept loop");
}

/// Serve one client connection.
///
/// Response ordering is structural: the read loop handles exactly one request
/// at a time and only pushes the next response after awaiting the previous
/// one, so a client always sees one response per request, in request order —
/// even though every handler now runs on the blocking pool.
async fn handle_conn(d: Arc<Daemon>, stream: UnixStream, conn_id: u64) {
    let (read_half, write_half) = stream.into_split();
    let outbox = Arc::new(Outbox::new(OUTBOUND_CAPACITY));
    let writer_outbox = outbox.clone();
    let writer = tokio::spawn(async move {
        let mut w = write_half;
        while let Some(item) = writer_outbox.next().await {
            let line = match item {
                Outbound::Response(line) => line,
                Outbound::Event(ev) => to_line(&ev),
            };
            if w.write_all(line.as_bytes()).await.is_err() || w.write_all(b"\n").await.is_err() {
                break;
            }
            if w.flush().await.is_err() {
                break;
            }
        }
        writer_outbox.close().await;
    });

    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    let mut event_forwarder = None;
    // Correlation is daemon-owned: the wire request id is an arbitrary payload
    // field and may contain credentials. `(conn, req_id)` is unique for this
    // daemon lifetime while the original id remains untouched on the wire.
    let mut request_correlation = 0_u64;
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim_end();
                if trimmed.is_empty() {
                    continue;
                }
                request_correlation = request_correlation.saturating_add(1);
                let req: Request = match serde_json::from_str(trimmed) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::info!(target: "board_rpc", operation_family = "board_rpc", conn = conn_id, req_id = request_correlation, method = "<parse>", outcome = "error", error_code = 1_i32, error_kind = "bad_request", duration_ms = 0_u64, "board RPC completed");
                        if !outbox
                            .response(to_line(&Response::err("", 1, format!("bad request: {e}"))))
                            .await
                        {
                            break;
                        }
                        continue;
                    }
                };
                if req.method == "events.subscribe" {
                    tracing::info!(target: "board_rpc", operation_family = "board_rpc", conn = conn_id, req_id = request_correlation, method = "events.subscribe", outcome = "ok", duration_ms = 0_u64, "board RPC completed");
                    // Activate the event receiver BEFORE acknowledging: the
                    // client treats the acknowledgement as "events will be
                    // delivered for any mutation from now on" and refetches
                    // the snapshot right after it. Acknowledging first would
                    // leave a window where a mutation between that refetch and
                    // receiver registration is missed.
                    if event_forwarder.is_none() {
                        event_forwarder = Some(spawn_event_forwarder(&d, outbox.clone()));
                    }
                    let ack = Response::ok(req.id, json!(SubscribeResult { subscribed: true }));
                    if !outbox.response(to_line(&ack)).await {
                        break;
                    }
                    continue;
                }
                let resp = dispatch_request(&d, conn_id, request_correlation, req).await;
                if !outbox.response(to_line(&resp)).await {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    outbox.close().await;
    if let Some(forwarder) = event_forwarder {
        forwarder.abort();
        let _ = forwarder.await;
    }
    let _ = writer.await;
}

/// Run one request handler off the async runtime.
///
/// `ops::handle_request` is fully blocking: it takes the SQLite mutex, makes
/// blocking AF_UNIX Herdr RPCs whose transport deadlines stack into tens of
/// seconds, and shells out to `herdr session list`. Running that inline in the
/// per-connection task pinned a tokio worker thread for the whole call, so
/// enough concurrent requests starved the accept loop, the dispatcher, and
/// shutdown. `spawn_blocking` moves it to the blocking pool; the caller still
/// awaits it, which is what keeps per-connection ordering intact.
async fn dispatch_request(
    d: &Arc<Daemon>,
    conn_id: u64,
    request_correlation: u64,
    req: Request,
) -> Response {
    let Request { id, method, params } = req;
    let diagnostic_method = if ops::ROUTED_METHODS.contains(&method.as_str()) {
        method.as_str()
    } else {
        "<unknown>"
    };
    let started = Instant::now();
    let span = tracing::info_span!(
        "request",
        conn = conn_id,
        method = diagnostic_method,
        req_id = request_correlation
    );
    let handler_d = d.clone();
    let handler_method = method.clone();
    let handled = tokio::task::spawn_blocking(move || {
        let _entered = span.enter();
        ops::handle_request(&handler_d, &handler_method, params)
    })
    .await;
    let duration_ms = started.elapsed().as_millis() as u64;
    match handled {
        Ok(Ok(value)) => {
            tracing::info!(target: "board_rpc", operation_family = "board_rpc", conn = conn_id, req_id = request_correlation, method = diagnostic_method, outcome = "ok", duration_ms, "board RPC completed");
            Response::ok(id, value)
        }
        Ok(Err(e)) => {
            let error_code = e.code();
            tracing::info!(target: "board_rpc", operation_family = "board_rpc", conn = conn_id, req_id = request_correlation, method = diagnostic_method, outcome = "error", error_code, error_kind = "protocol", duration_ms, "board RPC completed");
            Response::err(id, error_code, e.to_string())
        }
        Err(join_error) => {
            tracing::error!(target: "board_rpc", operation_family = "board_rpc", conn = conn_id, req_id = request_correlation, method = diagnostic_method, outcome = "panic", error_code = CODE_INTERNAL, error_kind = "handler_task", duration_ms, "board RPC completed");
            join_error_response(id, diagnostic_method, conn_id, join_error)
        }
    }
}

/// A panicked (or cancelled) handler must still answer: dropping the request
/// would leave the client waiting on a response that never comes, and killing
/// the connection would lose every other in-flight request on it.
fn join_error_response(
    id: String,
    method: &str,
    conn_id: u64,
    join_error: tokio::task::JoinError,
) -> Response {
    tracing::error!(
        conn = conn_id,
        method = %method,
        error_category = "task",
        "request handler task failed"
    );
    Response::err(
        id,
        CODE_INTERNAL,
        format!("internal error: request handler failed: {join_error}"),
    )
}

fn spawn_event_forwarder(d: &Arc<Daemon>, outbox: Arc<Outbox>) -> tokio::task::JoinHandle<()> {
    let mut ev_rx = d.events_tx.subscribe();
    tokio::spawn(async move {
        loop {
            let event = match ev_rx.recv().await {
                Ok(ev) => ev,
                Err(broadcast::error::RecvError::Lagged(_)) => Event::BoardChanged {
                    reason: BoardChangedReason::CardUpdated,
                    board_id: None,
                    card_id: None,
                    column_id: None,
                },
                Err(broadcast::error::RecvError::Closed) => break,
            };
            if !outbox.event(event).await {
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests;
