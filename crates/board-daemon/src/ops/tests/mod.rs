use super::*;
use crate::session::{SessionEntry, SessionRegistry};
use crate::testkit::{self, FakeHerdr};
use board_core::config::{Config, HarnessDef};
use board_core::db::{Db, EnqueueRun, FinalizeRun, LifecycleFaultPoint, BOARD_ID};
use std::collections::BTreeMap;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Mutex;
use tokio::sync::broadcast;

fn test_daemon(config: Config) -> Arc<Daemon> {
    test_daemon_with_registry(config, None)
}

/// A daemon whose spawner is a real `HerdrSpawner` on `socket`. The rescue takes
/// its per-card-tab allocation lock and tab memory from the spawner, so any test
/// that exercises those must not use the local spawner.
fn test_daemon_with_herdr_spawner(config: Config, socket: PathBuf) -> Arc<Daemon> {
    testkit::daemon()
        .config(config)
        .herdr_spawner(socket)
        .build_daemon()
}

fn test_daemon_with_registry(
    config: Config,
    session_registry: Option<SessionRegistry>,
) -> Arc<Daemon> {
    testkit::daemon()
        .config(config)
        .registry(session_registry)
        .build_daemon()
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

/// Create a finished first run and an open replacement run on the same pane,
/// exactly as a managed same-conversation resume hop appears durably. The live
/// process still carries `first.id` in BOARD_RUN_ID while HERDR_PANE_ID remains
/// `w1:p-shared` for the open run.
fn add_reused_pane_runs(d: &Arc<Daemon>) -> (i64, i64, i64) {
    let db = d.store.lock();
    let card = db
        .create_card(&CardCreateParams {
            title: "reused pane actor".into(),
            ..Default::default()
        })
        .unwrap();
    let enqueue = |prompt: &'static str| EnqueueRun {
        card_id: card.id,
        column_id: card.column_id,
        harness: "pi",
        argv_json: "[]",
        prompt_snapshot: prompt,
        system_prompt_snapshot: None,
        launch_spec_json: None,
        session_id: Some("conversation-1"),
        session: None,
    };
    let first = db.enqueue_run_uow(&enqueue("first")).unwrap();
    db.promote_run_uow(first.id, Some("w1"), Some("w1:p-shared"), None)
        .unwrap();
    db.finalize_run_uow(&FinalizeRun {
        run_id: first.id,
        outcome: RunOutcome::Ok,
        summary: Some("first stage done"),
        comments: &[],
        target_column_id: None,
        final_status: CardStatus::Done,
        final_awaiting_reason: None,
        next: None,
    })
    .unwrap();

    let current = db.enqueue_run_uow(&enqueue("current")).unwrap();
    db.promote_run_uow(current.id, Some("w1"), Some("w1:p-shared"), None)
        .unwrap();
    (card.id, first.id, current.id)
}

/// A fake Herdr for `run.focus`: `pane.get` reports the recorded pane as still
/// existing, and `pane.focus` answers with `reply` (a raw `"result":…` or
/// `"error":…` fragment). One reply per connection, like real herdr.
fn fake_herdr(reply: &'static str) -> FakeHerdr {
    fake_herdr_with_pane(reply, true)
}

/// `pane_exists == false` makes `pane.get` answer `pane_not_found`, i.e. the
/// run's recorded pane id is stale (its terminal was closed).
fn fake_herdr_with_pane(focus_reply: &'static str, pane_exists: bool) -> FakeHerdr {
    fake_herdr_inner(
        focus_reply,
        pane_exists,
        board_herdr::SUPPORTED_HERDR_PROTOCOL,
    )
}

/// A fake Herdr that answers the protocol gate with `protocol` and records
/// every method it is asked for, so a test can prove that a socket speaking the
/// wrong protocol is rejected *before* any other request reaches it.
fn fake_herdr_with_protocol(protocol: u32) -> FakeHerdr {
    fake_herdr_inner("\"result\":{\"type\":\"ok\"}", true, protocol)
}

fn fake_herdr_inner(focus_reply: &'static str, pane_exists: bool, protocol: u32) -> FakeHerdr {
    testkit::herdr_server()
        .protocol(protocol)
        .on("workspace.list", |req| {
            testkit::reply(req, json!({"workspaces": []}))
        })
        .on("pane.get", move |req| {
            if !pane_exists {
                return testkit::error(req, "pane_not_found", "pane not found");
            }
            let pane_id = req["params"]["pane_id"].as_str().unwrap();
            testkit::reply(
                req,
                json!({"type": "pane_info", "pane": {
                    "pane_id": pane_id, "terminal_id": "t1", "workspace_id": "w1",
                    "tab_id": "w1:t1", "focused": false, "agent_status": "unknown",
                    "revision": 0
                }}),
            )
        })
        .on("pane.focus", move |req| {
            // `focus_reply` is a raw `"result":…` / `"error":…` fragment, so a
            // test can hand-write an exact herdr answer.
            let fragment: Value = serde_json::from_str(&format!("{{{focus_reply}}}")).unwrap();
            let mut response = json!({"id": req["id"].clone()});
            match fragment.get("result") {
                Some(result) => response["result"] = result.clone(),
                None => response["error"] = fragment["error"].clone(),
            }
            response
        })
        .serve()
}

// ---------------------------------------------------------------------------
// Rescue fixture: a small stateful Herdr 0.8.0 / protocol 19 server for
// `run.focus` rescues
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
    db.promote_run_with_anchor_uow(
        run.id,
        Some("w1"),
        Some("w1:p9"),
        Some("w1:anchor"),
        None,
        None,
    )
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
    herdr: FakeHerdr,
    socket: PathBuf,
    /// Live panes, so tests can simulate a harness exiting inside one.
    panes: Arc<Mutex<Vec<FakePane>>>,
}

impl RescueFake {
    fn count(&self, method: &str) -> usize {
        self.herdr.count(method)
    }

    /// Every `agent.start` request, so tests can assert the resume argv.
    fn agent_starts(&self) -> Vec<Value> {
        self.herdr.requests_for("agent.start")
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
    /// Pane-first placement puts the run environment on
    /// `pane.split`, NOT on `agent.start`.
    fn last_split_env(&self) -> BTreeMap<String, String> {
        let splits = self.herdr.requests_for("pane.split");
        let last = splits.last().expect("a pane.split happened");
        serde_json::from_value(last["params"]["env"].clone()).unwrap_or_default()
    }
}

/// A stateful Herdr 0.8.0 / protocol 19 server covering the rescue path: the run's recorded
/// pane `w1:p9` is **absent** (its terminal was closed) while the card tab
/// `w1:t1` and its shell anchor `w1:anchor` are still alive. Panes created by
/// `pane.split` persist in this fake and are returned by `pane.list`, which is
/// what makes the idempotency test meaningful.
///
/// The tab's *label* is deliberately arbitrary here: board card-tab ownership is
/// resolved from the exact `tab_id` reconstructed from durable pane identity,
/// never from a label, so a fixture label cannot accidentally satisfy it.
fn fake_rescue_herdr(faults: RescueFakeFaults) -> RescueFake {
    let tab_label = "card-fixture".to_string();

    // (pane_id, tab_id, label, agent). The anchor survives its run's pane,
    // exactly as a real card tab's shell anchor does — unless the fault says the
    // whole tab is gone, in which case placement must create a new one. An
    // unrelated user pane (the workspace's own initial tab root, as a real
    // workspace keeps) is always present: it is what keeps the workspace cwd
    // lookup answerable once a successful managed rescue closes the card tab's
    // anchor.
    let initial: Vec<FakePane> = if faults.anchor_missing {
        vec![(
            "w1:foreign".to_string(),
            "w1:t9".to_string(),
            Some("someone-elses-pane".to_string()),
            None,
        )]
    } else {
        vec![
            (
                "w1:anchor".to_string(),
                "w1:t1".to_string(),
                Some(format!("{tab_label}-anchor")),
                None,
            ),
            (
                "w1:foreign".to_string(),
                "w1:t9".to_string(),
                Some("someone-elses-pane".to_string()),
                None,
            ),
        ]
    };
    let panes: Arc<Mutex<Vec<FakePane>>> = Arc::new(Mutex::new(initial));
    let panes2 = Arc::clone(&panes);
    let next_pane = Arc::new(Mutex::new(0_usize));
    let next_tab = Arc::new(Mutex::new(0_usize));

    let herdr = testkit::herdr_server()
        .handler(move |request, _index| {
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

            match request["method"].as_str().unwrap() {
                "pane.get" => {
                    let wanted = params["pane_id"].as_str().unwrap();
                    match snapshot().iter().find(|pane| pane.0 == wanted) {
                        Some(pane) => testkit::reply(
                            request,
                            json!({"type":"pane_info","pane":pane_json(pane)}),
                        ),
                        None => testkit::error(request, "pane_not_found", "pane not found"),
                    }
                }
                "pane.list" => {
                    let list: Vec<Value> = snapshot().iter().map(pane_json).collect();
                    testkit::reply(request, json!({"type":"pane_list","panes":list}))
                }
                "session.snapshot" => {
                    let list: Vec<Value> = snapshot().iter().map(pane_json).collect();
                    let tab_list: Vec<Value> =
                        tabs().iter().map(|tab| tab_json(tab, &tab_label)).collect();
                    testkit::reply(
                        request,
                        json!({"type":"session_snapshot","snapshot":{
                            "version":board_herdr::SUPPORTED_HERDR_VERSION,"protocol":board_herdr::SUPPORTED_HERDR_PROTOCOL,
                            "workspaces":[{"workspace_id":"w1","label":"ws","focused":true,
                                           "tab_count":1,"pane_count":1,"agent_status":"idle"}],
                            "tabs":tab_list,"panes":list,"agents":[]
                        }}),
                    )
                }
                "tab.list" => {
                    let tab_list: Vec<Value> =
                        tabs().iter().map(|tab| tab_json(tab, &tab_label)).collect();
                    testkit::reply(request, json!({"type":"tab_list","tabs":tab_list}))
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
                    testkit::reply(
                        request,
                        json!({"type":"tab_created","tab":tab_json(&tab_id,&label),
                               "root_pane":pane_json(&root)}),
                    )
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
                    testkit::reply(
                        request,
                        json!({"type":"pane_layout","layout":{
                            "workspace_id":"w1","tab_id":tab,"zoomed":false,
                            "area":{"x":0,"y":0,"width":120,"height":40},
                            "focused_pane_id":target,"panes":list,"splits":[]
                        }}),
                    )
                }
                "pane.split" if faults.split_fails => {
                    testkit::error(request, "pane_split_failed", "no room for another pane")
                }
                "pane.split" => {
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
                    testkit::reply(
                        request,
                        json!({"type":"pane_info","pane":pane_json(&child)}),
                    )
                }
                "pane.rename" => {
                    let target = params["pane_id"].as_str().unwrap().to_string();
                    let label = params["label"].as_str().map(str::to_string);
                    let mut guard = panes.lock().unwrap();
                    match guard.iter_mut().find(|pane| pane.0 == target) {
                        Some(pane) => {
                            pane.2 = label;
                            testkit::reply(
                                request,
                                json!({"type":"pane_info","pane":pane_json(pane)}),
                            )
                        }
                        None => testkit::error(request, "pane_not_found", "gone"),
                    }
                }
                "pane.close" => {
                    let target = params["pane_id"].as_str().unwrap().to_string();
                    panes.lock().unwrap().retain(|pane| pane.0 != target);
                    testkit::reply(request, json!({"type":"ok"}))
                }
                "pane.focus" => {
                    let target = params["pane_id"].as_str().unwrap().to_string();
                    match snapshot().iter().find(|pane| pane.0 == target) {
                        Some(pane) => testkit::reply(
                            request,
                            json!({"type":"pane_info","pane":pane_json(pane)}),
                        ),
                        None => testkit::error(request, "pane_not_found", "gone"),
                    }
                }
                "agent.start" if faults.agent_start_fails => {
                    testkit::error(request, "agent_start_failed", "harness refused to start")
                }
                "agent.start" => {
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
                    testkit::reply(
                        request,
                        json!({"type":"agent_started","agent":agent,"argv":[]}),
                    )
                }
                other => panic!("unexpected herdr method {other}"),
            }
        })
        .serve();

    let socket = herdr.socket.clone();
    RescueFake {
        herdr,
        socket,
        panes: panes2,
    }
}

mod boards;
mod cards;
mod comments;
mod discovery;
mod lifecycle;
mod panes;
mod parity;
mod rollback;
mod validation;
