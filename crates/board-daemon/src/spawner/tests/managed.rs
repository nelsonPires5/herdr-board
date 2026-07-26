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

#[test]
fn herdr_protocol_gate_rejects_mismatches_before_any_spawn_or_placement_call() {
    for (version, protocol) in [("0.7.4", 17), ("0.7.5", 16)] {
        let fake = serve_recording_herdr_with_ping(
            |req, _| error(req, "unexpected_call", "protocol gate was bypassed"),
            version,
            protocol,
        );
        let calls = Arc::new(Mutex::new(Vec::<PaneRunCall>::new()));
        let runner = RecordingPaneRunner {
            calls: Arc::clone(&calls),
            behavior: Box::new(|_, _| anyhow::bail!("runner must not be called")),
        };
        let spawner = HerdrSpawner::with_pane_runner(fake.socket.clone(), Arc::new(runner));

        let err = spawner
            .spawn(&custom_req(
                fake.socket.clone(),
                PathBuf::from("/tmp/card cwd"),
                vec!["custom-agent".into()],
            ))
            .unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("Herdr 0.7.5 with protocol 17 is required"),
            "mismatch must explain the required Herdr version/protocol: {text}"
        );
        assert_eq!(
            fake.requests
                .lock()
                .unwrap()
                .iter()
                .map(|r| r["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["ping"],
            "protocol mismatch must stop before tab.list/tab.create/pane.split"
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "protocol mismatch must stop before pane runner"
        );
    }
}

#[test]
fn managed_pi_uses_startup_only_system_file_then_polls_ready_before_card_prompt() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let prompt_path = Arc::new(Mutex::new(None::<PathBuf>));
    let prompt_path2 = Arc::clone(&prompt_path);
    let gets = Arc::new(AtomicUsize::new(0));
    let gets2 = Arc::clone(&gets);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p2"),
        "agent.start" => {
            let path = assert_startup_prompt_file(
                req,
                &[
                    "--model",
                    "provider/model with space",
                    "--session-id",
                    "session-42",
                ],
                "--append-system-prompt",
                "system instructions\nwith an exact second line",
            );
            *prompt_path2.lock().unwrap() = Some(path);
            agent_started(req, "w1:p2", true, false)
        }
        "agent.get" => {
            let call = gets2.fetch_add(1, Ordering::SeqCst);
            assert_eq!(req["params"], serde_json::json!({"target": "w1:p2"}));
            if call == 0 {
                agent_get_result(req, "w1:p2", "card-42-execute", true, false)
            } else {
                agent_get_result(req, "w1:p2", "card-42-execute", false, true)
            }
        }
        "agent.prompt" => {
            assert_eq!(
                gets2.load(Ordering::SeqCst),
                2,
                "agent.prompt must not be sent while agent.get is still pending",
            );
            assert_eq!(
                req["params"],
                serde_json::json!({
                    "target": "w1:p2",
                    "text": "first task line\nsecond task line with spaces"
                }),
                "only the initial/card prompt belongs in agent.prompt",
            );
            agent_prompted(req, "w1:p2", "card-42-execute")
        }
        method => panic!("unexpected protocol-17 method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let prompt = "first task line\nsecond task line with spaces";

    let handle = spawner.spawn(&pi_req(Some(prompt))).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p2"));
    let path = prompt_path.lock().unwrap().clone().unwrap();
    assert!(
        !path.exists(),
        "the 0600 system-prompt file must be removed before spawn returns"
    );

    let requests = fake.requests.lock().unwrap();
    let methods: Vec<_> = requests
        .iter()
        .map(|r| r["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        [
            "ping",
            "tab.list",
            "tab.create",
            "agent.start",
            "agent.get",
            "agent.get",
            "agent.prompt"
        ],
        "schema-valid readiness polling must precede prompt submission",
    );
    assert_eq!(
        requests[2]["params"],
        serde_json::json!({
            "workspace_id": "w1", "label": "kanban", "cwd": "/tmp/card cwd",
            "env": {"BOARD_CARD_ID": "42"}, "focus": false
        })
    );
    assert_eq!(requests[3]["params"]["name"], "card-42-execute");
    assert_eq!(requests[3]["params"]["kind"], "pi");
    assert_eq!(requests[3]["params"]["pane_id"], "w1:p2");
    assert_eq!(requests[3]["params"]["timeout_ms"], 30000);
}

#[test]
fn managed_claude_uses_file_specific_flag_after_unchanged_startup_tail() {
    let prompt_path = Arc::new(Mutex::new(None::<PathBuf>));
    let prompt_path2 = Arc::clone(&prompt_path);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p8"),
        "agent.start" => {
            let path = assert_startup_prompt_file(
                req,
                &[
                    "--model",
                    "provider/model with space",
                    "--effort",
                    "low",
                    "--permission-mode",
                    "acceptEdits",
                    "--allowedTools",
                    "Bash(board:*)",
                    "--resume",
                    "source-session",
                    "--fork-session",
                ],
                "--append-system-prompt-file",
                "claude system instructions",
            );
            *prompt_path2.lock().unwrap() = Some(path);
            agent_started(req, "w1:p8", false, true)
        }
        method => panic!("unexpected protocol-17 method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let handle = spawner.spawn(&claude_req()).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p8"));
    assert!(!prompt_path.lock().unwrap().as_ref().unwrap().exists());
    let requests = fake.requests.lock().unwrap();
    assert_eq!(requests[3]["params"]["kind"], "claude");
    assert!(requests.iter().all(|r| r["method"] != "agent.prompt"));
}

#[test]
fn managed_existing_tab_splits_selected_pane_before_exact_agent_start() {
    let fake = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
        "tab.list" => existing_tab_list(req),
        "pane.list" => reply(
            req,
            serde_json::json!({"type": "pane_list", "panes": [pane_info("w1:p1")]}),
        ),
        "pane.layout" => reply(
            req,
            serde_json::json!({"type": "pane_layout", "layout": {
                "workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false,
                "area": {"x": 0, "y": 0, "width": 200, "height": 40},
                "focused_pane_id": "w1:p1",
                "panes": [{"pane_id": "w1:p1", "focused": true,
                    "rect": {"x": 0, "y": 0, "width": 200, "height": 40}}],
                "splits": []
            }}),
        ),
        "pane.split" => pane_result(req, "w1:p3"),
        "agent.start" => {
            assert_startup_prompt_file(
                req,
                &[
                    "--model",
                    "provider/model with space",
                    "--session-id",
                    "session-42",
                ],
                "--append-system-prompt",
                "system instructions\nwith an exact second line",
            );
            agent_started(req, "w1:p3", false, true)
        }
        method => panic!("unexpected protocol-17 method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let handle = spawner.spawn(&pi_req(None)).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p3"));

    let requests = fake.requests.lock().unwrap();
    let methods: Vec<_> = requests
        .iter()
        .map(|r| r["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        [
            "ping",
            "tab.list",
            "pane.list",
            "pane.layout",
            "pane.split",
            "agent.start"
        ]
    );
    assert_eq!(requests[4]["params"]["target_pane_id"], "w1:p1");
    assert_eq!(requests[4]["params"]["direction"], "right");
    assert_eq!(requests[4]["params"]["cwd"], "/tmp/card cwd");
    assert_eq!(
        requests[4]["params"]["env"],
        serde_json::json!({"BOARD_CARD_ID": "42"}),
        "split placement must establish the requested child environment",
    );
    assert_eq!(requests[5]["params"]["pane_id"], "w1:p3");
    assert!(!methods.contains(&"pane.focus"));
}

#[test]
fn managed_busy_retry_preserves_exact_start_on_one_new_split_pane() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let starts = Arc::new(AtomicUsize::new(0));
    let starts2 = Arc::clone(&starts);
    let prompt_paths = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let prompt_paths2 = Arc::clone(&prompt_paths);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => existing_tab_list(req),
        "pane.list" => reply(
            req,
            serde_json::json!({"type": "pane_list", "panes": [pane_info("w1:p1")]}),
        ),
        "pane.layout" => reply(
            req,
            serde_json::json!({"type": "pane_layout", "layout": {
                "workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false,
                "area": {"x": 0, "y": 0, "width": 200, "height": 40},
                "focused_pane_id": "w1:p1",
                "panes": [{"pane_id": "w1:p1", "focused": true,
                    "rect": {"x": 0, "y": 0, "width": 200, "height": 40}}],
                "splits": []
            }}),
        ),
        "pane.split" => {
            assert_eq!(req["params"]["target_pane_id"], "w1:p1");
            pane_result(req, "w1:p3")
        }
        "agent.start" => {
            let path = assert_startup_prompt_file(
                req,
                &[
                    "--model",
                    "provider/model with space",
                    "--session-id",
                    "session-42",
                ],
                "--append-system-prompt",
                "system instructions\nwith an exact second line",
            );
            prompt_paths2.lock().unwrap().push(path);
            if starts2.fetch_add(1, Ordering::SeqCst) == 0 {
                error(req, "agent_pane_busy", "pane is still busy")
            } else {
                agent_started(req, "w1:p3", false, true)
            }
        }
        method => panic!("unexpected busy-retry method {method}"),
    });
    let delays = Arc::new(Mutex::new(Vec::new()));
    let delays2 = Arc::clone(&delays);
    let spawner = HerdrSpawner::with_pane_runner_and_delay(
        fake.socket.clone(),
        Arc::new(RecordingPaneRunner {
            calls: Arc::new(Mutex::new(Vec::new())),
            behavior: Box::new(|_, _| unreachable!("managed launch must not use pane runner")),
        }),
        Arc::new(move |delay| delays2.lock().unwrap().push(delay)),
    );

    let handle = spawner.spawn(&pi_req(None)).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p3"));
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    assert_eq!(
        delays.lock().unwrap().as_slice(),
        &[super::super::AGENT_START_BUSY_BACKOFF]
    );

    let requests = fake.requests.lock().unwrap();
    let starts: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "agent.start")
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0]["params"]["pane_id"], "w1:p3");
    assert_eq!(starts[1]["params"]["pane_id"], "w1:p3");
    assert_eq!(starts[0]["params"]["name"], "card-42-execute");
    assert_eq!(starts[1]["params"]["name"], "card-42-execute");
    assert_eq!(starts[0]["params"], starts[1]["params"]);
    let prompt_paths = prompt_paths.lock().unwrap();
    assert_eq!(prompt_paths[0], prompt_paths[1]);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "pane.split")
            .count(),
        1,
        "busy must retry on the owned pane instead of splitting again",
    );
}

#[test]
fn managed_composed_busy_then_name_taken_has_one_global_busy_budget() {
    assert_composed_busy_name_sequence(
        &["busy", "name_taken", "busy", "busy"],
        &[
            "card-42-execute",
            "card-42-execute",
            "card-42-execute-r7",
            "card-42-execute-r7",
        ],
    );
}

#[test]
fn managed_composed_name_taken_then_busy_has_one_global_busy_budget() {
    assert_composed_busy_name_sequence(
        &["name_taken", "busy", "busy", "busy"],
        &[
            "card-42-execute",
            "card-42-execute-r7",
            "card-42-execute-r7",
            "card-42-execute-r7",
        ],
    );
}

fn assert_composed_busy_name_sequence(sequence: &[&str], expected_names: &[&str]) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let starts = Arc::new(AtomicUsize::new(0));
    let starts2 = Arc::clone(&starts);
    let prompt_paths = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let prompt_paths2 = Arc::clone(&prompt_paths);
    let sequence = sequence
        .iter()
        .map(|outcome| (*outcome).to_string())
        .collect::<Vec<_>>();
    let sequence2 = sequence.clone();
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => existing_tab_list(req),
        "pane.list" => reply(
            req,
            serde_json::json!({"type": "pane_list", "panes": [pane_info("w1:p1")]}),
        ),
        "pane.layout" => reply(
            req,
            serde_json::json!({"type": "pane_layout", "layout": {
                "workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false,
                "area": {"x": 0, "y": 0, "width": 200, "height": 40},
                "focused_pane_id": "w1:p1",
                "panes": [{"pane_id": "w1:p1", "focused": true,
                    "rect": {"x": 0, "y": 0, "width": 200, "height": 40}}],
                "splits": []
            }}),
        ),
        "pane.split" => pane_result(req, "w1:p3"),
        "agent.start" => {
            let path = assert_startup_prompt_file(
                req,
                &[
                    "--model",
                    "provider/model with space",
                    "--session-id",
                    "session-42",
                ],
                "--append-system-prompt",
                "system instructions\nwith an exact second line",
            );
            prompt_paths2.lock().unwrap().push(path);
            let call = starts2.fetch_add(1, Ordering::SeqCst);
            match sequence2[call].as_str() {
                "busy" => error(req, "agent_pane_busy", "pane is still busy"),
                "name_taken" => error(req, "agent_name_taken", "agent name is taken"),
                "success" => agent_started(req, "w1:p3", false, true),
                outcome => panic!("unexpected test outcome {outcome}"),
            }
        }
        "pane.close" => pane_result(req, "w1:p3"),
        method => panic!("unexpected composed retry method {method}"),
    });
    let delays = Arc::new(Mutex::new(Vec::new()));
    let delays2 = Arc::clone(&delays);
    let spawner = HerdrSpawner::with_pane_runner_and_delay(
        fake.socket.clone(),
        Arc::new(RecordingPaneRunner {
            calls: Arc::new(Mutex::new(Vec::new())),
            behavior: Box::new(|_, _| unreachable!("managed launch must not use pane runner")),
        }),
        Arc::new(move |delay| delays2.lock().unwrap().push(delay)),
    );

    let err = spawner.spawn(&pi_req(None)).unwrap_err();
    assert!(err.to_string().contains("pane is still busy"));
    assert_eq!(starts.load(Ordering::SeqCst), sequence.len());
    assert_eq!(expected_names.len(), sequence.len());
    assert_eq!(
        delays.lock().unwrap().as_slice(),
        &[
            super::super::AGENT_START_BUSY_BACKOFF,
            super::super::AGENT_START_BUSY_BACKOFF.saturating_mul(2),
        ],
        "busy delays must be globally bounded across the name fallback",
    );

    let requests = fake.requests.lock().unwrap();
    let starts: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "agent.start")
        .collect();
    assert_eq!(starts.len(), sequence.len());
    for (request, expected_name) in starts.iter().zip(expected_names) {
        assert_eq!(request["params"]["name"], *expected_name);
        assert_eq!(request["params"]["pane_id"], "w1:p3");
        assert_eq!(request["params"]["kind"], "pi");
        assert_eq!(request["params"]["timeout_ms"], 30_000);
        assert_eq!(request["params"]["args"], starts[0]["params"]["args"]);
    }
    let prompt_paths = prompt_paths.lock().unwrap();
    assert!(prompt_paths.windows(2).all(|paths| paths[0] == paths[1]));
    assert!(!prompt_paths[0].exists());
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "pane.split")
            .count(),
        1,
        "the name fallback must reuse the one owned pane",
    );
    let closes: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "pane.close")
        .map(|request| request["params"]["pane_id"].as_str().unwrap())
        .collect();
    assert_eq!(closes, ["w1:p3"]);
    assert!(!closes.contains(&"w1:p1"));
}

#[test]
fn pane_split_race_rediscovers_tab_and_splits_a_live_replacement() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let split_calls = Arc::new(AtomicUsize::new(0));
    let split_calls2 = Arc::clone(&split_calls);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => existing_tab_list(req),
        "pane.list" => {
            let pane = if split_calls2.load(Ordering::SeqCst) == 0 {
                "w1:p1"
            } else {
                "w1:p4"
            };
            reply(
                req,
                serde_json::json!({"type": "pane_list", "panes": [pane_info(pane)]}),
            )
        }
        "pane.layout" => {
            let pane = if split_calls2.load(Ordering::SeqCst) == 0 {
                "w1:p1"
            } else {
                "w1:p4"
            };
            reply(
                req,
                serde_json::json!({"type": "pane_layout", "layout": {
                    "workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false,
                    "area": {"x": 0, "y": 0, "width": 200, "height": 40},
                    "focused_pane_id": pane,
                    "panes": [{"pane_id": pane, "focused": true,
                        "rect": {"x": 0, "y": 0, "width": 200, "height": 40}}],
                    "splits": []
                }}),
            )
        }
        "pane.split" => {
            let call = split_calls2.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                error(req, "pane_not_found", "selected pane raced away")
            } else {
                assert_eq!(req["params"]["target_pane_id"], "w1:p4");
                pane_result(req, "w1:p5")
            }
        }
        "agent.start" => agent_started(req, "w1:p5", false, true),
        method => panic!("unexpected protocol-17 method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let handle = spawner.spawn(&pi_req(None)).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p5"));
    let requests = fake.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "tab.list")
            .count(),
        2,
        "a pane.split race must restart tab discovery",
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "pane.split")
            .count(),
        2
    );
}

#[test]
fn listed_tab_disappearing_during_pane_discovery_creates_replacement() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pane_lists = Arc::new(AtomicUsize::new(0));
    let pane_lists2 = Arc::clone(&pane_lists);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => {
            if pane_lists2.load(Ordering::SeqCst) == 0 {
                existing_tab_list(req)
            } else {
                empty_tab_list(req)
            }
        }
        "pane.list" => {
            pane_lists2.fetch_add(1, Ordering::SeqCst);
            error(req, "pane_not_found", "listed tab disappeared")
        }
        "tab.create" => tab_created(req, "w1:p6"),
        "agent.start" => {
            assert_eq!(req["params"]["pane_id"], "w1:p6");
            agent_started(req, "w1:p6", false, true)
        }
        method => panic!("unexpected tab-discovery race method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let handle = spawner.spawn(&pi_req(None)).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p6"));

    let requests = fake.requests.lock().unwrap();
    let methods: Vec<_> = requests
        .iter()
        .map(|r| r["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        [
            "ping",
            "tab.list",
            "pane.list",
            "tab.list",
            "tab.create",
            "agent.start"
        ],
        "a vanished listed tab must trigger bounded full rediscovery"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "tab.create")
            .count(),
        1
    );
}

#[test]
fn name_collision_retries_on_the_same_owned_pane_and_same_prompt_file() {
    let prompt_paths = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let prompt_paths2 = Arc::clone(&prompt_paths);
    let fake = serve_recording_herdr(move |req, index| match req["method"].as_str().unwrap() {
        "tab.list" => empty_tab_list(req),
        "tab.create" => tab_created(req, "w1:p2"),
        "agent.start" => {
            let path = assert_startup_prompt_file(
                req,
                &[
                    "--model",
                    "provider/model with space",
                    "--session-id",
                    "session-42",
                ],
                "--append-system-prompt",
                "system instructions\nwith an exact second line",
            );
            prompt_paths2.lock().unwrap().push(path);
            if index == 2 {
                error(req, "agent_name_taken", "primary name is already used")
            } else {
                agent_started(req, "w1:p2", false, true)
            }
        }
        method => panic!("unexpected protocol-17 method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    spawner.spawn(&pi_req(None)).unwrap();

    let requests = fake.requests.lock().unwrap();
    let starts: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "agent.start")
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0]["params"]["name"], "card-42-execute");
    assert_eq!(starts[1]["params"]["name"], "card-42-execute-r7");
    assert_eq!(starts[0]["params"]["pane_id"], "w1:p2");
    assert_eq!(starts[1]["params"]["pane_id"], "w1:p2");
    let paths = prompt_paths.lock().unwrap();
    assert_eq!(paths[0], paths[1]);
    assert!(!paths[0].exists());
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "tab.create")
            .count(),
        1,
        "fallback owns and reuses the pane already created by the board",
    );
}

#[test]
fn empty_existing_tab_rediscovers_and_launches_in_replacement_tab() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pane_lists = Arc::new(AtomicUsize::new(0));
    let pane_lists2 = Arc::clone(&pane_lists);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => {
            if pane_lists2.load(Ordering::SeqCst) == 0 {
                existing_tab_list(req)
            } else {
                empty_tab_list(req)
            }
        }
        "pane.list" => {
            pane_lists2.fetch_add(1, Ordering::SeqCst);
            reply(req, serde_json::json!({"type": "pane_list", "panes": []}))
        }
        "tab.create" => tab_created(req, "w1:p-race-replacement"),
        "agent.start" => agent_started(req, "w1:p-race-replacement", false, true),
        method => panic!("unexpected empty-tab race method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    let handle = spawner.spawn(&pi_req(None)).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-race-replacement"));
    let methods: Vec<_> = fake
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request["method"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        methods,
        [
            "ping",
            "tab.list",
            "pane.list",
            "tab.list",
            "tab.create",
            "agent.start"
        ],
        "an existing tab that empties during discovery must trigger bounded rediscovery"
    );
}
