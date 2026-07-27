use super::*;
use crate::session::SessionRegistry;
use crate::settings::DaemonSettings;
use crate::spawner::LocalSpawner;
use crate::store::Store;
use board_core::config::{Config, HarnessDef};
use board_core::db::{Db, EnqueueRun, FinalizeRun, LifecycleFaultPoint, BOARD_ID};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use tokio::sync::{broadcast, mpsc, watch};

fn test_daemon(config: Config) -> Arc<Daemon> {
    test_daemon_with_registry(config, None)
}

/// A daemon whose spawner is a real `HerdrSpawner` on `socket`. The rescue takes
/// its per-card-tab allocation lock and tab memory from the spawner, so any test
/// that exercises those must not use the local spawner.
fn test_daemon_with_herdr_spawner(config: Config, socket: PathBuf) -> Arc<Daemon> {
    test_daemon_inner(
        config,
        Some(SessionRegistry::new(socket.clone())),
        Arc::new(crate::spawner::HerdrSpawner::new(socket)),
    )
}

fn test_daemon_with_registry(
    config: Config,
    session_registry: Option<SessionRegistry>,
) -> Arc<Daemon> {
    test_daemon_inner(config, session_registry, Arc::new(LocalSpawner::new()))
}

fn test_daemon_inner(
    config: Config,
    session_registry: Option<SessionRegistry>,
    spawner: Arc<dyn crate::spawner::Spawner>,
) -> Arc<Daemon> {
    let db = Db::open_in_memory().unwrap();
    let store = Store::new(db);
    let (events_tx, _events_rx) = broadcast::channel(16);
    let (dispatch_tx, _dispatch_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    Arc::new(Daemon::new(
        store,
        config,
        DaemonSettings::default(),
        PathBuf::from("/tmp/board-test.db"),
        PathBuf::from("/tmp/board-test.sock"),
        spawner,
        None, // no herdr
        session_registry,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ))
}

/// Create a card with one promoted run and return `(card_id, run_id)`.
fn add_run_with_pane(d: &Arc<Daemon>, pane: Option<&str>) -> (i64, i64) {
    let db = d.store.lock();
    let card = db
        .create_card(&CardCreateParams {
            title: "focus target".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "p",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    db.promote_run_uow(run.id, Some("w1"), pane, None).unwrap();
    (card.id, run.id)
}

/// A fake Herdr for `run.focus`: `pane.get` reports the recorded pane as still
/// existing, and `pane.focus` answers with `reply` (a raw `"result":…` or
/// `"error":…` fragment). One reply per connection, like real herdr.
fn fake_herdr(reply: &'static str) -> (tempfile::TempDir, PathBuf) {
    fake_herdr_with_pane(reply, true)
}

/// `pane_exists == false` makes `pane.get` answer `pane_not_found`, i.e. the
/// run's recorded pane id is stale (its terminal was closed).
fn fake_herdr_with_pane(
    focus_reply: &'static str,
    pane_exists: bool,
) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let stream = incoming.unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                // `HerdrClient::connect`'s liveness probe sends no request.
                continue;
            }
            let request: Value = serde_json::from_str(line.trim()).unwrap();
            let id = request["id"].as_str().unwrap();
            let reply = match request["method"].as_str().unwrap() {
                "pane.get" if pane_exists => {
                    let pane_id = request["params"]["pane_id"].as_str().unwrap();
                    format!(
                        "\"result\":{{\"type\":\"pane_info\",\"pane\":{{\"pane_id\":\"{pane_id}\",\
                         \"terminal_id\":\"t1\",\"workspace_id\":\"w1\",\"tab_id\":\"w1:t1\",\
                         \"focused\":false,\"agent_status\":\"unknown\",\"revision\":0}}}}"
                    )
                }
                "pane.get" => {
                    "\"error\":{\"code\":\"pane_not_found\",\"message\":\"pane not found\"}"
                        .to_string()
                }
                "pane.focus" => focus_reply.to_string(),
                other => panic!("unexpected herdr method {other}"),
            };
            writeln!(writer, "{{\"id\":\"{id}\",{reply}}}").unwrap();
        }
    });
    (dir, path)
}

// ---------------------------------------------------------------------------
// Rescue fixture: a small stateful protocol-17 Herdr for `run.focus` rescues
// ---------------------------------------------------------------------------

/// Create a card plus one *finished* run that recorded a dead pane, a live
/// card-tab anchor, a durable launch spec and a harness conversation id — i.e.
/// exactly the shape a rescue needs. Returns `(card_id, run_id)`.
fn add_rescuable_run(
    d: &Arc<Daemon>,
    harness: &str,
    agent_kind: Option<&str>,
    session_id: Option<&str>,
    with_launch_spec: bool,
) -> (i64, i64) {
    let db = d.store.lock();
    let card = db
        .create_card(&CardCreateParams {
            title: "rescue target".into(),
            ..Default::default()
        })
        .unwrap();
    let launch_spec = with_launch_spec.then(|| {
        serde_json::to_string(&board_core::launch::RunLaunchSpec::v1(
            board_core::launch::ExecutionSpec {
                argv: vec![
                    harness.to_string(),
                    "--model".into(),
                    "recorded-model".into(),
                    if harness == "claude" {
                        "--resume".into()
                    } else {
                        "--session-id".into()
                    },
                    "conv-1".into(),
                ],
                env: vec![
                    ("BOARD_PROMPT".into(), "the original task".into()),
                    ("RECORDED_ENV".into(), "recorded-value".into()),
                ],
                agent_kind: agent_kind.map(str::to_string),
                initial_prompt: Some("the original task".into()),
                system_prompt: Some("recorded system prompt".into()),
            },
        ))
        .unwrap()
    });
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness,
            argv_json: "[]",
            prompt_snapshot: "the original task",
            system_prompt_snapshot: Some("recorded system prompt"),
            launch_spec_json: launch_spec.as_deref(),
            session_id,
            session: None,
        })
        .unwrap();
    db.promote_run_with_anchor_uow(run.id, Some("w1"), Some("w1:p9"), Some("w1:anchor"), None)
        .unwrap();
    // A rescue must work on a *finished* run: the row below is exactly what the
    // tests then assert stays untouched.
    db.finalize_run_uow(&FinalizeRun {
        run_id: run.id,
        outcome: RunOutcome::Ok,
        summary: Some("done"),
        comments: &[],
        target_column_id: None,
        final_status: CardStatus::Done,
        final_awaiting_reason: None,
        next: None,
    })
    .unwrap();
    (card.id, run.id)
}

/// `(pane_id, tab_id, label, agent_name)` for one pane in the rescue fake.
type FakePane = (String, String, Option<String>, Option<String>);

/// Knobs for [`fake_rescue_herdr`].
#[derive(Clone, Copy, Default)]
struct RescueFakeFaults {
    /// `pane.split` refuses, i.e. the new pane cannot be created.
    split_fails: bool,
    /// `agent.start` refuses, i.e. the harness will not start in the new pane.
    agent_start_fails: bool,
    /// The card tab (and its shell anchor) are gone too, so nothing can prove a
    /// tab and placement has to `tab.create` a fresh one. An unrelated user pane
    /// remains in the workspace, which is what a real closed card tab looks like
    /// and what keeps the workspace cwd lookup answerable.
    anchor_missing: bool,
}

/// Observable, pokeable state of the rescue fake.
struct RescueFake {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    /// Every request method, in order.
    methods: Arc<Mutex<Vec<String>>>,
    /// Every `agent.start` request, so tests can assert the resume argv.
    agent_starts: Arc<Mutex<Vec<Value>>>,
    /// Every `pane.split` request. Protocol-17 placement is pane-first, so the
    /// run environment arrives here, NOT on `agent.start`.
    pane_splits: Arc<Mutex<Vec<Value>>>,
    /// Live panes, so tests can simulate a harness exiting inside one.
    panes: Arc<Mutex<Vec<FakePane>>>,
}

impl RescueFake {
    fn count(&self, method: &str) -> usize {
        self.methods
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.as_str() == method)
            .count()
    }

    fn pane_ids(&self) -> Vec<String> {
        self.panes
            .lock()
            .unwrap()
            .iter()
            .map(|(id, _, _, _)| id.clone())
            .collect()
    }

    /// Simulate the resumed harness exiting: Herdr drops the pane's `agent`
    /// while the pane and its label survive as a plain shell.
    fn drop_agent(&self, pane_id: &str) {
        let mut guard = self.panes.lock().unwrap();
        let pane = guard
            .iter_mut()
            .find(|(id, _, _, _)| id == pane_id)
            .expect("pane exists");
        pane.3 = None;
    }

    /// The env of the last `pane.split`, i.e. what the rescued pane received.
    fn last_split_env(&self) -> BTreeMap<String, String> {
        let splits = self.pane_splits.lock().unwrap();
        let last = splits.last().expect("a pane.split happened");
        serde_json::from_value(last["params"]["env"].clone()).unwrap_or_default()
    }
}

/// A stateful protocol-17 Herdr covering the rescue path: the run's recorded
/// pane `w1:p9` is **absent** (its terminal was closed) while the card tab
/// `w1:t1` and its shell anchor `w1:anchor` are still alive. Panes created by
/// `pane.split` persist in this fake and are returned by `pane.list`, which is
/// what makes the idempotency test meaningful.
///
/// The tab's *label* is deliberately arbitrary here: board card-tab ownership is
/// resolved from the exact `tab_id` reconstructed from durable pane identity,
/// never from a label, so a fixture label cannot accidentally satisfy it.
fn fake_rescue_herdr(faults: RescueFakeFaults) -> RescueFake {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let methods = Arc::new(Mutex::new(Vec::new()));
    let agent_starts = Arc::new(Mutex::new(Vec::new()));
    let pane_splits = Arc::new(Mutex::new(Vec::new()));
    let methods2 = Arc::clone(&methods);
    let agent_starts2 = Arc::clone(&agent_starts);
    let pane_splits2 = Arc::clone(&pane_splits);
    let tab_label = "card-fixture".to_string();

    // (pane_id, tab_id, label, agent). The anchor survives its run's pane,
    // exactly as a real card tab's shell anchor does — unless the fault says the
    // whole tab is gone, in which case placement must create a new one.
    let initial: Vec<FakePane> = if faults.anchor_missing {
        vec![(
            "w1:foreign".to_string(),
            "w1:t9".to_string(),
            Some("someone-elses-pane".to_string()),
            None,
        )]
    } else {
        vec![(
            "w1:anchor".to_string(),
            "w1:t1".to_string(),
            Some(format!("{tab_label}-anchor")),
            None,
        )]
    };
    let panes: Arc<Mutex<Vec<FakePane>>> = Arc::new(Mutex::new(initial));
    let panes2 = Arc::clone(&panes);
    let next_pane = Arc::new(Mutex::new(0_usize));
    let next_tab = Arc::new(Mutex::new(0_usize));

    thread::spawn(move || {
        let pane_json = |pane: &FakePane| {
            json!({
                "pane_id": pane.0, "terminal_id": format!("term-{}", pane.0),
                "workspace_id": "w1", "tab_id": pane.1,
                "label": pane.2, "agent": pane.3,
                "cwd": "/tmp/rescue-cwd",
                "focused": false, "agent_status": "idle", "revision": 1
            })
        };
        let tab_json = |tab_id: &str, label: &str| {
            json!({
                "tab_id": tab_id, "workspace_id": "w1", "number": 1, "label": label,
                "focused": true, "pane_count": 1, "agent_status": "idle"
            })
        };
        for incoming in listener.incoming() {
            let Ok(stream) = incoming else { break };
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                // `HerdrClient::connect`'s liveness probe sends no request.
                continue;
            }
            let request: Value = serde_json::from_str(line.trim()).unwrap();
            let id = request["id"].as_str().unwrap().to_string();
            let method = request["method"].as_str().unwrap().to_string();
            methods2.lock().unwrap().push(method.clone());
            let params = request["params"].clone();
            let snapshot = || panes.lock().unwrap().clone();
            let tabs = || {
                let mut ids: Vec<String> = snapshot()
                    .into_iter()
                    .map(|pane| pane.1)
                    .collect::<Vec<_>>();
                ids.sort();
                ids.dedup();
                ids
            };

            let body = match method.as_str() {
                "ping" => json!({"type":"pong","version":"0.7.5","protocol":17,"capabilities":{}}),
                "pane.get" => {
                    let wanted = params["pane_id"].as_str().unwrap();
                    match snapshot().iter().find(|pane| pane.0 == wanted) {
                        Some(pane) => json!({"type":"pane_info","pane":pane_json(pane)}),
                        None => {
                            json!({"__error":{"code":"pane_not_found","message":"pane not found"}})
                        }
                    }
                }
                "pane.list" => {
                    let list: Vec<Value> = snapshot().iter().map(pane_json).collect();
                    json!({"type":"pane_list","panes":list})
                }
                "session.snapshot" => {
                    let list: Vec<Value> = snapshot().iter().map(pane_json).collect();
                    let tab_list: Vec<Value> =
                        tabs().iter().map(|tab| tab_json(tab, &tab_label)).collect();
                    json!({"type":"session_snapshot","snapshot":{
                        "version":"0.7.5","protocol":17,
                        "workspaces":[{"workspace_id":"w1","label":"ws","focused":true,
                                       "tab_count":1,"pane_count":1,"agent_status":"idle"}],
                        "tabs":tab_list,"panes":list,"agents":[]
                    }})
                }
                "tab.list" => {
                    let tab_list: Vec<Value> =
                        tabs().iter().map(|tab| tab_json(tab, &tab_label)).collect();
                    json!({"type":"tab_list","tabs":tab_list})
                }
                "tab.create" => {
                    let mut counter = next_tab.lock().unwrap();
                    *counter += 1;
                    let tab_id = format!("w1:newtab{counter}");
                    let label = params["label"].as_str().unwrap_or("card-new").to_string();
                    let root = (
                        format!("w1:root{counter}"),
                        tab_id.clone(),
                        Some(label.clone()),
                        None,
                    );
                    panes.lock().unwrap().push(root.clone());
                    json!({"type":"tab_created","tab":tab_json(&tab_id,&label),
                           "root_pane":pane_json(&root)})
                }
                "pane.layout" => {
                    // Every pane in the target's tab, so a split target resolves.
                    let target = params["pane_id"].as_str().unwrap_or_default().to_string();
                    let all = snapshot();
                    let tab = all
                        .iter()
                        .find(|pane| pane.0 == target)
                        .map(|pane| pane.1.clone())
                        .unwrap_or_else(|| "w1:t1".to_string());
                    let list: Vec<Value> = all
                        .iter()
                        .filter(|pane| pane.1 == tab)
                        .map(|pane| {
                            json!({
                                "pane_id": pane.0, "focused": false,
                                "rect": {"x":0,"y":0,"width":120,"height":40}
                            })
                        })
                        .collect();
                    json!({"type":"pane_layout","layout":{
                        "workspace_id":"w1","tab_id":tab,"zoomed":false,
                        "area":{"x":0,"y":0,"width":120,"height":40},
                        "focused_pane_id":target,"panes":list,"splits":[]
                    }})
                }
                "pane.split" if faults.split_fails => json!({"__error":{
                    "code":"pane_split_failed","message":"no room for another pane"
                }}),
                "pane.split" => {
                    pane_splits2.lock().unwrap().push(request.clone());
                    let target = params["target_pane_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let mut counter = next_pane.lock().unwrap();
                    *counter += 1;
                    let mut guard = panes.lock().unwrap();
                    let tab = guard
                        .iter()
                        .find(|pane| pane.0 == target)
                        .map(|pane| pane.1.clone())
                        .unwrap_or_else(|| "w1:t1".to_string());
                    let child = (format!("w1:rescued{counter}"), tab, None, None);
                    guard.push(child.clone());
                    json!({"type":"pane_info","pane":pane_json(&child)})
                }
                "pane.rename" => {
                    let target = params["pane_id"].as_str().unwrap().to_string();
                    let label = params["label"].as_str().map(str::to_string);
                    let mut guard = panes.lock().unwrap();
                    match guard.iter_mut().find(|pane| pane.0 == target) {
                        Some(pane) => {
                            pane.2 = label;
                            json!({"type":"pane_info","pane":pane_json(pane)})
                        }
                        None => json!({"__error":{"code":"pane_not_found","message":"gone"}}),
                    }
                }
                "pane.close" => {
                    let target = params["pane_id"].as_str().unwrap().to_string();
                    panes.lock().unwrap().retain(|pane| pane.0 != target);
                    json!({"type":"ok"})
                }
                "pane.focus" => {
                    let target = params["pane_id"].as_str().unwrap().to_string();
                    match snapshot().iter().find(|pane| pane.0 == target) {
                        Some(pane) => json!({"type":"pane_info","pane":pane_json(pane)}),
                        None => json!({"__error":{"code":"pane_not_found","message":"gone"}}),
                    }
                }
                "agent.start" if faults.agent_start_fails => json!({"__error":{
                    "code":"agent_start_failed","message":"harness refused to start"
                }}),
                "agent.start" => {
                    agent_starts2.lock().unwrap().push(request.clone());
                    let target = params["pane_id"].as_str().unwrap().to_string();
                    let name = params["name"].as_str().unwrap().to_string();
                    let mut guard = panes.lock().unwrap();
                    let Some(pane) = guard.iter_mut().find(|pane| pane.0 == target) else {
                        panic!("agent.start on an unknown pane {target}");
                    };
                    // Real Herdr reports the agent *kind* here, not the
                    // exclusive `name` we chose: the pinned schema gives
                    // `AgentInfo` both an `agent` and a separate `name`, and
                    // `e2e/16-managed-p17.sh` matches `pane.agent` against
                    // `pi`/`claude`. Mirroring that keeps the dedup tests from
                    // passing on a false premise about this field.
                    pane.3 = params["kind"].as_str().map(str::to_string);
                    let mut agent = pane_json(pane);
                    agent["name"] = Value::String(name);
                    agent["launch_pending"] = Value::Bool(false);
                    agent["interactive_ready"] = Value::Bool(true);
                    json!({"type":"agent_started","agent":agent,"argv":[]})
                }
                other => panic!("unexpected herdr method {other}"),
            };

            let response = match body.get("__error") {
                Some(error) => json!({"id": id, "error": error}),
                None => json!({"id": id, "result": body}),
            };
            writeln!(writer, "{response}").unwrap();
            let _ = writer.flush();
        }
    });

    RescueFake {
        _dir: dir,
        socket,
        methods,
        agent_starts,
        pane_splits,
        panes: panes2,
    }
}

mod boards;
mod cards;
mod comments;
mod discovery;
mod lifecycle;
mod validation;
