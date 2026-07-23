//! The NDJSON Unix-socket server: accept loop, per-connection request handling,
//! and `events.subscribe` fan-out.

use std::collections::VecDeque;
use std::sync::Arc;

use board_core::protocol::{BoardChangedReason, Event, Request, Response, SubscribeResult};
use serde::Serialize;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, Mutex, Notify};

use crate::ops;
use crate::state::Daemon;

const OUTBOUND_CAPACITY: usize = 64;

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
                Ok((stream, _)) => { tokio::spawn(handle_conn(d.clone(), stream)); }
                Err(e) => tracing::warn!("accept failed: {e}"),
            },
            _ = rx.changed() => break,
        }
        if d.is_shutdown() {
            break;
        }
    }
    tracing::info!("server: shutting down accept loop");
}

async fn handle_conn(d: Arc<Daemon>, stream: UnixStream) {
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
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim_end();
                if trimmed.is_empty() {
                    continue;
                }
                let req: Request = match serde_json::from_str(trimmed) {
                    Ok(r) => r,
                    Err(e) => {
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
                    let ack = Response::ok(req.id, json!(SubscribeResult { subscribed: true }));
                    if !outbox.response(to_line(&ack)).await {
                        break;
                    }
                    if event_forwarder.is_none() {
                        event_forwarder = Some(spawn_event_forwarder(&d, outbox.clone()));
                    }
                    continue;
                }
                let resp = match ops::handle_request(&d, &req.method, req.params) {
                    Ok(v) => Response::ok(req.id, v),
                    Err(e) => Response::err(req.id, e.code(), e.to_string()),
                };
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

fn spawn_event_forwarder(d: &Arc<Daemon>, outbox: Arc<Outbox>) -> tokio::task::JoinHandle<()> {
    let mut ev_rx = d.events_tx.subscribe();
    tokio::spawn(async move {
        loop {
            let event = match ev_rx.recv().await {
                Ok(ev) => ev,
                Err(broadcast::error::RecvError::Lagged(_)) => Event::BoardChanged {
                    reason: BoardChangedReason::CardUpdated,
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
mod tests {
    use super::*;
    use board_core::protocol::RunOutcome;

    fn changed(card_id: i64) -> Event {
        Event::BoardChanged {
            reason: BoardChangedReason::CardUpdated,
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
}
