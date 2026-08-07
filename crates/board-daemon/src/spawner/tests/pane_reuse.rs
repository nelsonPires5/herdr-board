//! Pane reuse on same-conversation resume hops: the next run re-uses the prior
//! run's still-live agent pane (no fresh `pane.split`, no `agent.start`, just
//! `agent.prompt` of the new stage), while a fresh hop still splits a new pane.
//!
//! T5 (launch reuse mode) is covered by `resume_hop_reuses_...` below: the
//! recorded method sequence proves no `agent.start` and exactly one
//! `agent.prompt`.

use super::*;
use serde_json::json;

// A reused pane is already interactive and quiescent: Herdr may expose the
// prior turn as either idle or derived done. `agent_status` defaults to unknown
// in `testkit::agent_info`, so reuse readiness is asserted explicitly here.
fn reuse_agent_ready(req: &Value, pane_id: &str, kind: &str, status: &str) -> Value {
    reply(
        req,
        json!({"type":"agent_info","agent":{
            "pane_id": pane_id, "agent": kind, "agent_status": status,
            "interactive_ready": true, "launch_pending": false,
            "focused": false, "revision": 2
        }}),
    )
}

// T1 + T5: a same-conversation resume hop reuses the prior run's pane.
#[test]
fn resume_hop_reuses_the_prior_run_pane_without_split_or_agent_start() {
    let fake = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
        "tab.list" => reply(
            req,
            json!({"type":"tab_list","tabs":[{
                "tab_id":"w1:t1","workspace_id":"w1","number":1,
                "label":"card-42","pane_count":2
            }]}),
        ),
        "pane.list" => {
            let mut anchor = pane_info("w1:p-anchor");
            anchor["label"] = json!("card-42-anchor");
            anchor["agent_status"] = json!("idle");
            let mut prior = pane_info("w1:p-prior");
            prior["label"] = json!("card-42-setup");
            prior["agent"] = json!("pi");
            prior["agent_status"] = json!("done");
            reply(req, json!({"type":"pane_list","panes":[anchor, prior]}))
        }
        "agent.get" => reuse_agent_ready(req, "w1:p-prior", "pi", "done"),
        "agent.prompt" => agent_prompted(req, "w1:p-prior", "pi"),
        method => panic!("unexpected reuse method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let mut request = pi_req(Some("next stage task"));
    request.tab_label = Some("card-42".into());
    request.owned_tab_id = Some("w1:t1".into());
    request.durable_anchor_pane_ids = vec!["w1:p-anchor".into()];
    request.durable_pane_ids = vec!["w1:p-prior".into()];
    request.reclaimable_pane_ids = vec!["w1:p-prior".into()];
    request.reuse_pane_id = Some("w1:p-prior".into());

    let handle = spawner.spawn(&request).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-prior"));
    assert_eq!(handle.anchor_pane_id.as_deref(), Some("w1:p-anchor"));

    let requests = fake.requests.lock().unwrap();
    let methods: Vec<&str> = requests
        .iter()
        .map(|request| request["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        ["ping", "tab.list", "pane.list", "agent.get", "agent.prompt"],
        "reuse re-prompts the live agent; it neither splits nor starts"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "pane.split")
            .count(),
        0,
        "reuse must not split a new pane"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "agent.start")
            .count(),
        0,
        "reuse must not start a new agent (the name is already taken)"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "pane.close")
            .count(),
        0,
        "reuse must not close the prior pane"
    );
    let prompt = requests
        .iter()
        .find(|request| request["method"] == "agent.prompt")
        .unwrap();
    assert_eq!(prompt["params"]["target"], "w1:p-prior");
    assert_eq!(prompt["params"]["text"], "next stage task");
}

// T2 (fresh regression): without a reuse candidate, a fresh hop still splits a
// new pane, reclaims the prior child, and starts a new agent.
#[test]
fn fresh_hop_with_no_reuse_candidate_splits_a_new_pane_and_reclaims_the_prior() {
    let closed = Arc::new(Mutex::new(Vec::<String>::new()));
    let closed_for_server = Arc::clone(&closed);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => reply(
            req,
            json!({"type":"tab_list","tabs":[{
                "tab_id":"w1:t1","workspace_id":"w1","number":1,
                "label":"card-42","pane_count":2
            }]}),
        ),
        "pane.list" => {
            let mut anchor = pane_info("w1:p-anchor");
            anchor["label"] = json!("card-42-anchor");
            anchor["agent_status"] = json!("idle");
            let mut prior = pane_info("w1:p-prior");
            prior["label"] = json!("card-42-setup");
            prior["agent"] = json!("card-42-setup");
            prior["agent_status"] = json!("idle");
            reply(req, json!({"type":"pane_list","panes":[anchor, prior]}))
        }
        "pane.layout" => reply(
            req,
            json!({"type":"pane_layout","layout":{
                "workspace_id":"w1","tab_id":"w1:t1","zoomed":false,
                "area":{"x":0,"y":0,"width":200,"height":40},"focused_pane_id":"w1:p-anchor",
                "panes":[{"pane_id":"w1:p-anchor","focused":true,
                    "rect":{"x":0,"y":0,"width":200,"height":40}}],"splits":[]
            }}),
        ),
        "pane.close" => {
            let pane_id = req["params"]["pane_id"].as_str().unwrap().to_string();
            closed_for_server.lock().unwrap().push(pane_id);
            pane_result(req, "w1:p-prior")
        }
        "pane.split" => pane_result(req, "w1:p-fresh"),
        "agent.start" => agent_started(req, "w1:p-fresh", false, true),
        method => panic!("unexpected fresh-hop method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let mut request = pi_req(None);
    request.tab_label = Some("card-42".into());
    request.owned_tab_id = Some("w1:t1".into());
    request.durable_anchor_pane_ids = vec!["w1:p-anchor".into()];
    request.durable_pane_ids = vec!["w1:p-prior".into()];
    request.reclaimable_pane_ids = vec!["w1:p-prior".into()];
    request.reuse_pane_id = None;

    let handle = spawner.spawn(&request).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-fresh"));

    let requests = fake.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "pane.split")
            .count(),
        1,
        "a fresh hop splits exactly one new pane"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "agent.start")
            .count(),
        1,
        "a fresh hop starts a new agent on the new pane"
    );
    assert_eq!(*closed.lock().unwrap(), vec!["w1:p-prior"]);
}

#[test]
fn reuse_candidate_with_a_different_agent_kind_is_reclaimed_and_replaced() {
    let fake = serve_recording_herdr(|req, _| match req["method"].as_str().unwrap() {
        "tab.list" => reply(
            req,
            json!({"type":"tab_list","tabs":[{
                "tab_id":"w1:t1","workspace_id":"w1","number":1,
                "label":"card-42","pane_count":2
            }]}),
        ),
        "pane.list" => {
            let mut anchor = pane_info("w1:p-anchor");
            anchor["label"] = json!("card-42-anchor");
            anchor["agent_status"] = json!("idle");
            let mut prior = pane_info("w1:p-prior");
            prior["agent"] = json!("claude");
            prior["agent_status"] = json!("done");
            reply(req, json!({"type":"pane_list","panes":[anchor, prior]}))
        }
        "pane.layout" => reply(
            req,
            json!({"type":"pane_layout","layout":{
                "workspace_id":"w1","tab_id":"w1:t1","zoomed":false,
                "area":{"x":0,"y":0,"width":200,"height":40},
                "focused_pane_id":"w1:p-anchor",
                "panes":[{"pane_id":"w1:p-anchor","focused":true,
                    "rect":{"x":0,"y":0,"width":200,"height":40}}],"splits":[]
            }}),
        ),
        "pane.close" => pane_result(req, "w1:p-prior"),
        "pane.split" => pane_result(req, "w1:p-new"),
        "agent.start" => agent_started(req, "w1:p-new", false, true),
        method => panic!("unexpected different-kind method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());
    let mut request = pi_req(None);
    request.tab_label = Some("card-42".into());
    request.owned_tab_id = Some("w1:t1".into());
    request.durable_anchor_pane_ids = vec!["w1:p-anchor".into()];
    request.durable_pane_ids = vec!["w1:p-prior".into()];
    request.reclaimable_pane_ids = vec!["w1:p-prior".into()];
    request.reuse_pane_id = Some("w1:p-prior".into());

    let handle = spawner.spawn(&request).unwrap();
    assert_eq!(handle.pane_id.as_deref(), Some("w1:p-new"));
    let requests = fake.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "pane.close")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "pane.split")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "agent.start")
            .count(),
        1
    );
    assert!(!requests
        .iter()
        .any(|request| request["method"] == "agent.prompt"));
}

// T4: a non-fresh chain reuses one agent pane across hops — exactly one split
// (the fresh hop) and one agent.start, never closing the reused pane.
#[test]
fn a_non_fresh_chain_keeps_one_agent_pane_reusing_it_each_hop() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let tab_made = Arc::new(AtomicBool::new(false));
    let tab_made_for_server = Arc::clone(&tab_made);
    let splits = Arc::new(AtomicUsize::new(0));
    let splits_for_server = Arc::clone(&splits);
    let starts = Arc::new(AtomicUsize::new(0));
    let starts_for_server = Arc::clone(&starts);
    let prompts = Arc::new(AtomicUsize::new(0));
    let prompts_for_server = Arc::clone(&prompts);
    let fake = serve_recording_herdr(move |req, _| match req["method"].as_str().unwrap() {
        "tab.list" => {
            let tabs = if tab_made_for_server.load(Ordering::SeqCst) {
                json!([{"tab_id":"w1:t1","workspace_id":"w1","number":1,
                    "label":"card-42","pane_count":2}])
            } else {
                json!([])
            };
            reply(req, json!({"type":"tab_list","tabs":tabs}))
        }
        "tab.create" => {
            tab_made_for_server.store(true, Ordering::SeqCst);
            tab_created(req, "w1:p-anchor")
        }
        "pane.rename" => pane_result(req, req["params"]["pane_id"].as_str().unwrap()),
        "pane.list" => {
            let mut anchor = pane_info("w1:p-anchor");
            anchor["label"] = json!("card-42-anchor");
            anchor["agent_status"] = json!("idle");
            let mut child = pane_info("w1:p-child");
            child["label"] = json!("card-42-execute");
            child["agent"] = json!("pi");
            child["agent_status"] = json!("idle");
            reply(req, json!({"type":"pane_list","panes":[anchor, child]}))
        }
        "pane.layout" => reply(
            req,
            json!({"type":"pane_layout","layout":{
                "workspace_id":"w1","tab_id":"w1:t1","zoomed":false,
                "area":{"x":0,"y":0,"width":200,"height":40},"focused_pane_id":"w1:p-anchor",
                "panes":[{"pane_id":"w1:p-anchor","focused":true,
                    "rect":{"x":0,"y":0,"width":200,"height":40}}],"splits":[]
            }}),
        ),
        "pane.split" => {
            splits_for_server.fetch_add(1, Ordering::SeqCst);
            pane_result(req, "w1:p-child")
        }
        "pane.close" => pane_result(req, req["params"]["pane_id"].as_str().unwrap()),
        "agent.start" => {
            starts_for_server.fetch_add(1, Ordering::SeqCst);
            agent_started(req, "w1:p-child", false, true)
        }
        "agent.get" => reuse_agent_ready(req, "w1:p-child", "pi", "idle"),
        "agent.prompt" => {
            prompts_for_server.fetch_add(1, Ordering::SeqCst);
            agent_prompted(req, "w1:p-child", "pi")
        }
        method => panic!("unexpected chain method {method}"),
    });
    let spawner = HerdrSpawner::new(fake.socket.clone());

    // Hop 1 (fresh): creates the tab + anchor + child and starts the agent.
    let mut first = pi_req(None);
    first.tab_label = Some("card-42".into());
    let handle_one = spawner.spawn(&first).unwrap();
    assert_eq!(handle_one.pane_id.as_deref(), Some("w1:p-child"));

    // Hop 2 (resume): reuses the child pane — no split, no agent.start.
    let mut resume = pi_req(Some("stage two"));
    resume.tab_label = Some("card-42".into());
    resume.owned_tab_id = Some("w1:t1".into());
    resume.durable_anchor_pane_ids = vec!["w1:p-anchor".into()];
    resume.durable_pane_ids = vec!["w1:p-child".into()];
    resume.reclaimable_pane_ids = vec!["w1:p-child".into()];
    resume.reuse_pane_id = Some("w1:p-child".into());
    let handle_two = spawner.spawn(&resume).unwrap();
    assert_eq!(handle_two.pane_id.as_deref(), Some("w1:p-child"));

    // Hop 3 (resume): reuses it again.
    let handle_three = spawner.spawn(&resume).unwrap();
    assert_eq!(handle_three.pane_id.as_deref(), Some("w1:p-child"));

    assert_eq!(
        splits.load(Ordering::SeqCst),
        1,
        "the chain splits exactly once (the fresh hop)"
    );
    assert_eq!(
        starts.load(Ordering::SeqCst),
        1,
        "agent.start runs exactly once (the fresh hop)"
    );
    assert_eq!(
        prompts.load(Ordering::SeqCst),
        2,
        "each resume hop re-prompts the same pane"
    );
    let requests = fake.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "pane.close")
            .count(),
        0,
        "the reused pane is never closed across the chain"
    );
}
