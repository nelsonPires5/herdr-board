use super::*;

fn live_agent_herdr() -> FakeHerdr {
    testkit::herdr_server()
        .on("agent.get", |request| {
            let mut agent = testkit::agent_info("w7:p1", "existing-agent", false, true);
            agent["agent"] = json!("claude");
            agent["agent_status"] = json!("working");
            agent["workspace_id"] = json!("w7");
            agent["agent_session"] = json!({
                "agent": "claude",
                "kind": "id",
                "source": "herdr:claude",
                "value": "session-existing"
            });
            testkit::reply(request, json!({"type": "agent_info", "agent": agent}))
        })
        .serve()
}

#[test]
fn card_adopt_links_a_live_agent_without_starting_or_moving_it() {
    let herdr = live_agent_herdr();
    let d = test_daemon(Config::default());

    let result = handle_request(
        &d,
        "card.adopt",
        json!({
            "title": "existing-agent",
            "description": "Imported from Herdr",
            "pane_id": "w7:p1",
            "origin_socket": herdr.socket,
        }),
    )
    .expect("adopt live agent");

    assert_eq!(result["card"]["status"], "running");
    assert_eq!(result["card"]["harness"], "claude");
    assert_eq!(result["card"]["space_ref"], "w7");
    assert_eq!(result["run"]["herdr_pane_id"], "w7:p1");
    assert_eq!(result["run"]["herdr_workspace_id"], "w7");
    assert_eq!(result["run"]["session_id"], "session-existing");
    assert_eq!(herdr.methods(), vec!["ping", "agent.get"]);
    assert!(
        herdr.requests_for("agent.start").is_empty(),
        "adoption must not start another agent"
    );
    assert_eq!(d.store.lock().list_cards(BOARD_ID).unwrap().len(), 1);
    assert_eq!(d.sched.lock().unwrap().active.len(), 1);
}
