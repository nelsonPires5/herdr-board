//! Placement races and retries: a split target or listed tab that disappears
//! mid-placement is rediscovered once, and a taken agent name retries on the
//! same owned pane with the same startup prompt file.

use super::*;

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
        method => panic!("unexpected supported-contract method {method}"),
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
        method => panic!("unexpected supported-contract method {method}"),
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
