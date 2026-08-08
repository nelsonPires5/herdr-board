//! Card-tab and anchor ownership: a run's tab is proved by exact durable pane
//! identity, never by a label, and an owned tab plus its shell anchor are
//! reused, recreated, or replaced accordingly.

use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn bootstrap_hint_adopts_the_created_workspace_tab_and_root() {
    // A NewWorkspace-created workspace starts with exactly one initial tab
    // whose root is an idle shell. The first card-tab allocation adopts it
    // instead of leaving an unused initial tab: `tab.rename` -> `card-<id>`,
    // `pane.rename` root -> `card-<id>-anchor`, then the existing anchor split
    // path. No `tab.create` happens and the root is never closed.
    let fake = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
        "tab.list" => reply(
            req,
            json!({"type":"tab_list","tabs":[{
                "tab_id":"ws1:t0","workspace_id":"ws1","number":1,
                "label":"tab","focused":false,"pane_count":1,
                "agent_status":"unknown"
            }]}),
        ),
        "pane.list" => reply(
            req,
            json!({"type":"pane_list","panes":[{
                "pane_id":"ws1:p0","terminal_id":"term-root","workspace_id":"ws1",
                "tab_id":"ws1:t0","label":"tab","agent":null,
                "agent_status":"unknown","focused":false,"revision":0
            }]}),
        ),
        "tab.rename" => {
            assert_eq!(
                req["params"],
                serde_json::json!({"tab_id": "ws1:t0", "label": "card-9"})
            );
            reply(req, json!({"type":"ok"}))
        }
        "pane.rename" => {
            let pane_id = req["params"]["pane_id"].as_str().unwrap();
            let expected_label = if pane_id == "ws1:p0" {
                "card-9-anchor"
            } else {
                "card-9-custom"
            };
            assert_eq!(req["params"]["label"], expected_label);
            pane_result(req, pane_id)
        }
        "pane.layout" => reply(
            req,
            json!({"type":"pane_layout","layout":{
                "workspace_id":"ws1","tab_id":"ws1:t0","zoomed":false,
                "area":{"x":0,"y":0,"width":200,"height":40},
                "focused_pane_id":"ws1:p0",
                "panes":[{"pane_id":"ws1:p0","focused":true,
                    "rect":{"x":0,"y":0,"width":200,"height":40}}],"splits":[]
            }}),
        ),
        "pane.split" => {
            assert_eq!(req["params"]["target_pane_id"], "ws1:p0");
            pane_result(req, "ws1:p-child")
        }
        method => panic!("unexpected bootstrap method {method}"),
    });
    let spawner = HerdrSpawner::with_pane_runner(
        fake.socket.clone(),
        Arc::new(RecordingPaneRunner {
            calls: Arc::new(Mutex::new(Vec::new())),
            behavior: Box::new(|_, _| Ok(())),
        }),
    );
    let mut request = custom_req(
        fake.socket.clone(),
        PathBuf::from("/repo"),
        vec!["configured-agent".into()],
    );
    request.tab_label = Some("card-9".into());
    request.workspace_ref = Some("ws1".into());
    request.bootstrap = Some(WorkspaceBootstrapHint {
        tab_id: "ws1:t0".into(),
        root_pane_id: "ws1:p0".into(),
    });

    let handle = spawner.spawn(&request).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("ws1:p-child"));
    // Configured harnesses keep their persistent anchor (requirement: pane run
    // exits close the child, so the anchor must survive across runs).
    assert_eq!(handle.anchor_pane_id.as_deref(), Some("ws1:p0"));

    let requests = fake.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "tab.create")
            .count(),
        0,
        "the created workspace's initial tab must be adopted, not duplicated"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "pane.close")
            .count(),
        0,
        "the adopted workspace root is never closed by the allocator"
    );
    let split = requests
        .iter()
        .find(|r| r["method"] == "pane.split")
        .unwrap();
    assert_eq!(split["params"]["target_pane_id"], "ws1:p0");
}

#[test]
fn failed_adopted_launch_then_retry_recovers_the_remembered_tab_without_tab_create() {
    // The bootstrap hint's exact tab/root is remembered under the per-card
    // allocation lock BEFORE the first allocation. When the adopted launch then
    // fails (split or agent.start), the adopted tab is not orphaned: the next
    // spawn on the same HerdrSpawner — a placement retry or a later dispatch in
    // the same daemon — recovers the remembered tab by exact id and splits from
    // its root instead of calling `tab.create` a second time.
    // (pane_id, tab_id, label, agent)
    type FakePane = (String, String, String, Option<String>);
    let panes: Arc<Mutex<Vec<FakePane>>> = Arc::new(Mutex::new(vec![(
        "ws1:p0".to_string(),
        "ws1:t0".to_string(),
        "tab".to_string(),
        None,
    )]));
    let panes_for_server = Arc::clone(&panes);
    let tabs: Arc<Mutex<Vec<(String, String)>>> =
        Arc::new(Mutex::new(vec![("ws1:t0".to_string(), "tab".to_string())]));
    let tabs_for_server = Arc::clone(&tabs);
    let starts = Arc::new(AtomicUsize::new(0));
    let starts_for_server = Arc::clone(&starts);
    let next_child = Arc::new(AtomicUsize::new(0));
    let next_child_for_server = Arc::clone(&next_child);

    let fake = serve_recording_herdr(move |req, _| {
        let pane_json = |(pane_id, tab_id, label, agent): &FakePane| {
            json!({
                "pane_id": pane_id, "terminal_id": format!("term-{pane_id}"),
                "workspace_id": "ws1", "tab_id": tab_id, "label": label,
                "agent": agent, "agent_status": "idle",
                "focused": false, "revision": 1
            })
        };
        match req["method"].as_str().unwrap() {
            "tab.list" => {
                let list: Vec<Value> = tabs_for_server
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(tab_id, label)| {
                        json!({"tab_id": tab_id, "workspace_id": "ws1", "number": 1,
                               "label": label, "focused": false, "pane_count": 1,
                               "agent_status": "idle"})
                    })
                    .collect();
                reply(req, json!({"type": "tab_list", "tabs": list}))
            }
            "pane.list" => {
                let list: Vec<Value> = panes_for_server
                    .lock()
                    .unwrap()
                    .iter()
                    .map(pane_json)
                    .collect();
                reply(req, json!({"type": "pane_list", "panes": list}))
            }
            "tab.rename" => {
                let tab_id = req["params"]["tab_id"].as_str().unwrap().to_string();
                let label = req["params"]["label"].as_str().unwrap().to_string();
                for (id, current) in tabs_for_server.lock().unwrap().iter_mut() {
                    if *id == tab_id {
                        *current = label.clone();
                    }
                }
                reply(req, json!({"type": "ok"}))
            }
            "pane.rename" => {
                let pane_id = req["params"]["pane_id"].as_str().unwrap().to_string();
                let label = req["params"]["label"].as_str().unwrap().to_string();
                for (id, _, current, _) in panes_for_server.lock().unwrap().iter_mut() {
                    if *id == pane_id {
                        *current = label.clone();
                    }
                }
                pane_result(req, &pane_id)
            }
            "pane.layout" => {
                let target = req["params"]["pane_id"].as_str().unwrap().to_string();
                reply(
                    req,
                    json!({"type": "pane_layout", "layout": {
                        "workspace_id": "ws1", "tab_id": "ws1:t0", "zoomed": false,
                        "area": {"x": 0, "y": 0, "width": 200, "height": 40},
                        "focused_pane_id": target,
                        "panes": [{"pane_id": target, "focused": true,
                            "rect": {"x": 0, "y": 0, "width": 200, "height": 40}}],
                        "splits": []
                    }}),
                )
            }
            "pane.split" => {
                let target = req["params"]["target_pane_id"]
                    .as_str()
                    .unwrap()
                    .to_string();
                let tab = {
                    let guard = panes_for_server.lock().unwrap();
                    guard
                        .iter()
                        .find(|(id, _, _, _)| *id == target)
                        .map(|(_, tab, _, _)| tab.clone())
                        .unwrap_or_else(|| "ws1:t0".to_string())
                };
                let n = next_child_for_server.fetch_add(1, Ordering::SeqCst);
                let child = format!("ws1:p-child-{n}");
                panes_for_server
                    .lock()
                    .unwrap()
                    .push((child.clone(), tab, String::new(), None));
                pane_result(req, &child)
            }
            "agent.start" => {
                let call = starts_for_server.fetch_add(1, Ordering::SeqCst);
                let pane_id = req["params"]["pane_id"].as_str().unwrap().to_string();
                if call == 0 {
                    error(req, "agent_start_failed", "harness refused to start")
                } else {
                    agent_started(req, &pane_id, false, true)
                }
            }
            "pane.close" => {
                let pane_id = req["params"]["pane_id"].as_str().unwrap().to_string();
                panes_for_server
                    .lock()
                    .unwrap()
                    .retain(|(id, _, _, _)| *id != pane_id);
                pane_result(req, &pane_id)
            }
            method => panic!("unexpected adopted-retry method {method}"),
        }
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    // First dispatch: the workspace was just created, so the request carries
    // the one-shot bootstrap hint. The adoption renames tab/root and splits the
    // child, but the managed launch fails.
    let mut first = pi_req(None);
    first.tab_label = Some("card-9".into());
    first.workspace_ref = Some("ws1".into());
    first.bootstrap = Some(WorkspaceBootstrapHint {
        tab_id: "ws1:t0".into(),
        root_pane_id: "ws1:p0".into(),
    });
    let err = spawner.spawn(&first).unwrap_err();
    assert!(
        err.to_string().contains("harness refused to start"),
        "{err:#}"
    );

    // Later retry in the same daemon: the workspace now exists, so no bootstrap
    // hint — but the registry remembers the adopted tab/root. The retry must
    // recover that tab by exact id and split from its root, never tab.create.
    let mut second = pi_req(None);
    second.tab_label = Some("card-9".into());
    second.workspace_ref = Some("ws1".into());
    second.bootstrap = None;
    let handle = spawner.spawn(&second).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("ws1:p-child-1"));

    let requests = fake.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "tab.create")
            .count(),
        0,
        "the retry must recover the remembered adopted tab, not create a second one"
    );
    let splits: Vec<&str> = requests
        .iter()
        .filter(|r| r["method"] == "pane.split")
        .map(|r| r["params"]["target_pane_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        splits,
        ["ws1:p0", "ws1:p0"],
        "both attempts split their child from the adopted root"
    );
    let closes: Vec<&str> = requests
        .iter()
        .filter(|r| r["method"] == "pane.close")
        .map(|r| r["params"]["pane_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        closes,
        ["ws1:p-child-0", "ws1:p0"],
        "the failed launch closes only its child; the retry's successful managed launch closes the anchor"
    );
    let started: Vec<&str> = requests
        .iter()
        .filter(|r| r["method"] == "agent.start")
        .map(|r| r["params"]["pane_id"].as_str().unwrap())
        .collect();
    assert_eq!(started, ["ws1:p-child-0", "ws1:p-child-1"]);
}

#[test]
fn bootstrap_hint_verification_failure_falls_back_without_touching_the_root() {
    // The root pane already carries a user agent: the hint is not adoptable.
    // Allocation must fall back to a fresh `tab.create` and must NOT close or
    // rename the workspace root (a foreign pane is never a cleanup target).
    let fake = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
        "tab.list" => reply(
            req,
            json!({"type":"tab_list","tabs":[{
                "tab_id":"ws1:t0","workspace_id":"ws1","number":1,
                "label":"tab","focused":false,"pane_count":1,
                "agent_status":"unknown"
            }]}),
        ),
        "pane.list" => reply(
            req,
            json!({"type":"pane_list","panes":[{
                "pane_id":"ws1:p0","terminal_id":"term-root","workspace_id":"ws1",
                "tab_id":"ws1:t0","label":"tab","agent":"pi",
                "agent_status":"idle","focused":false,"revision":0
            }]}),
        ),
        "tab.create" => reply(
            req,
            json!({
                "type":"tab_created",
                "tab": {"tab_id":"ws1:t1","workspace_id":"ws1","number":2,
                    "label":"card-9","focused":false,"pane_count":1},
                "root_pane": {"pane_id":"ws1:p-fresh","terminal_id":"term-fresh",
                    "workspace_id":"ws1","tab_id":"ws1:t1","focused":false,"revision":0}
            }),
        ),
        "pane.rename" => pane_result(req, req["params"]["pane_id"].as_str().unwrap()),
        "pane.layout" => reply(
            req,
            json!({"type":"pane_layout","layout":{
                "workspace_id":"ws1","tab_id":"ws1:t1","zoomed":false,
                "area":{"x":0,"y":0,"width":200,"height":40},
                "focused_pane_id":"ws1:p-fresh",
                "panes":[{"pane_id":"ws1:p-fresh","focused":true,
                    "rect":{"x":0,"y":0,"width":200,"height":40}}],"splits":[]
            }}),
        ),
        "pane.split" => pane_result(req, "ws1:p-fresh-child"),
        method => panic!("unexpected bootstrap-fallback method {method}"),
    });
    let spawner = HerdrSpawner::with_pane_runner(
        fake.socket.clone(),
        Arc::new(RecordingPaneRunner {
            calls: Arc::new(Mutex::new(Vec::new())),
            behavior: Box::new(|_, _| Ok(())),
        }),
    );
    let mut request = custom_req(
        fake.socket.clone(),
        PathBuf::from("/repo"),
        vec!["configured-agent".into()],
    );
    request.tab_label = Some("card-9".into());
    request.workspace_ref = Some("ws1".into());
    request.bootstrap = Some(WorkspaceBootstrapHint {
        tab_id: "ws1:t0".into(),
        root_pane_id: "ws1:p0".into(),
    });

    let handle = spawner.spawn(&request).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("ws1:p-fresh-child"));
    let requests = fake.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "tab.rename")
            .count(),
        0,
        "an unverifiable bootstrap hint must not rename the user tab"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "pane.close")
            .count(),
        0,
        "the unverifiable workspace root must not be closed"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "tab.create")
            .count(),
        1,
        "verification failure must fall back to a fresh tab.create"
    );
}

#[test]
fn first_card_run_labels_root_anchor_and_starts_only_the_split_child() {
    let fake = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p-root"),
        "pane.rename" => {
            assert_eq!(
                req["params"],
                serde_json::json!({
                    "pane_id": "w1:p-root",
                    "label": "card-42-anchor"
                })
            );
            pane_result(req, "w1:p-root")
        }
        "pane.layout" => reply(
            req,
            serde_json::json!({"type":"pane_layout", "layout":{
                "workspace_id":"w1","tab_id":"w1:t1","zoomed":false,
                "area":{"x":0,"y":0,"width":200,"height":40},"focused_pane_id":"w1:p-root",
                "panes":[{"pane_id":"w1:p-root","focused":true,
                    "rect":{"x":0,"y":0,"width":200,"height":40}}],"splits":[]
            }}),
        ),
        "pane.split" => {
            assert_eq!(req["params"]["target_pane_id"], "w1:p-root");
            assert_eq!(req["params"]["ratio"], 0.4);
            pane_result(req, "w1:p-child")
        }
        "agent.start" => {
            assert_eq!(req["params"]["pane_id"], "w1:p-child");
            agent_started(req, "w1:p-child", false, true)
        }
        method => panic!("unexpected first-card method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let mut request = pi_req(None);
    request.tab_label = Some("card-42".into());

    let handle = spawner.spawn(&request).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-child"));
    let requests = fake.requests.lock().unwrap();
    let split = requests
        .iter()
        .find(|request| request["method"] == "pane.split")
        .unwrap();
    assert_eq!(split["params"]["target_pane_id"], "w1:p-root");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "agent.start")
            .count(),
        1
    );
}

#[test]
fn sequential_card_runs_reclaim_only_exact_ended_children_before_splitting() {
    let children = Arc::new(Mutex::new(vec![
        "w1:p-old-1".to_string(),
        "w1:p-old-2".to_string(),
    ]));
    let children_for_server = Arc::clone(&children);
    let closed = Arc::new(Mutex::new(Vec::<String>::new()));
    let closed_for_server = Arc::clone(&closed);
    let split_count = Arc::new(AtomicUsize::new(0));
    let split_count_for_server = Arc::clone(&split_count);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => reply(
            req,
            serde_json::json!({"type":"tab_list", "tabs":[{
                "tab_id":"w1:t1","workspace_id":"w1","number":1,
                "label":"card-42","pane_count":4
            }]}),
        ),
        "pane.list" => {
            let mut panes = vec![pane_info("w1:p-anchor")];
            let mut foreign = pane_info("w1:p-foreign");
            foreign["label"] = Value::String("card-42-anchor".into());
            panes.push(foreign);
            panes.extend(children_for_server.lock().unwrap().iter().map(|id| {
                let mut pane = pane_info(id);
                pane["agent_status"] = Value::String("done".into());
                pane
            }));
            reply(req, serde_json::json!({"type":"pane_list","panes":panes}))
        }
        "pane.layout" => reply(
            req,
            serde_json::json!({"type":"pane_layout","layout":{
                "workspace_id":"w1","tab_id":"w1:t1","zoomed":false,
                "area":{"x":0,"y":0,"width":200,"height":40},"focused_pane_id":"w1:p-anchor",
                "panes":[{"pane_id":"w1:p-anchor","focused":true,
                    "rect":{"x":0,"y":0,"width":200,"height":40}}],"splits":[]
            }}),
        ),
        "pane.close" => {
            let pane_id = req["params"]["pane_id"].as_str().unwrap().to_string();
            assert!(matches!(
                pane_id.as_str(),
                "w1:p-old-1" | "w1:p-old-2" | "w1:p-child-1" | "w1:p-child-2" | "w1:p-anchor"
            ));
            closed_for_server.lock().unwrap().push(pane_id.clone());
            children_for_server
                .lock()
                .unwrap()
                .retain(|id| id != &pane_id);
            pane_result(req, &pane_id)
        }
        "pane.split" => {
            assert_eq!(req["params"]["target_pane_id"], "w1:p-anchor");
            let n = split_count_for_server.fetch_add(1, Ordering::SeqCst) + 1;
            let child = format!("w1:p-child-{n}");
            children_for_server.lock().unwrap().push(child.clone());
            pane_result(req, &child)
        }
        "agent.start" => {
            agent_started(req, req["params"]["pane_id"].as_str().unwrap(), false, true)
        }
        method => panic!("unexpected sequential-card method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let mut request = pi_req(None);
    request.tab_label = Some("card-42".into());
    request.owned_tab_id = Some("w1:t1".into());
    request.durable_anchor_pane_ids = vec!["w1:p-anchor".into()];
    request.durable_pane_ids = vec![
        "w1:p-old-2".into(),
        "w1:p-old-1".into(),
        "w1:p-child-1".into(),
        "w1:p-child-2".into(),
    ];
    request.reclaimable_pane_ids = request.durable_pane_ids.clone();

    let first = spawner.spawn(&request).unwrap();
    let second = spawner.spawn(&request).unwrap();
    let third = spawner.spawn(&request).unwrap();
    assert_eq!(first.pane_id.as_deref(), Some("w1:p-child-1"));
    assert_eq!(second.pane_id.as_deref(), Some("w1:p-child-2"));
    assert_eq!(third.pane_id.as_deref(), Some("w1:p-child-3"));
    assert_eq!(split_count.load(Ordering::SeqCst), 3);
    assert_eq!(
        *closed.lock().unwrap(),
        vec![
            "w1:p-old-1",
            "w1:p-old-2",
            "w1:p-anchor",
            "w1:p-child-1",
            "w1:p-anchor",
            "w1:p-child-2",
            "w1:p-anchor"
        ],
        "each managed launch reclaims the ended child and then closes the anchor"
    );
    assert!(!closed.lock().unwrap().contains(&"w1:p-foreign".to_string()));
}

#[test]
fn fresh_card_tab_geometry_failure_cleans_anchor_and_preserves_error() {
    let fake = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p-fresh-anchor"),
        "pane.rename" => pane_result(req, "w1:p-fresh-anchor"),
        "pane.layout" => reply(
            req,
            serde_json::json!({"type":"pane_layout","layout":{
                "workspace_id":"w1","tab_id":"w1:t1","zoomed":false,
                "area":{"x":0,"y":0,"width":23,"height":13},"focused_pane_id":"w1:p-fresh-anchor",
                "panes":[{"pane_id":"w1:p-fresh-anchor","focused":true,
                    "rect":{"x":0,"y":0,"width":23,"height":13}}],"splits":[]
            }}),
        ),
        "pane.close" => {
            assert_eq!(req["params"]["pane_id"], "w1:p-fresh-anchor");
            error(req, "pane_not_found", "cleanup already won")
        }
        method => panic!("unexpected fresh-card geometry method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let mut request = pi_req(None);
    request.tab_label = Some("card-42".into());

    let error = spawner.spawn(&request).unwrap_err();
    assert!(format!("{error:#}").contains("anchor_too_small"));
    assert!(!format!("{error:#}").contains("cleanup already won"));
}

#[test]
fn renamed_owned_card_tab_reuses_exact_anchor_id_not_a_label() {
    let fake = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
        "tab.list" => reply(
            req,
            serde_json::json!({"type":"tab_list", "tabs":[
                {"tab_id":"w1:t-owned","workspace_id":"w1","number":1,
                    "label":"renamed-by-user","pane_count":2},
                {"tab_id":"w1:t-user","workspace_id":"w1","number":2,
                    "label":"card-42","pane_count":1}
            ]}),
        ),
        "pane.list" => reply(
            req,
            serde_json::json!({"type":"pane_list", "panes":[
                {"pane_id":"w1:p-foreign","terminal_id":"foreign","workspace_id":"w1",
                    "tab_id":"w1:t-owned","label":"card-42-anchor","focused":false,
                    "agent_status":"unknown","revision":1},
                {"pane_id":"w1:p-anchor","terminal_id":"owned","workspace_id":"w1",
                    "tab_id":"w1:t-owned","label":"renamed-by-user","focused":false,
                    "agent_status":"unknown","revision":1}
            ]}),
        ),
        "pane.layout" => reply(
            req,
            serde_json::json!({"type":"pane_layout", "layout":{
                "workspace_id":"w1","tab_id":"w1:t-owned","zoomed":false,
                "area":{"x":0,"y":0,"width":200,"height":40},"focused_pane_id":"w1:p-anchor",
                "panes":[{"pane_id":"w1:p-anchor","focused":true,
                    "rect":{"x":0,"y":0,"width":200,"height":40}}],"splits":[]
            }}),
        ),
        "pane.split" => {
            assert_eq!(req["params"]["target_pane_id"], "w1:p-anchor");
            pane_result(req, "w1:p-child")
        }
        "agent.start" => agent_started(req, "w1:p-child", false, true),
        method => panic!("unexpected renamed card-tab method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let mut request = pi_req(None);
    request.tab_label = Some("card-42".into());
    request.owned_tab_id = Some("w1:t-owned".into());
    request.durable_anchor_pane_ids = vec!["w1:p-anchor".into()];

    let handle = spawner.spawn(&request).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-child"));
    assert_eq!(handle.anchor_pane_id.as_deref(), Some("w1:p-anchor"));
    let requests = fake.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "tab.create")
            .count(),
        0
    );
    assert!(requests
        .iter()
        .all(|request| request["method"] != "pane.rename"));
}

#[test]
fn card_tab_with_no_panes_is_replaced_without_label_adoption() {
    let fake = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
        "tab.list" => reply(
            req,
            serde_json::json!({"type":"tab_list", "tabs":[{
                "tab_id":"w1:t-owned","workspace_id":"w1","number":1,
                "label":"card-42","focused":false,"pane_count":0
            }]}),
        ),
        "pane.list" => reply(req, serde_json::json!({"type":"pane_list", "panes":[]})),
        "tab.create" => reply(
            req,
            serde_json::json!({
                "type":"tab_created",
                "tab": {"tab_id":"w1:t-replacement","workspace_id":"w1","number":2,
                    "label":"card-42","focused":false,"pane_count":1},
                "root_pane": {"pane_id":"w1:p-replacement","terminal_id":"term-root",
                    "workspace_id":"w1","tab_id":"w1:t-replacement","focused":false,"revision":0}
            }),
        ),
        "pane.rename" => pane_result(req, "w1:p-replacement"),
        "pane.layout" => reply(
            req,
            serde_json::json!({"type":"pane_layout", "layout":{
                "workspace_id":"w1","tab_id":"w1:t-replacement","zoomed":false,
                "area":{"x":0,"y":0,"width":200,"height":40},"focused_pane_id":"w1:p-replacement",
                "panes":[{"pane_id":"w1:p-replacement","focused":true,
                    "rect":{"x":0,"y":0,"width":200,"height":40}}],"splits":[]
            }}),
        ),
        "pane.split" => pane_result(req, "w1:p-replacement-child"),
        "agent.start" => agent_started(req, "w1:p-replacement-child", false, true),
        method => panic!("unexpected empty card-tab method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let mut request = pi_req(None);
    request.tab_label = Some("card-42".into());
    request.owned_tab_id = Some("w1:t-owned".into());

    let handle = spawner.spawn(&request).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-replacement-child"));
    let requests = fake.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "tab.create")
            .count(),
        1
    );
    assert!(requests
        .iter()
        .any(|request| request["method"] == "pane.split"));
}

#[test]
fn concurrent_first_card_allocations_create_one_owned_tab() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let created = Arc::new(AtomicBool::new(false));
    let created2 = Arc::clone(&created);
    let tab_creates = Arc::new(AtomicUsize::new(0));
    let tab_creates2 = Arc::clone(&tab_creates);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => {
            let tabs = if created2.load(Ordering::SeqCst) {
                serde_json::json!([{
                    "tab_id":"w1:t-owned","workspace_id":"w1","number":1,
                    "label":"card-42","focused":false,"pane_count":1
                }])
            } else {
                serde_json::json!([])
            };
            reply(req, serde_json::json!({"type":"tab_list", "tabs":tabs}))
        }
        "tab.create" => {
            assert!(!created2.swap(true, Ordering::SeqCst));
            tab_creates2.fetch_add(1, Ordering::SeqCst);
            reply(
                req,
                serde_json::json!({
                    "type":"tab_created",
                    "tab": {"tab_id":"w1:t-owned","workspace_id":"w1","number":1,
                        "label":"card-42","focused":false,"pane_count":1},
                    "root_pane": {"pane_id":"w1:p-root","terminal_id":"term-root",
                        "workspace_id":"w1","tab_id":"w1:t-owned","focused":false,"revision":0}
                }),
            )
        }
        "pane.rename" => pane_result(req, "w1:p-root"),
        "pane.list" => reply(
            req,
            serde_json::json!({"type":"pane_list", "panes":[{
                "pane_id":"w1:p-root","terminal_id":"term-root","workspace_id":"w1",
                "tab_id":"w1:t-owned","label":"card-42-anchor","focused":false,
                "agent_status":"unknown","revision":0
            }]}),
        ),
        "pane.layout" => reply(
            req,
            serde_json::json!({"type":"pane_layout", "layout":{
                "workspace_id":"w1","tab_id":"w1:t-owned","zoomed":false,
                "area":{"x":0,"y":0,"width":100,"height":40},"focused_pane_id":"w1:p-root",
                "panes":[{"pane_id":"w1:p-root","focused":true,
                    "rect":{"x":0,"y":0,"width":100,"height":40}}],"splits":[]
            }}),
        ),
        "pane.split" => pane_result(req, "w1:p-split"),
        "agent.start" => {
            agent_started(req, req["params"]["pane_id"].as_str().unwrap(), false, true)
        }
        "pane.close" => pane_result(req, req["params"]["pane_id"].as_str().unwrap()),
        method => panic!("unexpected concurrent card-tab method {method}"),
    });
    let spawner = Arc::new(HerdrSpawner::new(fake.socket.clone()));
    let mut request = pi_req(None);
    request.tab_label = Some("card-42".into());
    let first_request = request.clone();
    let second_request = request;
    let first_spawner = Arc::clone(&spawner);
    let second_spawner = Arc::clone(&spawner);
    let first = std::thread::spawn(move || first_spawner.spawn(&first_request).unwrap());
    let second = std::thread::spawn(move || second_spawner.spawn(&second_request).unwrap());

    let first_handle = first.join().unwrap();
    let second_handle = second.join().unwrap();
    assert!(first_handle.pane_id.is_some());
    assert!(second_handle.pane_id.is_some());
    assert_eq!(tab_creates.load(Ordering::SeqCst), 1);
}

#[test]
fn card_tabs_reuse_exact_owned_id_and_ignore_duplicate_and_legacy_labels() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let list_calls = Arc::new(AtomicUsize::new(0));
    let list_calls2 = Arc::clone(&list_calls);
    let create_calls = Arc::new(AtomicUsize::new(0));
    let create_calls2 = Arc::clone(&create_calls);
    let split_calls = Arc::new(AtomicUsize::new(0));
    let split_calls2 = Arc::clone(&split_calls);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => {
            let call = list_calls2.fetch_add(1, Ordering::SeqCst);
            let tabs = match call {
                0 => serde_json::json!([
                    {"tab_id":"w1:t-kanban","workspace_id":"w1","number":1,"label":"kanban"},
                    {"tab_id":"w1:t-user-z","workspace_id":"w1","number":2,"label":"card-42"},
                    {"tab_id":"w1:t-user-a","workspace_id":"w1","number":3,"label":"card-42"}
                ]),
                1 => serde_json::json!([
                    {"tab_id":"w1:t-user-a","workspace_id":"w1","number":3,"label":"card-42"},
                    {"tab_id":"w1:t-owned-42","workspace_id":"w1","number":9,"label":"card-42"},
                    {"tab_id":"w1:t-user-z","workspace_id":"w1","number":2,"label":"card-42"}
                ]),
                2 => serde_json::json!([
                    {"tab_id":"w1:t-owned-42","workspace_id":"w1","number":9,"label":"card-42"},
                    {"tab_id":"w1:t-user-43","workspace_id":"w1","number":4,"label":"card-43"}
                ]),
                3 => serde_json::json!([
                    {"tab_id":"w1:t-kanban","workspace_id":"w1","number":1,"label":"kanban"},
                    {"tab_id":"w1:t-user-a","workspace_id":"w1","number":3,"label":"card-42"}
                ]),
                other => panic!("unexpected tab.list call {other}"),
            };
            reply(req, serde_json::json!({"type":"tab_list", "tabs": tabs}))
        }
        "tab.create" => {
            let tab_id = match create_calls2.fetch_add(1, Ordering::SeqCst) {
                0 => "w1:t-owned-42",
                1 => "w1:t-owned-43",
                2 => "w1:t-recreated-42",
                other => panic!("unexpected tab.create call {other}"),
            };
            let pane_id = format!("{tab_id}:root");
            reply(
                req,
                serde_json::json!({
                    "type":"tab_created",
                    "tab": {"tab_id":tab_id,"workspace_id":"w1","number":10,
                        "label":req["params"]["label"],"focused":false,"pane_count":1},
                    "root_pane": {"pane_id":pane_id,"terminal_id":"term-root",
                        "workspace_id":"w1","tab_id":tab_id,"focused":false,"revision":0}
                }),
            )
        }
        "pane.list" => reply(
            req,
            serde_json::json!({"type":"pane_list","panes":[{
                "pane_id":"w1:t-owned-42:root","terminal_id":"term-owned","workspace_id":"w1",
                "tab_id":"w1:t-owned-42","label":"card-42-anchor", "focused":false,
                "agent_status":"unknown","revision":0
            }]}),
        ),
        "pane.layout" => {
            let target = req["params"]["pane_id"].as_str().unwrap();
            let tab_id = if target.starts_with("w1:t-owned-42") {
                "w1:t-owned-42"
            } else if target.starts_with("w1:t-owned-43") {
                "w1:t-owned-43"
            } else {
                "w1:t-recreated-42"
            };
            reply(
                req,
                serde_json::json!({"type":"pane_layout","layout":{
                    "workspace_id":"w1","tab_id":tab_id,"zoomed":false,
                    "area":{"x":0,"y":0,"width":100,"height":40},"focused_pane_id":target,
                    "panes":[{"pane_id":target,"focused":true,
                        "rect":{"x":0,"y":0,"width":100,"height":40}}],"splits":[]
                }}),
            )
        }
        "pane.rename" => pane_result(req, req["params"]["pane_id"].as_str().unwrap()),
        "pane.split" => {
            let target = req["params"]["target_pane_id"].as_str().unwrap();
            let call = split_calls2.fetch_add(1, Ordering::SeqCst);
            let child = match call {
                1 => "w1:p-split-owned-42".to_string(),
                _ if target.ends_with(":root") => format!("{target}:child"),
                _ => "w1:p-split-owned-42".to_string(),
            };
            pane_result(req, &child)
        }
        "agent.start" => {
            agent_started(req, req["params"]["pane_id"].as_str().unwrap(), false, true)
        }
        "pane.close" => pane_result(req, req["params"]["pane_id"].as_str().unwrap()),
        method => panic!("unexpected card-tab method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let mut card_42 = pi_req(None);
    card_42.tab_label = Some("card-42".into());
    let mut card_43 = card_42.clone();
    card_43.tab_label = Some("card-43".into());
    card_43.name = "card-43-execute".into();

    let first = spawner.spawn(&card_42).unwrap();
    let reused = spawner.spawn(&card_42).unwrap();
    let other = spawner.spawn(&card_43).unwrap();
    let recreated = spawner.spawn(&card_42).unwrap();

    assert_eq!(first.pane_id.as_deref(), Some("w1:t-owned-42:root:child"));
    assert_eq!(reused.pane_id.as_deref(), Some("w1:p-split-owned-42"));
    assert_eq!(other.pane_id.as_deref(), Some("w1:t-owned-43:root:child"));
    assert_eq!(
        recreated.pane_id.as_deref(),
        Some("w1:t-recreated-42:root:child")
    );
    let requests = fake.requests.lock().unwrap();
    let created_labels = requests
        .iter()
        .filter(|request| request["method"] == "tab.create")
        .map(|request| request["params"]["label"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(created_labels, ["card-42", "card-43", "card-42"]);
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "pane.split")
            .count(),
        4
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "pane.rename")
            .count(),
        3
    );
    assert!(requests.iter().all(|r| r["method"] != "tab.rename"));
    let starts = requests
        .iter()
        .filter(|r| r["method"] == "agent.start")
        .map(|r| r["params"]["pane_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(starts.iter().all(|pane| !pane.ends_with(":root")));
    let first_split = requests
        .iter()
        .find(|r| {
            r["method"] == "pane.split" && r["params"]["target_pane_id"] == "w1:t-owned-42:root"
        })
        .unwrap();
    assert_eq!(first_split["params"]["ratio"], 0.4);
}

#[test]
fn missing_anchor_is_recreated_from_a_durable_child_only() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let splits = Arc::new(AtomicUsize::new(0));
    let splits2 = Arc::clone(&splits);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => reply(
            req,
            serde_json::json!({"type":"tab_list", "tabs":[{
                "tab_id":"w1:t-owned","workspace_id":"w1","number":1,
                "label":"card-42","focused":false,"pane_count":1
            }]}),
        ),
        "pane.list" => reply(
            req,
            serde_json::json!({"type":"pane_list", "panes":[{
                "pane_id":"w1:p-run","terminal_id":"term-run","workspace_id":"w1",
                "tab_id":"w1:t-owned","agent":null,"agent_status":"unknown",
                "focused":false,"revision":2
            }]}),
        ),
        "pane.layout" => {
            let target = req["params"]["pane_id"].as_str().unwrap();
            let (width, height) = if target == "w1:p-run" {
                (240, 40)
            } else {
                (100, 40)
            };
            reply(
                req,
                serde_json::json!({"type":"pane_layout", "layout":{
                    "workspace_id":"w1","tab_id":"w1:t-owned","zoomed":false,
                    "area":{"x":0,"y":0,"width":width,"height":height},
                    "focused_pane_id":target,
                    "panes":[{"pane_id":target,"focused":true,
                        "rect":{"x":0,"y":0,"width":width,"height":height}}],"splits":[]
                }}),
            )
        }
        "pane.split" => {
            let call = splits2.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                req["params"]["target_pane_id"],
                if call == 0 {
                    "w1:p-run"
                } else {
                    "w1:p-anchor-recreated"
                }
            );
            pane_result(
                req,
                if call == 0 {
                    "w1:p-anchor-recreated"
                } else {
                    "w1:p-new-child"
                },
            )
        }
        "pane.rename" => pane_result(req, "w1:p-anchor-recreated"),
        "agent.start" => agent_started(req, "w1:p-new-child", false, true),
        "pane.close" => {
            assert_eq!(req["params"]["pane_id"], "w1:p-anchor-recreated");
            pane_result(req, "w1:p-anchor-recreated")
        }
        method => panic!("unexpected anchor-recovery method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let mut request = pi_req(None);
    request.tab_label = Some("card-42".into());
    request.owned_tab_id = Some("w1:t-owned".into());
    request.durable_pane_ids = vec!["w1:p-run".into()];

    let handle = spawner.spawn(&request).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-new-child"));
    assert_eq!(
        handle.anchor_pane_id.as_deref(),
        None,
        "a managed recovery must not persist its temporary anchor"
    );
    assert_eq!(splits.load(Ordering::SeqCst), 2);
    let requests = fake.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "pane.close")
            .count(),
        1,
        "the temporary recovery anchor is closed after the managed launch"
    );
    assert_eq!(
        requests
            .iter()
            .find(|request| request["method"] == "pane.close")
            .unwrap()["params"]["pane_id"],
        "w1:p-anchor-recreated"
    );
    assert_eq!(
        requests
            .iter()
            .find(|request| request["method"] == "agent.start")
            .unwrap()["params"]["pane_id"],
        "w1:p-new-child"
    );
}
