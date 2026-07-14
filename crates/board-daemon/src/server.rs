//! The NDJSON Unix-socket server: accept loop, per-connection request handling,
//! and `events.subscribe` fan-out.

use std::sync::Arc;

use board_core::protocol::{Request, Response, SubscribeResult};
use serde::Serialize;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};

use crate::ops;
use crate::state::Daemon;

fn to_line<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v)
        .unwrap_or_else(|_| "{\"error\":{\"code\":5,\"message\":\"encode\"}}".into())
}

/// Accept connections until shutdown.
pub async fn serve(d: Arc<Daemon>, listener: UnixListener) {
    let mut rx = d.shutdown_rx();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    tokio::spawn(handle_conn(d.clone(), stream));
                }
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

    // A single writer task serializes all outbound lines (responses + events).
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut w = write_half;
        while let Some(line) = out_rx.recv().await {
            if w.write_all(line.as_bytes()).await.is_err() || w.write_all(b"\n").await.is_err() {
                break;
            }
            let _ = w.flush().await;
        }
    });

    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
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
                        let _ = out_tx.send(to_line(&Response::err(
                            "",
                            1,
                            format!("bad request: {e}"),
                        )));
                        continue;
                    }
                };

                if req.method == "events.subscribe" {
                    let ack =
                        Response::ok(req.id.clone(), json!(SubscribeResult { subscribed: true }));
                    let _ = out_tx.send(to_line(&ack));
                    spawn_event_forwarder(&d, out_tx.clone());
                    continue;
                }

                let resp = match ops::handle_request(&d, &req.method, req.params) {
                    Ok(v) => Response::ok(req.id, v),
                    Err(e) => Response::err(req.id, e.code(), e.to_string()),
                };
                let _ = out_tx.send(to_line(&resp));
            }
            Err(_) => break,
        }
    }

    drop(out_tx);
    let _ = writer.await;
}

fn spawn_event_forwarder(d: &Arc<Daemon>, out_tx: mpsc::UnboundedSender<String>) {
    let mut ev_rx = d.events_tx.subscribe();
    tokio::spawn(async move {
        loop {
            match ev_rx.recv().await {
                Ok(ev) => {
                    if out_tx.send(to_line(&ev)).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}
