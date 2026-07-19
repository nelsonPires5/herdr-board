//! Shared daemon state (`Daemon`) plus small effect helpers (events, herdr
//! notifications, watch-set tracking, shutdown).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use board_core::config::Config;
use board_core::protocol::{BoardChangedReason, Event, RunOutcome};
use board_core::spawn::{SpawnHandle, Spawner};
use board_herdr::{HerdrClient, NotificationSound};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, watch};

use crate::session::SessionRegistry;
use crate::settings::DaemonSettings;
use crate::store::Store;

/// Max consecutive auto-transitions for one card without a human action before
/// the daemon stops the chain (cycle protection).
pub const MAX_AUTO_HOPS: u32 = 8;

/// In-memory bookkeeping for a started run.
pub struct ActiveRun {
    pub card_id: i64,
    pub handle: SpawnHandle,
    pub started: Instant,
    pub timeout_deadline: Option<Instant>,
    /// When the agent last went idle (herdr status), for idle-grace detection.
    pub idle_since: Option<Instant>,
    /// When the card entered `awaiting`. While set the column timeout is
    /// paused; on exit the deadline is shifted forward by the awaiting span.
    pub awaiting_since: Option<Instant>,
    pub is_local: bool,
    pub pane_id: Option<String>,
}

/// In-memory scheduler state.
#[derive(Default)]
pub struct Sched {
    /// Started runs by run id.
    pub active: HashMap<i64, ActiveRun>,
    /// Cards whose ended run is still applying its transition. The value is the
    /// owning run id, so a duplicate finalizer cannot clear another claim.
    pub finalizing_cards: HashMap<i64, i64>,
    /// Consecutive auto-hops per card (reset on human action / chain end).
    pub chain_hops: HashMap<i64, u32>,
}

impl Sched {
    /// Reject a public mutation that could conflict with a pending transition.
    pub fn ensure_card_not_finalizing(&self, card_id: i64) -> board_core::Result<()> {
        if self.finalizing_cards.contains_key(&card_id) {
            return Err(board_core::Error::InvalidState(
                "card finalization is in progress; retry after it completes".into(),
            ));
        }
        Ok(())
    }
}

/// The panes the herdr event watcher should subscribe to, grouped by the herdr
/// socket (session) they live on, plus a generation counter bumped whenever the
/// set changes so the watcher rebuilds its per-session subscriptions.
///
/// Grouping by socket is what fixes the multi-session bug: `agent.start`'s
/// `pane.agent_status_changed` subscription is validated per socket, so each
/// session needs its own event stream over its own socket.
#[derive(Default)]
pub struct WatchState {
    pub panes_by_socket: HashMap<PathBuf, Vec<String>>,
    pub generation: u64,
}

/// The whole daemon: store, config, spawner, herdr handle, event bus, and the
/// in-memory scheduler state. Shared as `Arc<Daemon>`.
pub struct Daemon {
    pub store: Store,
    pub config: Config,
    pub settings: DaemonSettings,
    pub db_path: PathBuf,
    pub socket_path: PathBuf,
    pub spawner: Arc<dyn Spawner>,
    /// Default-session herdr client (notifications, status, default-session
    /// space listing). `None` when the daemon runs with the local spawner.
    pub herdr: Option<HerdrClient>,
    /// herdr session registry (present iff `herdr` is). Resolves card sessions
    /// to sockets and backs `session.list` / session-scoped `space.list`.
    pub session_registry: Option<SessionRegistry>,
    pub events_tx: broadcast::Sender<Event>,
    pub dispatch_tx: mpsc::UnboundedSender<()>,
    pub sched: Mutex<Sched>,
    pub watch: Mutex<WatchState>,
    shutdown_tx: watch::Sender<bool>,
    stopping: AtomicBool,
}

impl Daemon {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Store,
        config: Config,
        settings: DaemonSettings,
        db_path: PathBuf,
        socket_path: PathBuf,
        spawner: Arc<dyn Spawner>,
        herdr: Option<HerdrClient>,
        session_registry: Option<SessionRegistry>,
        events_tx: broadcast::Sender<Event>,
        dispatch_tx: mpsc::UnboundedSender<()>,
        shutdown_tx: watch::Sender<bool>,
    ) -> Daemon {
        Daemon {
            store,
            config,
            settings,
            db_path,
            socket_path,
            spawner,
            herdr,
            session_registry,
            events_tx,
            dispatch_tx,
            sched: Mutex::new(Sched::default()),
            watch: Mutex::new(WatchState::default()),
            shutdown_tx,
            stopping: AtomicBool::new(false),
        }
    }

    /// Broadcast an event to all subscribers (no-op if none).
    pub fn emit(&self, ev: Event) {
        let _ = self.events_tx.send(ev);
    }

    /// Convenience: emit a coarse `board_changed` event.
    pub fn emit_changed(
        &self,
        reason: BoardChangedReason,
        card_id: Option<i64>,
        column_id: Option<i64>,
    ) {
        self.emit(Event::BoardChanged {
            reason,
            card_id,
            column_id,
        });
    }

    /// Emit both the typed `run_ended` and its coarse `board_changed` twin.
    pub fn emit_run_ended(&self, card_id: i64, run_id: i64, outcome: RunOutcome) {
        self.emit(Event::RunEnded {
            card_id,
            run_id,
            outcome,
        });
        self.emit_changed(BoardChangedReason::RunEnded, Some(card_id), None);
    }

    /// Wake the dispatcher to (re)evaluate the queue.
    pub fn wake_dispatch(&self) {
        let _ = self.dispatch_tx.send(());
    }

    /// Fire a herdr notification (best effort, detached; no-op without herdr).
    pub fn notify(&self, title: String, body: Option<String>, sound: NotificationSound) {
        if let Some(h) = &self.herdr {
            let mut c = h.clone();
            std::thread::spawn(move || {
                let _ = c.notification_show(&title, body.as_deref(), sound);
            });
        }
    }

    /// The herdr socket a run's pane lives on: its handle's socket, else the
    /// default session socket.
    pub fn default_herdr_socket(&self) -> PathBuf {
        self.session_registry
            .as_ref()
            .map(|r| r.default_socket().to_path_buf())
            .unwrap_or_else(board_herdr::default_socket_path)
    }

    /// Recompute the herdr watch pane-set (grouped by session socket) from
    /// active runs; bump generation on change so the watcher rebuilds.
    pub fn refresh_watch(&self) {
        let default_sock = self.default_herdr_socket();
        let grouped: HashMap<PathBuf, Vec<String>> = {
            let s = self.sched.lock().unwrap();
            let mut m: HashMap<PathBuf, Vec<String>> = HashMap::new();
            for a in s.active.values() {
                if let Some(pane) = a.pane_id.clone() {
                    let sock = a
                        .handle
                        .herdr_socket
                        .clone()
                        .unwrap_or_else(|| default_sock.clone());
                    m.entry(sock).or_default().push(pane);
                }
            }
            // Deterministic ordering so equality comparison is stable.
            for v in m.values_mut() {
                v.sort();
            }
            m
        };
        let mut w = self.watch.lock().unwrap();
        if w.panes_by_socket != grouped {
            w.panes_by_socket = grouped;
            w.generation += 1;
        }
    }

    pub fn trigger_shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.shutdown_tx.send(true);
    }

    pub fn is_shutdown(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    pub fn shutdown_rx(&self) -> watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }
}
