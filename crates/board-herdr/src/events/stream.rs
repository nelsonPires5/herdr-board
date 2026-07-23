use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::envelope::{Request, Response};
use crate::error::{HerdrError, Result};
use crate::transport::{self, connect_with_deadline, SocketDeadlines};

use super::backoff::Backoff;
use super::parse::{parse_event_line, HerdrEvent};

/// One subscription entry for `events.subscribe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription(Value);

impl Subscription {
    /// Watch a specific pane's agent-status transitions. `pane_id` is required
    /// by herdr — a missing/invalid id makes the subscribe call fail.
    pub fn agent_status(pane_id: &str) -> Subscription {
        Subscription(json!({ "type": "pane.agent_status_changed", "pane_id": pane_id }))
    }

    /// Watch for panes whose process exits (global; no pane_id).
    pub fn pane_exited() -> Subscription {
        Subscription(json!({ "type": "pane.exited" }))
    }

    /// Watch for panes being closed/removed (global; no pane_id).
    pub fn pane_closed() -> Subscription {
        Subscription(json!({ "type": "pane.closed" }))
    }

    /// Escape hatch: any subscription object.
    pub fn raw(value: Value) -> Subscription {
        Subscription(value)
    }

    fn into_value(self) -> Value {
        self.0
    }
}

/// The daemon's default watch set: global exit/close events plus one
/// agent-status subscription per live pane id.
pub fn watch_subscriptions(pane_ids: &[String]) -> Vec<Subscription> {
    let mut subs = vec![Subscription::pane_exited(), Subscription::pane_closed()];
    subs.extend(pane_ids.iter().map(|p| Subscription::agent_status(p)));
    subs
}

/// A persistent event-stream connection. Iterating yields decoded events until
/// the socket closes.
pub struct HerdrEvents {
    path: PathBuf,
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    /// Partial line carried across [`HerdrEvents::poll_event`] timeouts so a
    /// read deadline mid-line never drops event bytes.
    pending: String,
    pending_events: VecDeque<HerdrEvent>,
    deadlines: SocketDeadlines,
    subscribe_id: u64,
}

impl HerdrEvents {
    /// Connect and subscribe. Reads and validates the `subscription_started`
    /// ack before returning.
    pub fn connect(path: &Path, subscriptions: &[Subscription]) -> Result<HerdrEvents> {
        Self::connect_with_deadlines(path, subscriptions, SocketDeadlines::default())
    }

    /// Connect using injectable socket deadlines.
    pub fn connect_with_deadlines(
        path: &Path,
        subscriptions: &[Subscription],
        deadlines: SocketDeadlines,
    ) -> Result<HerdrEvents> {
        let stream = connect_with_deadline(path, deadlines.connect)?;
        // Put the socket in non-blocking mode so poll_event can honour
        // deadlines without SO_RCVTIMEO (which macOS locks on first read).
        // The Iterator impl uses poll with infinite timeout for blocking
        // semantics.
        transport::set_nonblocking(&stream, true)?;

        let reader = BufReader::new(stream.try_clone()?);
        let writer = stream;
        let mut ev = HerdrEvents {
            path: path.to_path_buf(),
            reader,
            writer,
            pending: String::new(),
            pending_events: VecDeque::new(),
            deadlines,
            subscribe_id: 0,
        };

        let id = ev.send_subscribe(subscriptions)?;
        ev.read_ack(&id)?;
        Ok(ev)
    }

    /// Connect with exponential backoff. Honors [`Backoff::max_retries`].
    pub fn connect_with_retry(
        path: &Path,
        subscriptions: &[Subscription],
        backoff: &Backoff,
    ) -> Result<HerdrEvents> {
        let mut attempt: usize = 0;
        let mut delay = backoff.initial;
        loop {
            match HerdrEvents::connect(path, subscriptions) {
                Ok(ev) => return Ok(ev),
                Err(e) => {
                    attempt += 1;
                    if let Some(max) = backoff.max_retries {
                        if attempt >= max {
                            return Err(e);
                        }
                    }
                    std::thread::sleep(delay);
                    delay = backoff.next_delay(delay);
                }
            }
        }
    }

    /// Add more subscriptions on the same connection (e.g. a newly spawned
    /// pane). Sends another `events.subscribe`; on failure the daemon should
    /// reconnect with the full set instead.
    pub fn add_subscriptions(&mut self, subscriptions: &[Subscription]) -> Result<()> {
        let id = self.send_subscribe(subscriptions)?;
        self.read_ack(&id)
    }

    /// The socket path (for reconnects).
    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// Wait up to `timeout` for the next event. `Ok(None)` means the deadline
    /// passed with the stream still healthy — callers use this to interleave
    /// housekeeping (shutdown checks, watch-set changes) with a blocking read.
    /// Non-event lines (acks) are skipped within the same call.
    ///
    /// Partial lines (no trailing newline) are kept in an internal buffer
    /// and survive `Ok(None)` returns so a subsequent call can complete the
    /// event once the peer writes the rest.
    pub fn poll_event(&mut self, timeout: Duration) -> Result<Option<HerdrEvent>> {
        if let Some(event) = self.pending_events.pop_front() {
            return Ok(Some(event));
        }
        let deadline = Instant::now() + timeout;
        loop {
            match self.reader.read_line(&mut self.pending) {
                Ok(0) => return Err(HerdrError::Disconnected),
                Ok(_) if self.pending.ends_with('\n') => {
                    let line = std::mem::take(&mut self.pending);
                    if let Some(event) = parse_event_line(&line) {
                        return Ok(Some(event));
                    }
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(HerdrError::from(e)),
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || !transport::poll_read_ready(&self.writer, remaining)? {
                return Ok(None);
            }
        }
    }

    fn send_subscribe(&mut self, subscriptions: &[Subscription]) -> Result<String> {
        let subs: Vec<Value> = subscriptions
            .iter()
            .cloned()
            .map(Subscription::into_value)
            .collect();
        self.subscribe_id += 1;
        let id = if self.subscribe_id == 1 {
            "subscribe".to_string()
        } else {
            format!("subscribe-{}", self.subscribe_id)
        };
        let req = Request {
            id: id.clone(),
            method: "events.subscribe".to_string(),
            params: json!({ "subscriptions": subs }),
        };
        self.writer
            .write_all(req.to_line()?.as_bytes())
            .map_err(|e| transport::deadline_io(e, "subscribe write"))?;
        self.writer
            .flush()
            .map_err(|e| transport::deadline_io(e, "subscribe write"))?;
        Ok(id)
    }

    fn read_ack(&mut self, expected_id: &str) -> Result<()> {
        let deadline = Instant::now() + self.deadlines.handshake;
        let mut pending = String::new();
        loop {
            match self.reader.read_line(&mut pending) {
                Ok(0) => return Err(HerdrError::Disconnected),
                Ok(_) if pending.ends_with('\n') => {
                    let line = std::mem::take(&mut pending);
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Some(event) = parse_event_line(&line) {
                        self.pending_events.push_back(event);
                        continue;
                    }
                    let response = Response::from_line(&line)?;
                    if response.id != expected_id {
                        continue;
                    }
                    if let Some(error) = response.error {
                        return Err(HerdrError::Protocol {
                            code: error.code,
                            message: error.message,
                        });
                    }
                    return Ok(());
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(HerdrError::from(e)),
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || !transport::poll_read_ready(&self.writer, remaining)? {
                return Err(HerdrError::Deadline {
                    operation: "subscribe ack",
                });
            }
        }
    }
}

impl Iterator for HerdrEvents {
    type Item = HerdrEvent;

    fn next(&mut self) -> Option<HerdrEvent> {
        if let Some(event) = self.pending_events.pop_front() {
            return Some(event);
        }
        loop {
            match self.reader.read_line(&mut self.pending) {
                Ok(0) => return None,
                Ok(_) => {
                    if !self.pending.ends_with('\n') {
                        // Partial line — poll indefinitely for more data.
                        if !transport::poll_read_ready_infinite(&self.writer).unwrap_or(false) {
                            return None;
                        }
                        continue;
                    }
                    let line = std::mem::take(&mut self.pending);
                    if let Some(ev) = parse_event_line(&line) {
                        return Some(ev);
                    }
                    // ack / blank / non-event: keep reading.
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No data in socket or BufReader buffer.  Poll
                    // indefinitely, then retry.
                    if !transport::poll_read_ready_infinite(&self.writer).unwrap_or(false) {
                        return None;
                    }
                }
                Err(_) => return None,
            }
        }
    }
}
