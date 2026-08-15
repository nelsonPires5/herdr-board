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
    add_rescuable_run_with_space(d, harness, agent_kind, session_id, with_launch_spec, None)
}

/// Like [`add_rescuable_run`], but with the card's space config set to
/// `space` — the current config a rescue falls back to when the run's
/// recorded workspace is gone.
fn add_rescuable_run_with_space(
    d: &Arc<Daemon>,
    harness: &str,
    agent_kind: Option<&str>,
    session_id: Option<&str>,
    with_launch_spec: bool,
    space: Option<(SpaceKind, String, String)>,
) -> (i64, i64) {
    let db = d.store.lock();
    let card = db
        .create_card(&CardCreateParams {
            title: "rescue target".into(),
            space_kind: space.as_ref().map(|(kind, _, _)| *kind),
            space_ref: space.as_ref().map(|(_, reference, _)| reference.clone()),
            space_cwd: space.as_ref().map(|(_, _, cwd)| cwd.clone()),
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
/// The workspace is the `workspace_id:` prefix of the pane/tab ids, exactly as
/// real Herdr scopes pane ids.
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
    /// The run's recorded workspace `w1` is closed entirely: it is absent from
    /// `workspace.list`/`session.snapshot` and has no panes, so the rescue's
    /// recorded-workspace probe fails and the card's current space config must
    /// supply a replacement. `workspace.create` mints fresh workspaces (`w2`,
    /// `w3`, …) with an initial tab/root, exactly like a real created workspace.
    workspace_gone: bool,
    /// The recorded workspace's live panes disagree on their cwd (a
    /// heterogeneous workspace): the strict cwd probe fails, but an explicit
    /// `space_cwd` on the card can still address the SAME workspace.
    multi_cwd: bool,
}

/// Observable, pokeable state of the rescue fake.
struct RescueFake {
    herdr: FakeHerdr,
    socket: PathBuf,
    /// Live panes, so tests can simulate a harness exiting inside one.
    panes: Arc<Mutex<Vec<FakePane>>>,
    /// Open workspaces `(id, label)`.
    workspaces: Arc<Mutex<Vec<(String, String)>>>,
}

impl RescueFake {
    fn count(&self, method: &str) -> usize {
        self.herdr.count(method)
    }

    /// Every `agent.start` request, so tests can assert the resume argv.
    fn agent_starts(&self) -> Vec<Value> {
        self.herdr.requests_for("agent.start")
    }

    /// Every `workspace.create` request, so tests can assert the replacement
    /// workspace's label and cwd.
    fn workspace_creates(&self) -> Vec<Value> {
        self.herdr.requests_for("workspace.create")
    }

    fn pane_ids(&self) -> Vec<String> {
        self.panes
            .lock()
            .unwrap()
            .iter()
            .map(|(id, _, _, _)| id.clone())
            .collect()
    }

    fn workspace_ids(&self) -> Vec<String> {
        self.workspaces
            .lock()
            .unwrap()
            .iter()
            .map(|(id, _)| id.clone())
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
    // anchor. With `workspace_gone` the recorded workspace `w1` is closed
    // entirely: no workspace, no panes — the rescue must replace it from the
    // card's space config.
    let (initial_workspaces, initial): (Vec<(String, String)>, Vec<FakePane>) =
        if faults.workspace_gone {
            (Vec::new(), Vec::new())
        } else if faults.anchor_missing {
            (
                vec![("w1".to_string(), "ws".to_string())],
                vec![(
                    "w1:foreign".to_string(),
                    "w1:t9".to_string(),
                    Some("someone-elses-pane".to_string()),
                    None,
                )],
            )
        } else {
            (
                vec![("w1".to_string(), "ws".to_string())],
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
                ],
            )
        };
    let panes: Arc<Mutex<Vec<FakePane>>> = Arc::new(Mutex::new(initial));
    let panes2 = Arc::clone(&panes);
    let workspaces: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(initial_workspaces));
    let workspaces2 = Arc::clone(&workspaces);
    // Tab labels, keyed by tab id. Real Herdr keeps them per tab; the rescue
    // path never reads them back, but the created-workspace adoption renames
    // tabs, so the fake answers faithfully.
    let tab_labels: Arc<Mutex<std::collections::HashMap<String, String>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    let next_pane = Arc::new(Mutex::new(0_usize));
    let next_tab = Arc::new(Mutex::new(0_usize));
    let next_workspace = Arc::new(Mutex::new(1_usize));

    let herdr = testkit::herdr_server()
        .handler(move |request, _index| {
            let ws_of = |id: &str| id.split(':').next().unwrap_or("w1").to_string();
            let pane_json = |pane: &FakePane| {
                let cwd = if faults.multi_cwd && pane.0.contains("foreign") {
                    "/tmp/other-cwd"
                } else {
                    "/tmp/rescue-cwd"
                };
                json!({
                    "pane_id": pane.0, "terminal_id": format!("term-{}", pane.0),
                    "workspace_id": ws_of(&pane.0), "tab_id": pane.1,
                    "label": pane.2, "agent": pane.3,
                    "cwd": cwd,
                    "focused": false, "agent_status": "idle", "revision": 1
                })
            };
            let tab_json = |tab_id: &str, label: &str| {
                json!({
                    "tab_id": tab_id, "workspace_id": ws_of(tab_id), "number": 1,
                    "label": label, "focused": true, "pane_count": 1,
                    "agent_status": "idle"
                })
            };
            let tab_label_for = |tab_id: &str| {
                tab_labels
                    .lock()
                    .unwrap()
                    .get(tab_id)
                    .cloned()
                    .unwrap_or_else(|| tab_label.clone())
            };
            let params = request["params"].clone();
            let snapshot = || panes.lock().unwrap().clone();
            let panes_in = |workspace_id: Option<&str>| {
                snapshot()
                    .into_iter()
                    .filter(|pane| {
                        workspace_id.is_none_or(|ws| ws_of(&pane.0) == ws)
                    })
                    .collect::<Vec<_>>()
            };
            let tabs_in = |workspace_id: Option<&str>| {
                let mut ids: Vec<String> = snapshot()
                    .into_iter()
                    .map(|pane| pane.1)
                    .filter(|tab| workspace_id.is_none_or(|ws| ws_of(tab) == ws))
                    .collect::<Vec<_>>();
                ids.sort();
                ids.dedup();
                ids
            };
            let ws_param = || params.get("workspace_id").and_then(Value::as_str);

            match request["method"].as_str().unwrap() {
                "workspace.list" => {
                    let list: Vec<Value> = workspaces
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(id, label)| {
                            json!({
                                "workspace_id": id, "label": label, "number": 1,
                                "focused": false, "agent_status": "idle"
                            })
                        })
                        .collect();
                    testkit::reply(request, json!({"type":"workspace_list","workspaces":list}))
                }
                "workspace.create" => {
                    let mut counter = next_workspace.lock().unwrap();
                    *counter += 1;
                    let ws_id = format!("w{counter}");
                    let label = params["label"].as_str().unwrap_or("new").to_string();
                    let tab_id = format!("{ws_id}:init");
                    let root = (
                        format!("{ws_id}:root"),
                        tab_id.clone(),
                        Some("initial".to_string()),
                        None,
                    );
                    workspaces.lock().unwrap().push((ws_id.clone(), label.clone()));
                    panes.lock().unwrap().push(root.clone());
                    tab_labels
                        .lock()
                        .unwrap()
                        .insert(tab_id.clone(), "initial".to_string());
                    testkit::reply(
                        request,
                        json!({"type":"workspace_created","workspace":{
                            "workspace_id": ws_id, "label": label, "number": 1,
                            "focused": false, "agent_status": "idle"
                        },"tab":tab_json(&tab_id, "initial"),"root_pane":pane_json(&root)}),
                    )
                }
                "workspace.close" => {
                    let ws = params["workspace_id"].as_str().unwrap().to_string();
                    workspaces.lock().unwrap().retain(|(id, _)| *id != ws);
                    panes.lock().unwrap().retain(|pane| ws_of(&pane.0) != ws);
                    testkit::reply(request, json!({"type":"ok"}))
                }
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
                    let list: Vec<Value> = panes_in(ws_param()).iter().map(pane_json).collect();
                    testkit::reply(request, json!({"type":"pane_list","panes":list}))
                }
                "session.snapshot" => {
                    let list: Vec<Value> = snapshot().iter().map(pane_json).collect();
                    let tab_list: Vec<Value> = tabs_in(None)
                        .iter()
                        .map(|tab| tab_json(tab, &tab_label_for(tab)))
                        .collect();
                    let ws_list: Vec<Value> = workspaces
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(id, label)| {
                            json!({
                                "workspace_id": id, "label": label, "focused": true,
                                "tab_count": 1, "pane_count": 1, "agent_status": "idle"
                            })
                        })
                        .collect();
                    testkit::reply(
                        request,
                        json!({"type":"session_snapshot","snapshot":{
                            "version":board_herdr::SUPPORTED_HERDR_VERSION,"protocol":board_herdr::SUPPORTED_HERDR_PROTOCOL,
                            "workspaces":ws_list,"tabs":tab_list,"panes":list,"agents":[]
                        }}),
                    )
                }
                "tab.list" => {
                    let tab_list: Vec<Value> = tabs_in(ws_param())
                        .iter()
                        .map(|tab| tab_json(tab, &tab_label_for(tab)))
                        .collect();
                    testkit::reply(request, json!({"type":"tab_list","tabs":tab_list}))
                }
                "tab.rename" => {
                    let tab_id = params["tab_id"].as_str().unwrap().to_string();
                    let label = params["label"].as_str().unwrap_or("").to_string();
                    tab_labels.lock().unwrap().insert(tab_id.clone(), label.clone());
                    testkit::reply(request, json!({"type":"tab_info","tab":tab_json(&tab_id, &label)}))
                }
                "tab.create" => {
                    let mut counter = next_tab.lock().unwrap();
                    *counter += 1;
                    let ws = ws_param().unwrap_or("w1").to_string();
                    let tab_id = format!("{ws}:newtab{counter}");
                    let label = params["label"].as_str().unwrap_or("card-new").to_string();
                    let root = (
                        format!("{ws}:root{counter}"),
                        tab_id.clone(),
                        Some(label.clone()),
                        None,
                    );
                    panes.lock().unwrap().push(root.clone());
                    tab_labels.lock().unwrap().insert(tab_id.clone(), label.clone());
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
                        .unwrap_or_else(|| format!("{}:t1", ws_of(&target)));
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
                            "workspace_id": ws_of(&tab),"tab_id":tab,"zoomed":false,
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
                    let (tab, ws) = guard
                        .iter()
                        .find(|pane| pane.0 == target)
                        .map(|pane| (pane.1.clone(), ws_of(&pane.0)))
                        .unwrap_or_else(|| ("w1:t1".to_string(), "w1".to_string()));
                    let child = (format!("{ws}:rescued{counter}"), tab, None, None);
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
        workspaces: workspaces2,
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
