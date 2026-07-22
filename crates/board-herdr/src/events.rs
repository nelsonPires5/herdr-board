//! Event streaming over a dedicated herdr connection.
//!
//! `events.subscribe` must run on its own persistent socket (never the
//! request/response connection). This module opens that socket, subscribes,
//! and exposes a blocking [`Iterator`] of [`HerdrEvent`].
//!
//! ## Subscription quirk (verified live, protocol 17)
//! A `pane.agent_status_changed` subscription **requires a concrete `pane_id`**
//! — herdr validates the pane exists and rejects a wildcard/missing id with
//! `internal_error`. So the daemon must build one subscription per pane it
//! wants status for (see [`watch_subscriptions`]) and re-subscribe (or
//! reconnect) as it starts new agents. `pane.exited` / `pane.closed` are global
//! and take no `pane_id`.
//!
//! Emitted event lines use the `EventEnvelope` shape
//! `{"event":"<kind>","data":{"type":"<kind>",...}}` with **underscore** kind
//! names (`pane_agent_status_changed`, `pane_exited`), whereas *subscription*
//! entries use **dotted** names (`pane.agent_status_changed`). Both are handled
//! here.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::envelope::{Request, Response};
use crate::error::{HerdrError, Result};
use crate::types::AgentStatus;

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

/// A decoded event. Tolerant: unknown event kinds become [`HerdrEvent::Other`]
/// carrying the raw line, and unknown fields are ignored.
#[derive(Debug, Clone, PartialEq)]
pub enum HerdrEvent {
    /// `pane_agent_status_changed`.
    AgentStatusChanged {
        pane_id: String,
        workspace_id: Option<String>,
        status: AgentStatus,
        agent: Option<String>,
    },
    /// `pane_exited` or `pane_closed` — the pane is gone.
    PaneExited {
        pane_id: String,
        workspace_id: Option<String>,
    },
    /// Any other event line (raw envelope preserved).
    Other(Value),
}

#[derive(Deserialize)]
struct StatusFields {
    #[serde(default)]
    pane_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    agent_status: Option<AgentStatus>,
    #[serde(default)]
    agent: Option<String>,
}

#[derive(Deserialize)]
struct PaneFields {
    #[serde(default)]
    pane_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
}

/// Parse one raw NDJSON line into an event, or `None` if it is not an event
/// (e.g. the `subscription_started` ack, an error/ack line, or blank).
pub fn parse_event_line(line: &str) -> Option<HerdrEvent> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    // Skip request/response envelopes (acks, errors) — they carry an `id`.
    if value.get("result").is_some() || value.get("error").is_some() {
        return None;
    }
    // Event body: prefer the nested `data` object, else the value itself.
    let data = match value.get("data") {
        Some(d) if d.is_object() => d,
        _ => &value,
    };
    // The kind lives in `data.type` (underscore names) on some herdr builds and
    // in the top-level `event` key (dotted names) on others (verified live on
    // protocol 17: {"event":"pane.agent_status_changed","data":{...}} with
    // no data.type). Accept both, normalized to underscores.
    let kind = data
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| value.get("event").and_then(Value::as_str))?
        .replace('.', "_");
    match kind.as_str() {
        "pane_agent_status_changed" => {
            let f: StatusFields = serde_json::from_value(data.clone()).ok()?;
            let pane_id = f.pane_id?;
            Some(HerdrEvent::AgentStatusChanged {
                pane_id,
                workspace_id: f.workspace_id,
                status: f.agent_status.unwrap_or(AgentStatus::Unknown),
                agent: f.agent,
            })
        }
        "pane_exited" | "pane_closed" => {
            let f: PaneFields = serde_json::from_value(data.clone()).ok()?;
            let pane_id = f.pane_id?;
            Some(HerdrEvent::PaneExited {
                pane_id,
                workspace_id: f.workspace_id,
            })
        }
        _ => Some(HerdrEvent::Other(value)),
    }
}

/// Retry policy for [`HerdrEvents::connect_with_retry`].
#[derive(Debug, Clone)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
    pub multiplier: f64,
    /// `None` = retry forever (daemon default); `Some(n)` = give up after `n`
    /// failed attempts and return the last error.
    pub max_retries: Option<usize>,
}

impl Default for Backoff {
    fn default() -> Backoff {
        Backoff {
            initial: Duration::from_millis(200),
            max: Duration::from_secs(5),
            multiplier: 2.0,
            max_retries: None,
        }
    }
}

impl Backoff {
    /// A bounded policy (useful for tests).
    pub fn bounded(max_retries: usize) -> Backoff {
        Backoff {
            max_retries: Some(max_retries),
            ..Backoff::default()
        }
    }

    fn next_delay(&self, current: Duration) -> Duration {
        let next = current.mul_f64(self.multiplier);
        if next > self.max {
            self.max
        } else {
            next
        }
    }
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
}

impl HerdrEvents {
    /// Connect and subscribe. Reads and validates the `subscription_started`
    /// ack before returning.
    pub fn connect(path: &Path, subscriptions: &[Subscription]) -> Result<HerdrEvents> {
        let stream = UnixStream::connect(path)?;
        let reader = BufReader::new(stream.try_clone()?);
        let writer = stream;
        let mut ev = HerdrEvents {
            path: path.to_path_buf(),
            reader,
            writer,
            pending: String::new(),
        };
        ev.send_subscribe(subscriptions)?;
        ev.read_ack()?;
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
        self.send_subscribe(subscriptions)?;
        self.read_ack()
    }

    /// The socket path (for reconnects).
    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    /// Wait up to `timeout` for the next event. `Ok(None)` means the deadline
    /// passed with the stream still healthy — callers use this to interleave
    /// housekeeping (shutdown checks, watch-set changes) with a blocking read.
    /// Non-event lines (acks) are skipped within the same call.
    pub fn poll_event(&mut self, timeout: Duration) -> Result<Option<HerdrEvent>> {
        self.reader.get_ref().set_read_timeout(Some(timeout))?;
        loop {
            match self.reader.read_line(&mut self.pending) {
                Ok(0) => return Err(HerdrError::Disconnected),
                Ok(_) => {
                    if !self.pending.ends_with('\n') {
                        // Partial line (deadline hit mid-line): keep and retry.
                        continue;
                    }
                    let line = std::mem::take(&mut self.pending);
                    if let Some(ev) = parse_event_line(&line) {
                        return Ok(Some(ev));
                    }
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(HerdrError::from(e)),
            }
        }
    }

    fn send_subscribe(&mut self, subscriptions: &[Subscription]) -> Result<()> {
        let subs: Vec<Value> = subscriptions
            .iter()
            .cloned()
            .map(Subscription::into_value)
            .collect();
        let req = Request {
            id: "subscribe".to_string(),
            method: "events.subscribe".to_string(),
            params: json!({ "subscriptions": subs }),
        };
        self.writer.write_all(req.to_line()?.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }

    fn read_ack(&mut self) -> Result<()> {
        loop {
            let mut buf = String::new();
            let n = self.reader.read_line(&mut buf)?;
            if n == 0 {
                return Err(HerdrError::Disconnected);
            }
            if buf.trim().is_empty() {
                continue;
            }
            let resp = Response::from_line(&buf)?;
            if let Some(err) = resp.error {
                return Err(HerdrError::Protocol {
                    code: err.code,
                    message: err.message,
                });
            }
            // subscription_started (or any result) => subscription is live.
            return Ok(());
        }
    }
}

impl Iterator for HerdrEvents {
    type Item = HerdrEvent;

    fn next(&mut self) -> Option<HerdrEvent> {
        loop {
            let mut buf = String::new();
            match self.reader.read_line(&mut buf) {
                Ok(0) => return None,
                Ok(_) => {
                    if let Some(ev) = parse_event_line(&buf) {
                        return Some(ev);
                    }
                    // ack / blank / non-event: keep reading.
                }
                Err(_) => return None,
            }
        }
    }
}
