//! Shared daemon state (`Daemon`) plus small effect helpers (events, herdr
//! notifications, watch-set tracking, shutdown).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::spawner::{RuntimeHandle, Spawner};
use board_core::config::Config;
use board_core::protocol::{BoardChangedReason, Event, RunOutcome};
use board_herdr::{HerdrClient, NotificationSound};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, watch, Mutex as AsyncMutex};

use crate::session::SessionRegistry;
use crate::settings::DaemonSettings;
use crate::store::Store;

fn system_wall_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// In-memory bookkeeping for a started run.
pub struct ActiveRun {
    pub card_id: i64,
    pub handle: RuntimeHandle,
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

impl ActiveRun {
    /// Reconstruct the monotonic timeout deadline from a durable wall-clock
    /// millisecond timestamp. Returns `None` only for an unlimited durable
    /// deadline; an overdue deadline reconstructs as `adopted_at` so it fires
    /// immediately instead of receiving a fresh budget.
    #[inline]
    pub fn reconstruct_deadline(
        adopted_at: Instant,
        wall_now_ms: i64,
        deadline_at_ms: Option<i64>,
    ) -> Option<Instant> {
        deadline_at_ms.and_then(|ms| {
            adopted_at.checked_add(Duration::from_millis(
                ms.saturating_sub(wall_now_ms).max(0) as u64
            ))
        })
    }

    /// Reconstruct the monotonic `awaiting_since` instant from a durable
    /// wall-clock millisecond timestamp. Saturates at `adopted_at` (the run
    /// cannot have entered awaiting before it was adopted).
    #[inline]
    pub fn reconstruct_awaiting_since(
        adopted_at: Instant,
        wall_now_ms: i64,
        paused_at_ms: Option<i64>,
    ) -> Option<Instant> {
        paused_at_ms.map(|paused| {
            adopted_at
                .checked_sub(Duration::from_millis(
                    wall_now_ms.saturating_sub(paused).max(0) as u64,
                ))
                .unwrap_or(adopted_at)
        })
    }

    /// Enter awaiting: disarm idle tracking and begin the timeout pause.
    #[inline]
    pub fn enter_awaiting(&mut self, now: Instant) {
        self.idle_since = None;
        self.awaiting_since = Some(now);
    }

    /// Leave awaiting: resume column timeout, shifting the deadline forward
    /// by the time spent awaiting. A clock observation before the pause point
    /// contributes zero via `saturating_duration_since`.
    #[inline]
    pub fn leave_awaiting(&mut self, now: Instant) {
        if let Some(paused) = self.awaiting_since.take() {
            if let Some(deadline) = &mut self.timeout_deadline {
                *deadline += now.saturating_duration_since(paused);
            }
        }
    }
}

/// In-memory scheduler state.
#[derive(Default)]
pub struct Sched {
    /// Started runs by run id.
    pub active: HashMap<i64, ActiveRun>,
    /// Consecutive auto-hops per card (reset on human action / chain end).
    pub chain_hops: HashMap<i64, u32>,
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
    /// Injectable wall clock used only for durable timeout timestamps.
    pub wall_now_ms: fn() -> i64,
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
    /// Serializes complete dispatch passes so capacity/space claims remain
    /// authoritative until their launches have either registered or failed.
    pub dispatch_pass: AsyncMutex<()>,
    pub sched: Mutex<Sched>,
    pub watch: Mutex<WatchState>,
    shutdown_tx: watch::Sender<bool>,
    stopping: AtomicBool,
    #[cfg(test)]
    pub effect_log: Mutex<Option<Arc<Mutex<Vec<&'static str>>>>>,
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
            wall_now_ms: system_wall_now_ms,
            config,
            settings,
            db_path,
            socket_path,
            spawner,
            herdr,
            session_registry,
            events_tx,
            dispatch_tx,
            dispatch_pass: AsyncMutex::new(()),
            sched: Mutex::new(Sched::default()),
            watch: Mutex::new(WatchState::default()),
            shutdown_tx,
            stopping: AtomicBool::new(false),
            #[cfg(test)]
            effect_log: Mutex::new(None),
        }
    }

    pub fn wall_now_ms(&self) -> i64 {
        (self.wall_now_ms)()
    }

    /// Broadcast an event to all subscribers (no-op if none).
    pub fn emit(&self, ev: Event) {
        #[cfg(test)]
        self.record_effect(match &ev {
            Event::RunEnded { .. } => "run_ended",
            Event::BoardChanged { .. } => "board_changed",
        });
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
        #[cfg(test)]
        self.record_effect("dispatch_wake");
        let _ = self.dispatch_tx.send(());
    }

    /// Fire a herdr notification (best effort, detached; no-op without herdr).
    pub fn notify(&self, title: String, body: Option<String>, sound: NotificationSound) {
        #[cfg(test)]
        self.record_effect("notification");
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
        #[cfg(test)]
        self.record_effect("watch");
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

    #[cfg(test)]
    pub(crate) fn record_effect(&self, effect: &'static str) {
        if let Some(log) = self.effect_log.lock().unwrap().as_ref() {
            log.lock().unwrap().push(effect);
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
