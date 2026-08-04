//! Shared test infrastructure for the daemon's private `#[cfg(test)]` suites.
//!
//! Three things live here, each of which used to be copy-pasted across a dozen
//! sibling test modules:
//!
//! 1. [`daemon`] — one builder for the twelve-argument [`Daemon::new`].
//! 2. [`herdr_server`] — one fake Herdr 0.8.0 / protocol 19 Unix socket server,
//!    with a configurable protocol/version (so protocol-gate tests can serve a
//!    *wrong* one), per-method canned responses, an optional accept count, and
//!    recorded request inspection. The generic supported-contract JSON
//!    constructors used to build those responses live here too.
//! 3. The "nothing escaped" assertions and the armed lifecycle-fault `Db`.
//!
//! This module is compiled only under `cfg(test)`; nothing here is production
//! code.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, watch};

use board_core::config::Config;
use board_core::db::{Db, LifecycleFaultPoint};
use board_core::protocol::Event;
use board_core::Error;
use board_herdr::HerdrClient;

use crate::session::SessionRegistry;
use crate::settings::DaemonSettings;
use crate::spawner::{LocalSpawner, Spawner};
use crate::state::Daemon;
use crate::store::Store;

// ---------------------------------------------------------------------------
// 1. The daemon builder
// ---------------------------------------------------------------------------

/// A built test daemon plus the receiving ends of its two channels.
///
/// Holding the receivers is what lets a test assert that *no* event and *no*
/// dispatch wake escaped a rolled-back operation. Tests that do not care call
/// [`DaemonBuilder::build_daemon`], which drops them exactly like the
/// hand-rolled helpers this replaces did.
pub(crate) struct TestDaemon {
    pub daemon: Arc<Daemon>,
    pub events: broadcast::Receiver<Event>,
    pub dispatch: mpsc::UnboundedReceiver<()>,
}

/// Builder for a test [`Daemon`]. Defaults: an in-memory store, default config
/// and settings, a [`LocalSpawner`], no herdr client, no session registry, and
/// dummy `/tmp` db/socket paths.
pub(crate) struct DaemonBuilder {
    db: Option<Db>,
    config: Config,
    settings: DaemonSettings,
    spawner: Arc<dyn Spawner>,
    session_registry: Option<SessionRegistry>,
    herdr: Option<HerdrClient>,
    db_path: PathBuf,
    socket_path: PathBuf,
    events_capacity: usize,
}

/// Start building a test daemon.
pub(crate) fn daemon() -> DaemonBuilder {
    DaemonBuilder {
        db: None,
        config: Config::default(),
        settings: DaemonSettings::default(),
        spawner: Arc::new(LocalSpawner::new()),
        session_registry: None,
        herdr: None,
        db_path: PathBuf::from("/tmp/board-test.db"),
        socket_path: PathBuf::from("/tmp/board-test.sock"),
        events_capacity: 16,
    }
}

impl DaemonBuilder {
    /// Use an already-open `Db` (a file-backed one, or one carrying a
    /// lifecycle fault hook) instead of a fresh in-memory database.
    pub(crate) fn db(mut self, db: Db) -> Self {
        self.db = Some(db);
        self
    }

    pub(crate) fn config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    pub(crate) fn spawner(mut self, spawner: Arc<dyn Spawner>) -> Self {
        self.spawner = spawner;
        self
    }

    /// A real `HerdrSpawner` on `socket`, plus the matching session registry.
    /// The rescue path takes its per-card-tab allocation lock and tab memory
    /// from the spawner, so tests that exercise it must not use the local one.
    pub(crate) fn herdr_spawner(mut self, socket: PathBuf) -> Self {
        self.session_registry = Some(SessionRegistry::new(socket.clone()));
        self.spawner = Arc::new(crate::spawner::HerdrSpawner::new(socket));
        self
    }

    pub(crate) fn registry(mut self, registry: Option<SessionRegistry>) -> Self {
        self.session_registry = registry;
        self
    }

    pub(crate) fn herdr(mut self, herdr: HerdrClient) -> Self {
        self.herdr = Some(herdr);
        self
    }

    /// Point the daemon at a real on-disk database file. The socket path is
    /// derived from it unless [`DaemonBuilder::socket_path`] overrides it.
    pub(crate) fn db_path(mut self, path: PathBuf) -> Self {
        self.socket_path = path.with_extension("sock");
        self.db_path = path;
        self
    }

    pub(crate) fn socket_path(mut self, path: PathBuf) -> Self {
        self.socket_path = path;
        self
    }

    pub(crate) fn events_capacity(mut self, capacity: usize) -> Self {
        self.events_capacity = capacity;
        self
    }

    pub(crate) fn build(self) -> TestDaemon {
        let db = self.db.unwrap_or_else(|| Db::open_in_memory().unwrap());
        let (events_tx, events) = broadcast::channel(self.events_capacity);
        let (dispatch_tx, dispatch) = mpsc::unbounded_channel();
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let daemon = Arc::new(Daemon::new(
            Store::new(db),
            self.config,
            self.settings,
            self.db_path,
            self.socket_path,
            self.spawner,
            self.herdr,
            self.session_registry,
            events_tx,
            dispatch_tx,
            shutdown_tx,
        ));
        TestDaemon {
            daemon,
            events,
            dispatch,
        }
    }

    /// Build and drop both receivers (the common case).
    pub(crate) fn build_daemon(self) -> Arc<Daemon> {
        self.build().daemon
    }

    /// Build and return `(daemon, events, dispatch)`.
    pub(crate) fn build_parts(
        self,
    ) -> (
        Arc<Daemon>,
        broadcast::Receiver<Event>,
        mpsc::UnboundedReceiver<()>,
    ) {
        let built = self.build();
        (built.daemon, built.events, built.dispatch)
    }
}

// ---------------------------------------------------------------------------
// 2. The fake Herdr socket server
// ---------------------------------------------------------------------------

type MethodHandler = Box<dyn Fn(&Value) -> Value + Send + Sync>;
type IndexedHandler = Box<dyn Fn(&Value, usize) -> Value + Send + Sync>;

/// A running fake Herdr on a private Unix socket.
pub(crate) struct FakeHerdr {
    _dir: tempfile::TempDir,
    pub socket: PathBuf,
    /// Every request received, in order — including the `ping` protocol gate.
    pub requests: Arc<Mutex<Vec<Value>>>,
}

impl FakeHerdr {
    /// Every requested method name, in order. Protocol-gate tests assert this
    /// is exactly `["ping"]` when a wrong protocol must stop everything else.
    pub(crate) fn methods(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|req| req["method"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// How many times `method` was requested.
    pub(crate) fn count(&self, method: &str) -> usize {
        self.methods()
            .iter()
            .filter(|m| m.as_str() == method)
            .count()
    }

    /// Every recorded request for `method`, in order.
    pub(crate) fn requests_for(&self, method: &str) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|req| req["method"].as_str() == Some(method))
            .cloned()
            .collect()
    }
}

/// Builder for [`FakeHerdr`].
pub(crate) struct FakeHerdrBuilder {
    version: String,
    protocol: u32,
    take: Option<usize>,
    by_method: HashMap<String, MethodHandler>,
    fallback: Option<IndexedHandler>,
}

/// Start building a fake Herdr. It answers the `ping` protocol gate itself
/// (the supported Herdr release/protocol by default) and records every request.
pub(crate) fn herdr_server() -> FakeHerdrBuilder {
    FakeHerdrBuilder {
        version: board_herdr::SUPPORTED_HERDR_VERSION.to_string(),
        protocol: board_herdr::SUPPORTED_HERDR_PROTOCOL,
        take: None,
        by_method: HashMap::new(),
        fallback: None,
    }
}

impl FakeHerdrBuilder {
    /// Serve a different protocol number in the `pong` — the protocol-gate
    /// tests depend on being able to serve a wrong one.
    pub(crate) fn protocol(mut self, protocol: u32) -> Self {
        self.protocol = protocol;
        self
    }

    /// Serve a different Herdr version string in the `pong`.
    pub(crate) fn version(mut self, version: &str) -> Self {
        self.version = version.to_string();
        self
    }

    /// Accept at most `n` connections (herdr serves one request per
    /// connection), then stop. Some fixtures are deliberately exhaustible so a
    /// test proves nothing more was asked of them.
    pub(crate) fn take(mut self, n: usize) -> Self {
        self.take = Some(n);
        self
    }

    /// A canned response for one method. The closure returns the full
    /// response envelope — build it with [`reply`] or [`error`].
    pub(crate) fn on<F>(mut self, method: &str, handler: F) -> Self
    where
        F: Fn(&Value) -> Value + Send + Sync + 'static,
    {
        self.by_method.insert(method.to_string(), Box::new(handler));
        self
    }

    /// Handle every method without a canned response. The `usize` is the
    /// zero-based index of this non-`ping` request, so a fixture can serve a
    /// different answer to the first and second attempt.
    pub(crate) fn handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(&Value, usize) -> Value + Send + Sync + 'static,
    {
        self.fallback = Some(Box::new(handler));
        self
    }

    pub(crate) fn serve(self) -> FakeHerdr {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("herdr.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);

        let FakeHerdrBuilder {
            version,
            protocol,
            take,
            by_method,
            fallback,
        } = self;

        thread::spawn(move || {
            let incoming: Box<dyn Iterator<Item = std::io::Result<UnixStream>>> = match take {
                Some(n) => Box::new(listener.incoming().take(n)),
                None => Box::new(listener.incoming()),
            };
            let mut index = 0_usize;
            for conn in incoming {
                let Ok(stream) = conn else { break };
                let mut writer = stream.try_clone().unwrap();
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    // `HerdrClient::connect`'s liveness probe sends no request.
                    continue;
                }
                let request: Value = serde_json::from_str(line.trim()).unwrap();
                recorded.lock().unwrap().push(request.clone());
                let method = request["method"].as_str().unwrap_or_default().to_string();
                let response = if let Some(handler) = by_method.get(&method) {
                    handler(&request)
                } else if method == "ping" {
                    reply(
                        &request,
                        json!({
                            "type": "pong", "version": version,
                            "protocol": protocol, "capabilities": {}
                        }),
                    )
                } else if let Some(handler) = &fallback {
                    let response = handler(&request, index);
                    index += 1;
                    response
                } else {
                    panic!("unexpected herdr method {method}");
                };
                writeln!(writer, "{response}").unwrap();
                let _ = writer.flush();
            }
        });

        FakeHerdr {
            _dir: dir,
            socket,
            requests,
        }
    }
}

// ---------------------------------------------------------------------------
// Generic supported-contract JSON constructors
// ---------------------------------------------------------------------------

/// A successful response envelope for `req`.
pub(crate) fn reply(req: &Value, result: Value) -> Value {
    json!({"id": req["id"].clone(), "result": result})
}

/// An error response envelope for `req`.
pub(crate) fn error(req: &Value, code: &str, message: &str) -> Value {
    json!({
        "id": req["id"].clone(),
        "error": {"code": code, "message": message}
    })
}

/// Minimal schema-valid `PaneInfo` fixture. In particular,
/// `focused` and `revision` are required by the authoritative schema.
pub(crate) fn pane_info(id: &str) -> Value {
    json!({
        "pane_id": id,
        "terminal_id": format!("term-{id}"),
        "workspace_id": "w1",
        "tab_id": "w1:t1",
        "focused": false,
        "agent_status": "unknown",
        "revision": 1
    })
}

/// A `PaneInfo` extended with the agent fields Herdr adds for a managed pane.
pub(crate) fn agent_info(pane_id: &str, name: &str, pending: bool, ready: bool) -> Value {
    let mut agent = pane_info(pane_id);
    agent["name"] = Value::String(name.into());
    agent["launch_pending"] = Value::Bool(pending);
    agent["interactive_ready"] = Value::Bool(ready);
    agent
}

/// A `tab.create` reply whose root pane is `root_pane`.
pub(crate) fn tab_created(req: &Value, root_pane: &str) -> Value {
    reply(
        req,
        json!({
            "type": "tab_created",
            "tab": {
                "tab_id": "w1:t1", "workspace_id": "w1", "number": 1,
                "label": "kanban", "focused": false, "pane_count": 1,
                "agent_status": "unknown"
            },
            "root_pane": pane_info(root_pane)
        }),
    )
}

/// An `agent.start` reply echoing the request's `kind`/`args` as the argv.
pub(crate) fn agent_started(req: &Value, pane_id: &str, pending: bool, ready: bool) -> Value {
    let name = req["params"]["name"].as_str().unwrap();
    let mut argv = vec![Value::String(
        req["params"]["kind"].as_str().unwrap().into(),
    )];
    argv.extend(req["params"]["args"].as_array().unwrap().iter().cloned());
    reply(
        req,
        json!({
            "type": "agent_started",
            "agent": agent_info(pane_id, name, pending, ready),
            "argv": argv
        }),
    )
}

// ---------------------------------------------------------------------------
// 3. Shared assertions and the armed lifecycle fault hook
// ---------------------------------------------------------------------------

/// No board event escaped.
pub(crate) fn assert_no_events(events: &mut broadcast::Receiver<Event>) {
    assert!(
        matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ),
        "an event escaped a rolled-back operation"
    );
}

/// Nothing escaped: neither a board event nor a dispatch wake.
pub(crate) fn assert_no_effects(
    events: &mut broadcast::Receiver<Event>,
    dispatch: &mut mpsc::UnboundedReceiver<()>,
) {
    assert_no_events(events);
    assert!(
        matches!(dispatch.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "a dispatch wake escaped a rolled-back operation"
    );
}

/// A rolled-back *started* run: nothing escaped, the handle was not killed,
/// and the run stays in the in-memory active set.
pub(crate) fn assert_no_rollback_effects(
    d: &Arc<Daemon>,
    events: &mut broadcast::Receiver<Event>,
    dispatch: &mut mpsc::UnboundedReceiver<()>,
    kills: &AtomicUsize,
    run_id: i64,
) {
    assert_eq!(kills.load(Ordering::SeqCst), 0);
    assert!(d.sched.lock().unwrap().active.contains_key(&run_id));
    assert_no_effects(events, dispatch);
}

/// The arming switch of a [`fault_db`] hook.
pub(crate) struct FaultSwitch {
    armed: Arc<AtomicBool>,
    observed: Arc<AtomicBool>,
}

impl FaultSwitch {
    /// Arm the fault. Setup before this point runs against a healthy database.
    pub(crate) fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    /// Whether the armed fault point was actually reached.
    pub(crate) fn observed(&self) -> bool {
        self.observed.load(Ordering::SeqCst)
    }
}

/// A file-backed `Db` that fails with `message` at `point` once armed.
pub(crate) fn fault_db(
    path: &Path,
    point: LifecycleFaultPoint,
    message: &'static str,
) -> (Db, FaultSwitch) {
    let armed = Arc::new(AtomicBool::new(false));
    let observed = Arc::new(AtomicBool::new(false));
    let hook_armed = Arc::clone(&armed);
    let hook_observed = Arc::clone(&observed);
    let db = Db::open_with_lifecycle_fault_hook(path, move |reached| {
        if hook_armed.load(Ordering::SeqCst) && reached == point {
            hook_observed.store(true, Ordering::SeqCst);
            return Err(Error::InvalidState(message.into()));
        }
        Ok(())
    })
    .unwrap();
    (db, FaultSwitch { armed, observed })
}
