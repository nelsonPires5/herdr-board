use super::*;

#[test]
fn run_focus_rejects_missing_pane_and_cross_session_socket() {
    let d = test_daemon(Config::default());
    let card_id = add_run_with_pane(&d, None);
    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"origin_socket":"/tmp/origin.sock"}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 2);
    assert!(err.to_string().contains("pane"));

    let target_dir = tempfile::tempdir().unwrap();
    let target = target_dir.path().join("target.sock");
    let _listener = UnixListener::bind(&target).unwrap();
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path().join("origin.sock");
    let _origin_listener = UnixListener::bind(&origin).unwrap();
    let d = test_daemon_with_registry(
        Config::default(),
        Some(SessionRegistry::new(target.clone())),
    );
    let card_id = add_run_with_pane(&d, Some("w1:p2"));
    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"origin_socket":origin}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 3);
    assert!(err.to_string().contains("different Herdr session"));
}

#[test]
fn run_focus_propagates_herdr_error_and_returns_success_ids() {
    let (_dir, socket) = fake_herdr("\"error\":{\"code\":\"pane_not_found\",\"message\":\"gone\"}");
    let d = test_daemon_with_registry(
        Config::default(),
        Some(SessionRegistry::new(socket.clone())),
    );
    let card_id = add_run_with_pane(&d, Some("w1:p9"));
    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"origin_socket":socket}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 4);
    assert!(err.to_string().contains("gone"));

    let (_dir, socket) = fake_herdr(
            "\"result\":{\"type\":\"pane_info\",\"pane\":{\"pane_id\":\"w1:p9\",\"terminal_id\":\"term\",\"workspace_id\":\"w1\",\"tab_id\":\"w1:t1\",\"focused\":true,\"revision\":0,\"agent_status\":\"idle\"}}",
        );
    let d = test_daemon_with_registry(
        Config::default(),
        Some(SessionRegistry::new(socket.clone())),
    );
    let card_id = add_run_with_pane(&d, Some("w1:p9"));
    let result = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"origin_socket":socket}),
    )
    .unwrap();
    assert_eq!(result["pane_id"], "w1:p9");
    assert!(result["run_id"].as_i64().unwrap() > 0);
}

#[test]
fn harness_list_builtin_only() {
    let d = test_daemon(Config::default());
    let v = handle_request(&d, "harness.list", json!({})).unwrap();
    let names: Vec<String> = serde_json::from_value(v["harnesses"].clone()).unwrap();
    assert_eq!(names, vec!["pi".to_string(), "claude".to_string()]);
}

#[test]
fn harness_list_includes_config_defined() {
    let mut config = Config::default();
    config.harness.insert(
        "fake".to_string(),
        HarnessDef {
            argv: vec!["bash".into(), "fake.sh".into()],
            ..Default::default()
        },
    );
    let d = test_daemon(config);
    let v = handle_request(&d, "harness.list", json!({})).unwrap();
    let names: Vec<String> = serde_json::from_value(v["harnesses"].clone()).unwrap();
    assert_eq!(names, vec!["pi", "claude", "fake"]);
}

#[test]
fn harness_capabilities_claude_ok() {
    let d = test_daemon(Config::default());
    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "claude" })).unwrap();
    assert_eq!(v["harness"], "claude");
    assert_eq!(v["model_freeform"], true);
    assert!(v["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == "sonnet"));
}

#[test]
fn harness_capabilities_pi_ok() {
    let d = test_daemon(Config::default());
    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "pi" })).unwrap();
    assert_eq!(v["harness"], "pi");
    assert_eq!(v["model_freeform"], true);
    assert!(v["models"].as_array().unwrap().is_empty());
    assert!(v["permission_modes"].as_array().unwrap().is_empty());
    assert!(v["default_efforts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|effort| effort == "low"));
}

#[test]
fn harness_capabilities_pi_overlays_live_catalog() {
    // A pi agent dir with auth.json + models-store.json → the daemon
    // overlays real models (per-model efforts) onto the pi catalog.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("auth.json"),
        r#"{"zai": {"type": "api_key"}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("models-store.json"),
        r#"{"zai": {"models": [{"id": "glm-5.2", "reasoning": true,
                 "thinkingLevelMap": {"minimal": "low", "xhigh": "xhigh"}}]}}"#,
    )
    .unwrap();
    let config = Config {
        pi_agent_dir: Some(dir.path().to_path_buf()),
        ..Config::default()
    };
    let d = test_daemon(config);

    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "pi" })).unwrap();
    let models = v["models"].as_array().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["id"], "zai/glm-5.2");
    // Per-model efforts come from thinkingLevelMap, in canonical order.
    let efforts: Vec<&str> = models[0]["efforts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    assert_eq!(efforts, vec!["minimal", "xhigh"]);
    // model_freeform stays true: arbitrary model strings are still accepted.
    assert_eq!(v["model_freeform"], true);
}

#[test]
fn harness_capabilities_pi_falls_back_to_static_without_agent_dir() {
    // No pi_agent_dir (tests) → static free-form catalog (models: []).
    let d = test_daemon(Config::default());
    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "pi" })).unwrap();
    assert!(v["models"].as_array().unwrap().is_empty());
}

#[test]
fn harness_capabilities_config_defined_ok() {
    let mut config = Config::default();
    config.harness.insert(
        "fake".to_string(),
        HarnessDef {
            argv: vec!["bash".into(), "fake.sh".into()],
            models: vec!["m1".into()],
            efforts: vec!["low".into()],
            permission_modes: vec!["auto".into()],
        },
    );
    let d = test_daemon(config);
    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "fake" })).unwrap();
    assert_eq!(v["harness"], "fake");
    assert_eq!(v["permission_modes"][0], "auto");
}

#[test]
fn harness_capabilities_unknown_is_not_found() {
    let d = test_daemon(Config::default());
    let err =
        handle_request(&d, "harness.capabilities", json!({ "harness": "ghost" })).unwrap_err();
    assert_eq!(err.code(), 2);
    let msg = err.to_string();
    assert!(msg.contains("ghost"), "message: {msg}");
    assert!(msg.contains("pi"), "message lists Pi: {msg}");
    assert!(msg.contains("claude"), "message lists Claude: {msg}");
}

#[test]
fn space_list_without_herdr_is_herdr_unavailable() {
    let d = test_daemon(Config::default());
    let err = handle_request(&d, "space.list", json!({})).unwrap_err();
    assert_eq!(err.code(), 4);
}
