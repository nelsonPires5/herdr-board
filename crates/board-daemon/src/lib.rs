//! board-daemon — boardd server (OWNED BY PHASE D).
//!
//! The single SQLite writer, run queue, column-engine executor, and NDJSON Unix
//! socket server. Started by `board daemon`; talks to herdr (or a local child
//! spawner) to launch agents.

mod dispatch;
mod ops;
mod server;
mod session;
mod settings;
mod singleton;
mod spawner;
mod state;
mod store;
mod template;
mod watchers;

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Arc;

use board_core::config::Config;
use board_core::db::Db;
use board_core::paths;
use board_core::spawn::{SpawnHandle, Spawner};
use board_herdr::HerdrClient;
use tokio::sync::{broadcast, mpsc, watch};

use crate::settings::{DaemonSettings, SpawnerKind};
use crate::spawner::{HerdrSpawner, LocalSpawner};
use crate::state::Daemon;
use crate::store::Store;

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

    init_logging(foreground);
    tracing::info!("boardd starting: db={:?} socket={:?}", db_path, socket_path);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async_main(db_path, socket_path))?;
    tracing::info!("boardd stopped");
    Ok(())
}

fn init_logging(foreground: bool) {
    use tracing_subscriber::fmt::writer::MakeWriterExt;
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let log_path = paths::log_path();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = OpenOptions::new().create(true).append(true).open(&log_path);

    match file {
        Ok(f) => {
            let f = Arc::new(f);
            if foreground {
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_writer(FileWriter(f).and(std::io::stderr))
                    .try_init();
            } else {
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_writer(FileWriter(f))
                    .try_init();
            }
        }
        Err(_) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .try_init();
        }
    }
}

/// A `MakeWriter` over a shared append-mode log file.
struct FileWriter(Arc<std::fs::File>);
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for FileWriter {
    type Writer = &'a std::fs::File;
    fn make_writer(&'a self) -> Self::Writer {
        &self.0
    }
}

async fn async_main(db_path: PathBuf, socket_path: PathBuf) -> anyhow::Result<()> {
    let mut config = Config::load().unwrap_or_default();
    // Resolve the Pi agent dir for live model discovery unless the user pinned
    // it in config.toml. Tests construct Config directly (pi_agent_dir stays
    // None), so this never runs for them and the pi catalog stays static.
    if config.pi_agent_dir.is_none() {
        config.pi_agent_dir = board_core::pi_catalog::default_agent_dir();
    }
    let settings = DaemonSettings::load(&paths::config_path());
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
    if daemon.herdr.is_some() {
        let d = daemon.clone();
        std::thread::spawn(move || watchers::herdr_event_thread(d));
    }
    spawn_signal_handler(daemon.clone());

    // Startup adoption of runs that were active at the last shutdown/crash.
    adopt_runs(&daemon).await;
    daemon.wake_dispatch();

    // Bind the socket (removing any stale file first) and serve.
    let _ = std::fs::remove_file(&socket_path);
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = tokio::net::UnixListener::bind(&socket_path)?;
    tracing::info!("listening on {:?}", socket_path);

    server::serve(daemon.clone(), listener).await;

    // Graceful: leave running panes alone; just clean up the socket.
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

/// On startup, reconcile runs that were started but never ended.
async fn adopt_runs(d: &Arc<Daemon>) {
    let active = match d.store.active_runs() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("adoption: active_runs failed: {e}");
            return;
        }
    };
    for (run, card) in active {
        // Resolve the run's session socket so kill/liveness target the right
        // session after a restart (default session → None handle socket).
        let herdr_socket = d.session_registry.as_ref().and_then(|reg| {
            reg.resolve(run.session.as_deref())
                .ok()
                .filter(|r| Some(r.socket.as_path()) != Some(reg.default_socket()))
                .map(|r| r.socket)
        });
        let handle = SpawnHandle {
            pane_id: run.herdr_pane_id.clone(),
            workspace_id: run.herdr_workspace_id.clone(),
            pid: None,
            herdr_socket,
        };
        let alive = if handle.pane_id.is_some() {
            let spawner = d.spawner.clone();
            let h = handle.clone();
            tokio::task::spawn_blocking(move || spawner.is_alive(&h))
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or(false)
        } else {
            false
        };

        if alive {
            tracing::info!("adopting live run {} (card {})", run.id, card.id);
            let deadline = {
                let col = d.store.lock().get_column(run.column_id).ok().flatten();
                col.and_then(|c| c.timeout_minutes).map(|m| {
                    std::time::Instant::now()
                        + std::time::Duration::from_secs(
                            m.max(0) as u64 * d.settings.timeout_unit_secs,
                        )
                })
            };
            let mut sched = d.sched.lock().unwrap();
            sched.active.insert(
                run.id,
                crate::state::ActiveRun {
                    card_id: card.id,
                    handle,
                    started: std::time::Instant::now(),
                    timeout_deadline: deadline,
                    idle_since: None,
                    awaiting_since: None,
                    is_local: false,
                    pane_id: run.herdr_pane_id.clone(),
                },
            );
            drop(sched);
            d.refresh_watch();
        } else {
            tracing::info!("run {} (card {}) lost across restart", run.id, card.id);
            let msg = "daemon restart: run lost".to_string();
            let _ = dispatch::finalize_run(
                d,
                run.id,
                board_core::protocol::RunOutcome::Fail,
                Some(msg.clone()),
                Some(msg),
                false,
                false,
            );
        }
    }
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
