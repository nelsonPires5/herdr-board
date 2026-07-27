//! board-daemon — boardd server (OWNED BY PHASE D).
//!
//! The single SQLite writer, run queue, column-engine executor, and NDJSON Unix
//! socket server. Started by `board daemon`; talks to herdr (or a local child
//! spawner) to launch agents.

mod dispatch;
mod herdr_conn;
mod herdr_snapshot;
mod logging;
mod ops;
mod recovery;
mod rescue;
mod server;
mod session;
mod settings;
mod singleton;
mod spawner;
mod state;
mod store;
mod supervisor;
#[cfg(test)]
mod testkit;
mod watchers;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;

use crate::spawner::Spawner;
use board_core::config::{Config, RootConfig};
use board_core::db::Db;
use board_core::paths;
use board_herdr::HerdrClient;
use tokio::sync::{broadcast, mpsc, watch};

use crate::settings::{DaemonSettings, ProcessEnv, SpawnerKind};
use crate::spawner::{HerdrSpawner, LocalSpawner};
use crate::state::Daemon;
use crate::store::Store;

/// The board protocol method names boardd routes, generated from the dispatch
/// table itself so it cannot drift from routing.
pub use ops::ROUTED_METHODS;

/// The herdr protocol version the daemon requires.
pub(crate) const HERDR_PROTOCOL: u32 = 17;

/// Run the daemon. `foreground` mirrors logs to stderr and is used by
/// `board daemon --foreground`. Returns `Ok(())` immediately if another daemon
/// already holds the single-instance lock.
pub fn run(foreground: bool) -> anyhow::Result<()> {
    let db_path = paths::db_path();
    let socket_path = paths::socket_path();

    // Single instance: exclusive flock on <db>.lock. Losing the race = exit 0.
    let _lock = match singleton::acquire(&db_path)? {
        Some(f) => f,
        None => return Ok(()),
    };

    logging::init_logging(foreground);
    tracing::info!("boardd starting: db={:?} socket={:?}", db_path, socket_path);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async_main(db_path, socket_path))?;
    tracing::info!("boardd stopped");
    Ok(())
}

async fn async_main(db_path: PathBuf, socket_path: PathBuf) -> anyhow::Result<()> {
    // Parse the root document exactly once. In particular, do not let the
    // daemon settings' legacy parser turn malformed TOML into defaults.
    let root = RootConfig::load()?;
    let settings = DaemonSettings::from_root(&root, &ProcessEnv)?;
    let mut config: Config = root.board;
    // Resolve the Pi agent dir for live model discovery unless the user pinned
    // it in config.toml. Tests construct Config directly (pi_agent_dir stays
    // None), so this never runs for them and the pi catalog stays static.
    if config.pi_agent_dir.is_none() {
        config.pi_agent_dir = board_core::pi_catalog::default_agent_dir();
    }
    tracing::info!(
        "spawner={:?} max_concurrent={}",
        settings.spawner,
        config.max_concurrent
    );

    let db = Db::open(&db_path)?;
    let store = Store::new(db);

    // Herdr handle (best effort): used for notifications, liveness, status, and
    // the default-session event stream. Absence never crashes the daemon.
    let herdr: Option<HerdrClient> = match settings.spawner {
        SpawnerKind::Local => None,
        SpawnerKind::Herdr => HerdrClient::connect_default().ok(),
    };

    // Session registry (herdr spawner only): resolves card sessions to sockets.
    let session_registry = match settings.spawner {
        SpawnerKind::Local => None,
        SpawnerKind::Herdr => Some(crate::session::SessionRegistry::new(
            board_herdr::default_socket_path(),
        )),
    };

    let spawner: Arc<dyn Spawner> = match settings.spawner {
        SpawnerKind::Local => Arc::new(LocalSpawner::new()),
        SpawnerKind::Herdr => Arc::new(HerdrSpawner::new(board_herdr::default_socket_path())),
    };

    let (dispatch_tx, mut dispatch_rx) = mpsc::unbounded_channel::<()>();
    let (events_tx, _events_rx) = broadcast::channel(256);
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);

    let daemon = Arc::new(Daemon::new(
        store,
        config,
        settings,
        db_path,
        socket_path.clone(),
        spawner,
        herdr,
        session_registry,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ));

    // Background tasks.
    {
        let d = daemon.clone();
        tokio::spawn(async move {
            while dispatch_rx.recv().await.is_some() {
                dispatch::dispatch_pass(&d).await;
            }
        });
    }
    tokio::spawn(watchers::timeout_ticker(daemon.clone()));
    tokio::spawn(watchers::local_liveness_poller(daemon.clone()));
    if matches!(daemon.settings.spawner, SpawnerKind::Herdr) {
        // The supervisor is independent of the startup best-effort client: a
        // Herdr server may appear after boardd and must still be discovered.
        let d = daemon.clone();
        std::thread::spawn(move || watchers::herdr_event_thread(d));
        // Durable runs that could not be adopted at startup are not in the
        // in-memory watch set yet. Retry the conservative pass slowly until a
        // socket becomes available; successful adoption feeds the per-socket
        // stream supervisor above.
        if let Some(registry) = &daemon.session_registry {
            let d = daemon.clone();
            let default_socket = registry.default_socket().to_path_buf();
            tokio::spawn(async move {
                while !d.is_shutdown() {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    if d.is_shutdown() {
                        break;
                    }
                    supervisor::reconcile_once(
                        &d,
                        Arc::new(crate::session::SessionRegistry::new(default_socket.clone())),
                        Arc::new(supervisor::HerdrRuntime),
                        Arc::new(supervisor::SystemClock),
                    )
                    .await;
                }
            });
        }
    }
    spawn_signal_handler(daemon.clone());

    // Startup recovery is independent of the best-effort initial Herdr
    // connection. The always-on supervisor subsequently repeats conservative
    // reconciliation after every connection and at a slow interval.
    recovery::startup_recovery(&daemon).await;
    daemon.wake_dispatch();

    // Bind the socket (removing any stale file first) and serve.
    let _ = std::fs::remove_file(&socket_path);
    if let Some(parent) = socket_path.parent() {
        // Name the directory: without it the failure resurfaces below as an
        // opaque `bind` error about the socket path instead.
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create the boardd socket directory {parent:?}"))?;
    }
    let listener = tokio::net::UnixListener::bind(&socket_path)
        .with_context(|| format!("cannot bind the boardd socket {socket_path:?}"))?;
    tracing::info!("listening on {:?}", socket_path);

    server::serve(daemon.clone(), listener).await;

    // Graceful: leave running panes alone; just clean up the socket.
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

fn spawn_signal_handler(d: Arc<Daemon>) {
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("SIGTERM handler: {e}");
                return;
            }
        };
        let mut int = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("SIGINT handler: {e}");
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => tracing::info!("SIGTERM received"),
            _ = int.recv() => tracing::info!("SIGINT received"),
        }
        d.trigger_shutdown();
    });
}
