//! Card-tab and anchor ownership: a run's tab is proved by exact durable pane
//! identity, never by a label, and an owned tab plus its shell anchor are
//! reused, recreated, or replaced accordingly.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

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
                "w1:p-old-1" | "w1:p-old-2" | "w1:p-child-1" | "w1:p-child-2"
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
        vec!["w1:p-old-1", "w1:p-old-2", "w1:p-child-1", "w1:p-child-2"]
    );
    assert!(!closed.lock().unwrap().contains(&"w1:p-anchor".to_string()));
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
        method => panic!("unexpected anchor-recovery method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let mut request = pi_req(None);
    request.tab_label = Some("card-42".into());
    request.owned_tab_id = Some("w1:t-owned".into());
    request.durable_pane_ids = vec!["w1:p-run".into()];

    let handle = spawner.spawn(&request).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-new-child"));
    assert_eq!(splits.load(Ordering::SeqCst), 2);
    let requests = fake.requests.lock().unwrap();
    assert!(requests
        .iter()
        .all(|request| request["method"] != "pane.close"));
    assert_eq!(
        requests
            .iter()
            .find(|request| request["method"] == "agent.start")
            .unwrap()["params"]["pane_id"],
        "w1:p-new-child"
    );
}
