//! board-daemon — boardd server (OWNED BY PHASE D).
//!
//! The single SQLite writer, run queue, column-engine executor, and NDJSON
//! local-endpoint server (a Unix socket, or a named pipe on Windows). Started
//! by `board daemon`; talks to herdr (or a local child spawner) to launch
//! agents.

mod dispatch;
mod endpoint;
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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

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

/// Retain a reachable default-session handle without imposing the operation
/// compatibility gate during startup. Status and notifications perform that
/// exact gate when they run; all dispatch, event, and mutation paths keep their
/// checked connection helpers.
fn initial_herdr_handle(socket: &Path) -> Option<HerdrClient> {
    HerdrClient::connect(socket).ok()
}

/// Run the daemon. `foreground` mirrors logs to stderr and is used by
/// `board daemon --foreground`. Returns `Ok(())` immediately if another daemon
/// already holds the single-instance lock.
pub fn run(foreground: bool) -> anyhow::Result<()> {
    let db_path = paths::db_path();
    let socket_path = paths::socket_path();

    // Single instance: exclusive lock on <db>.lock. Losing the race = exit 0.
    let _lock = match singleton::acquire(&db_path)? {
        Some(f) => f,
        None => return Ok(()),
    };

    logging::init_logging(foreground);
    tracing::info!("boardd starting");

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
    // Same for the codex home: `$CODEX_HOME` else `~/.codex` (mirror of the
    // codex CLI's own default). Tests leave `codex_home` None and get the
    // static free-form catalog.
    if config.codex_home.is_none() {
        config.codex_home = board_core::codex_catalog::default_codex_home();
    }
    // Same for the opencode binary: `$OPENCODE_BIN` else `opencode` on PATH.
    // Tests leave `opencode_bin` None and get the static fallback catalog.
    if config.opencode_bin.is_none() {
        config.opencode_bin = board_core::opencode_catalog::default_opencode_bin();
    }
    // Same for the antigravity binary: `$AGY_BIN` else `agy` on PATH.
    // Tests leave `agy_bin` None and get the free-form catalog (no probe).
    if config.agy_bin.is_none() {
        config.agy_bin = board_core::agy_catalog::default_agy_bin();
    }
    tracing::info!(
        "spawner={:?} max_concurrent={}",
        settings.spawner,
        config.max_concurrent
    );

    let db = Db::open(&db_path)?;
    let store = Store::new(db);

    // Herdr handle (best effort): used for notifications, liveness, status, and
    // the default-session event stream. Keep any reachable socket handle even
    // when its current contract is incompatible; status and notifications
    // probe the supported protocol at operation time, so a server that
    // upgrades in place can recover without restarting boardd. Dispatch,
    // events, and mutations retain their own checked gates.
    let herdr: Option<HerdrClient> = match settings.spawner {
        SpawnerKind::Local => None,
        SpawnerKind::Herdr => initial_herdr_handle(&board_herdr::default_socket_path()),
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
    let retention = tokio::spawn(logging::retention_task(daemon.shutdown_rx()));
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

    // Bind the endpoint (unix: removing any stale socket file first, then
    // chmod 0600; Windows: owner-only pipe plus the identity marker) and serve.
    let (listener, endpoint) = endpoint::bind(&socket_path)?;
    tracing::info!("boardd listening");

    server::serve(daemon.clone(), listener).await;
    // The same shutdown watch that ends the accept loop ends retention; join it
    // before runtime teardown so an in-progress prune is not cancelled midway.
    if retention.await.is_err() {
        tracing::warn!(
            error_category = "retention_task",
            "diagnostic retention task failed"
        );
    }

    // Graceful: leave running panes alone; just clean up the endpoint.
    endpoint.release();
    Ok(())
}

fn spawn_signal_handler(d: Arc<Daemon>) {
    tokio::spawn(async move {
        if wait_for_shutdown_signal().await {
            d.trigger_shutdown();
        }
    });
}

/// Wait for a process-level stop request. `false` means the handlers could not
/// be installed (already logged); the daemon then stops only via `daemon.stop`.
#[cfg(unix)]
async fn wait_for_shutdown_signal() -> bool {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => {
            tracing::warn!(
                error_category = "signal_handler",
                "SIGTERM handler setup failed"
            );
            return false;
        }
    };
    let mut int = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(_) => {
            tracing::warn!(
                error_category = "signal_handler",
                "SIGINT handler setup failed"
            );
            return false;
        }
    };
    tokio::select! {
        _ = term.recv() => tracing::info!("SIGTERM received"),
        _ = int.recv() => tracing::info!("SIGINT received"),
    }
    true
}

/// Windows has no SIGTERM: the console control events are the equivalent.
/// Ctrl+C / Ctrl+Break interrupt a foreground daemon; close, logoff and
/// shutdown arrive when its console or session goes away. A detached daemon has
/// no console and stops via `daemon.stop`, exactly as `board daemon stop` does
/// on every platform.
#[cfg(windows)]
async fn wait_for_shutdown_signal() -> bool {
    use tokio::signal::windows;

    macro_rules! install {
        ($ctor:path, $name:literal) => {
            match $ctor() {
                Ok(s) => s,
                Err(_) => {
                    tracing::warn!(
                        error_category = "signal_handler",
                        concat!($name, " handler setup failed")
                    );
                    return false;
                }
            }
        };
    }
    let mut ctrl_c = install!(windows::ctrl_c, "CTRL_C");
    let mut ctrl_break = install!(windows::ctrl_break, "CTRL_BREAK");
    let mut ctrl_close = install!(windows::ctrl_close, "CTRL_CLOSE");
    let mut ctrl_logoff = install!(windows::ctrl_logoff, "CTRL_LOGOFF");
    let mut ctrl_shutdown = install!(windows::ctrl_shutdown, "CTRL_SHUTDOWN");
    tokio::select! {
        _ = ctrl_c.recv() => tracing::info!("CTRL_C received"),
        _ = ctrl_break.recv() => tracing::info!("CTRL_BREAK received"),
        _ = ctrl_close.recv() => tracing::info!("CTRL_CLOSE received"),
        _ = ctrl_logoff.recv() => tracing::info!("CTRL_LOGOFF received"),
        _ = ctrl_shutdown.recv() => tracing::info!("CTRL_SHUTDOWN received"),
    }
    true
}
