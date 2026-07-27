use super::*;

#[test]
fn run_focus_rejects_missing_pane_and_cross_session_socket() {
    let d = test_daemon(Config::default());
    let (card_id, run_id) = add_run_with_pane(&d, None);
    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":"/tmp/origin.sock"}),
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
    let (card_id, run_id) = add_run_with_pane(&d, Some("w1:p2"));
    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":origin}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 3);
    assert!(err.to_string().contains("different Herdr session"));
}

#[test]
fn run_focus_rejects_a_run_belonging_to_another_card() {
    let d = test_daemon(Config::default());
    let (owner_id, _owner_run) = add_run_with_pane(&d, Some("w1:p1"));
    let (other_id, other_run) = add_run_with_pane(&d, Some("w1:p2"));
    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":owner_id,"run_id":other_run,"origin_socket":"/tmp/origin.sock"}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 2);
    let msg = err.to_string();
    assert!(msg.contains(&other_run.to_string()), "message: {msg}");
    assert!(msg.contains(&owner_id.to_string()), "message: {msg}");
    // The other card's identity is never disclosed.
    assert!(!msg.contains(&format!("card {other_id}")), "leak: {msg}");
}

#[test]
fn run_focus_requires_an_explicit_run_id() {
    let d = test_daemon(Config::default());
    let (card_id, _run_id) = add_run_with_pane(&d, Some("w1:p1"));
    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"origin_socket":"/tmp/origin.sock"}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
}

#[test]
fn run_focus_reports_a_recorded_pane_that_no_longer_exists() {
    // `pane.focus` would still "succeed" in this fake; the run must be refused
    // by the explicit `pane.get` liveness check before that, and never fall
    // back to another run's pane.
    let herdr = fake_herdr_with_pane(
        "\"result\":{\"type\":\"pane_info\",\"pane\":{\"pane_id\":\"w1:p9\",\"terminal_id\":\"term\",\"workspace_id\":\"w1\",\"tab_id\":\"w1:t1\",\"focused\":true,\"revision\":0,\"agent_status\":\"idle\"}}",
        false,
    );
    let socket = herdr.socket.clone();
    let d = test_daemon_with_registry(
        Config::default(),
        Some(SessionRegistry::new(socket.clone())),
    );
    let (card_id, run_id) = add_run_with_pane(&d, Some("w1:p9"));
    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":socket}),
    )
    .unwrap_err();
    // Same class as "no pane recorded", distinct and actionable message.
    assert_eq!(err.code(), 2);
    let msg = err.to_string();
    assert!(msg.contains("no longer exists"), "message: {msg}");
    assert!(msg.contains("w1:p9"), "message: {msg}");
    assert!(msg.contains(&run_id.to_string()), "message: {msg}");
}

#[test]
fn run_focus_propagates_herdr_error_and_returns_success_ids() {
    let herdr = fake_herdr("\"error\":{\"code\":\"pane_not_found\",\"message\":\"gone\"}");
    let socket = herdr.socket.clone();
    let d = test_daemon_with_registry(
        Config::default(),
        Some(SessionRegistry::new(socket.clone())),
    );
    let (card_id, run_id) = add_run_with_pane(&d, Some("w1:p9"));
    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":socket}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 4);
    assert!(err.to_string().contains("gone"));

    let herdr = fake_herdr(
            "\"result\":{\"type\":\"pane_info\",\"pane\":{\"pane_id\":\"w1:p9\",\"terminal_id\":\"term\",\"workspace_id\":\"w1\",\"tab_id\":\"w1:t1\",\"focused\":true,\"revision\":0,\"agent_status\":\"idle\"}}",
        );
    let socket = herdr.socket.clone();
    let d = test_daemon_with_registry(
        Config::default(),
        Some(SessionRegistry::new(socket.clone())),
    );
    let (card_id, run_id) = add_run_with_pane(&d, Some("w1:p9"));
    let result = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":socket}),
    )
    .unwrap();
    assert_eq!(result["pane_id"], "w1:p9");
    assert_eq!(result["run_id"].as_i64().unwrap(), run_id);
    // Full run identity comes back so the caller can name what it focused.
    assert_eq!(result["card_id"].as_i64().unwrap(), card_id);
    assert!(result["column_id"].as_i64().unwrap() > 0);
    assert_eq!(result["harness"], "pi");
    assert!(result["session"].is_null());
    assert!(result["session_id"].is_null());
}

// ---------------------------------------------------------------------------
// run.focus rescue: reopen a run whose pane is gone
// ---------------------------------------------------------------------------

/// Snapshot of everything the `runs` table holds for one card, used to prove a
/// rescue mutates nothing.
fn runs_fingerprint(d: &Arc<Daemon>, card_id: i64) -> String {
    let runs = d.store.lock().list_runs(card_id).unwrap();
    runs.iter()
        .map(|run| {
            format!(
                "{}|{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
                run.id,
                run.card_id,
                run.column_id,
                run.harness,
                run.herdr_workspace_id,
                run.herdr_pane_id,
                run.herdr_anchor_pane_id,
                run.session_id,
                run.started_at,
                run.ended_at,
                run.outcome
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn run_focus_rescues_a_dead_pane_by_resuming_in_a_new_pane_without_touching_the_db() {
    // The card's tab and shell anchor survive; only the run's pane is gone.
    let fake = fake_rescue_herdr(RescueFakeFaults::default());
    let d = test_daemon_with_herdr_spawner(Config::default(), fake.socket.clone());
    let (card_id, run_id) = add_rescuable_run(&d, "claude", Some("claude"), Some("conv-1"), true);
    let before = runs_fingerprint(&d, card_id);

    let result = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap();

    assert_eq!(result["action"], "rescued");
    // The dead pane is reported for diagnostics; the focused pane is the new one.
    assert_eq!(result["recorded_pane_id"], "w1:p9");
    assert_eq!(result["pane_id"], "w1:rescued1");
    assert_eq!(result["run_id"].as_i64().unwrap(), run_id);
    assert_eq!(result["session_id"], "conv-1");

    // The harness was started in *resume* mode with the persisted conversation
    // id, and the original task was NOT re-sent (no agent.prompt at all).
    let starts = fake.agent_starts();
    assert_eq!(starts.len(), 1, "exactly one agent.start");
    let args: Vec<String> = starts[0]["params"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    let resume_at = args.iter().position(|a| a == "--resume").expect("--resume");
    assert_eq!(args[resume_at + 1], "conv-1");
    // The persisted execution environment is preserved, not rebuilt.
    assert!(
        args.contains(&"recorded-model".to_string()),
        "argv: {args:?}"
    );
    assert!(!args.iter().any(|a| a.contains("the original task")));
    assert_eq!(
        fake.count("agent.prompt"),
        0,
        "the task must not be re-sent"
    );
    // The rescued pane is named per-run so a second `o` can find it again.
    let name = starts[0]["params"]["name"].as_str().unwrap();
    assert!(name.ends_with("-rescue"), "name: {name}");
    assert!(name.contains(&format!("card-{card_id}")), "name: {name}");
    assert!(name.contains(&format!("-r{run_id}")), "name: {name}");

    // THE central constraint: a rescue writes nothing. No new run row, no
    // mutation of the historical one.
    assert_eq!(
        runs_fingerprint(&d, card_id),
        before,
        "a rescue wrote to the db"
    );
    assert_eq!(d.store.lock().list_runs(card_id).unwrap().len(), 1);
}

#[test]
fn run_focus_rescue_gives_the_new_pane_the_board_env_but_never_the_run_credential() {
    // Protocol-17 placement is pane-first: the run environment arrives on
    // `pane.split`, not `agent.start`. Without it a harness that reads the board
    // env (every checked-in fixture does, under `set -u`) exits immediately.
    let fake = fake_rescue_herdr(RescueFakeFaults::default());
    let d = test_daemon_with_herdr_spawner(Config::default(), fake.socket.clone());
    let (card_id, run_id) = add_rescuable_run(&d, "pi", Some("pi"), Some("conv-1"), true);

    handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap();

    let env = fake.last_split_env();
    assert_eq!(env.get("BOARD_CARD_ID"), Some(&card_id.to_string()));
    assert_eq!(
        env.get("BOARD_SOCKET").map(String::as_str),
        Some("/tmp/board-test.sock")
    );
    assert!(env.contains_key("BOARD_BIN"), "env: {env:?}");
    assert_eq!(env.get("BOARD_RESCUE"), Some(&"1".to_string()));
    assert_eq!(
        env.get("BOARD_RESUME_SESSION_ID"),
        Some(&"conv-1".to_string())
    );
    // The persisted run environment survives.
    assert_eq!(env.get("RECORDED_ENV"), Some(&"recorded-value".to_string()));
    // The original task is never handed back.
    assert!(!env.contains_key("BOARD_PROMPT"), "env: {env:?}");

    // THE credential rule: `BOARD_RUN_ID` is the actor identity for
    // `board comment`/`done`/`__pane-exited`. A rescued pane belongs to no run
    // and must not be able to write to the immutable historical row, so it is
    // withheld; the id travels as an inert label instead.
    assert!(
        !env.contains_key("BOARD_RUN_ID"),
        "a rescued pane must not receive the run credential: {env:?}"
    );
    assert_eq!(env.get("BOARD_RESCUED_RUN_ID"), Some(&run_id.to_string()));
}

#[test]
fn run_focus_rescue_dedup_survives_a_column_rename() {
    // The marker is the ONLY correlator a rescue may leave (no DB writes), so it
    // must depend on stable identity alone. Deriving it from the column's
    // current name would make a rename resume the same conversation twice.
    let fake = fake_rescue_herdr(RescueFakeFaults::default());
    let d = test_daemon_with_herdr_spawner(Config::default(), fake.socket.clone());
    let (card_id, run_id) = add_rescuable_run(&d, "pi", Some("pi"), Some("conv-1"), true);
    let column_id = d
        .store
        .lock()
        .run_for_card(card_id, run_id)
        .unwrap()
        .column_id;

    let first = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap();
    assert_eq!(first["action"], "rescued");
    let pane = first["pane_id"].as_str().unwrap().to_string();
    // The marker names only the card and the run.
    let marker = fake.agent_starts()[0]["params"]["name"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(marker, format!("card-{card_id}-r{run_id}-rescue"));

    handle_request(
        &d,
        "column.update",
        json!({"id": column_id, "name": "Renamed After The Rescue"}),
    )
    .unwrap();

    let second = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap();
    assert_eq!(
        second["action"], "focused_rescued_pane",
        "a column rename must not hide the pane the rescue just created"
    );
    assert_eq!(second["pane_id"], pane);
    assert_eq!(fake.count("pane.split"), 1);
}

#[test]
fn run_focus_re_rescues_when_the_earlier_rescue_pane_outlived_its_harness() {
    // A Herdr pane label outlives the process. If a label match alone counted as
    // "already rescued", `o` would become a permanent no-op once the resumed
    // harness exited, leaving the user staring at an idle shell forever.
    let fake = fake_rescue_herdr(RescueFakeFaults::default());
    let d = test_daemon_with_herdr_spawner(Config::default(), fake.socket.clone());
    let (card_id, run_id) = add_rescuable_run(&d, "pi", Some("pi"), Some("conv-1"), true);

    let first = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap();
    let dead = first["pane_id"].as_str().unwrap().to_string();
    // The harness exits: Herdr drops the pane's agent, the label survives.
    fake.drop_agent(&dead);

    let second = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap();
    assert_eq!(
        second["action"], "rescued",
        "a leftover label must not be mistaken for a live rescue"
    );
    let fresh = second["pane_id"].as_str().unwrap().to_string();
    assert_ne!(fresh, dead);
    assert_eq!(
        fake.count("agent.start"),
        2,
        "the harness was started again"
    );
    // The dead shell is reclaimed rather than accumulating: no run row exists
    // that could ever collect it.
    let live = fake.pane_ids();
    assert!(
        !live.contains(&dead),
        "dead rescue pane left behind: {live:?}"
    );
    assert!(live.contains(&fresh));
}

#[test]
fn run_focus_rescue_that_created_a_card_tab_removes_it_again_on_failure() {
    // With the anchor gone, placement has to `tab.create`. If the launch then
    // fails, closing only the child would orphan an empty `card-<id>` tab that
    // nothing can ever reclaim — a rescue leaves no run row and never retries.
    let fake = fake_rescue_herdr(RescueFakeFaults {
        anchor_missing: true,
        agent_start_fails: true,
        ..Default::default()
    });
    let d = test_daemon_with_herdr_spawner(Config::default(), fake.socket.clone());
    let (card_id, run_id) = add_rescuable_run(&d, "pi", Some("pi"), Some("conv-1"), true);
    let before = runs_fingerprint(&d, card_id);

    for attempt in 1..=2 {
        let err = handle_request(
            &d,
            "run.focus",
            json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
        )
        .unwrap_err();
        assert_eq!(err.code(), 4, "attempt {attempt}: {err}");
        assert_eq!(fake.count("tab.create"), attempt, "attempt {attempt}");
        // Nothing this rescue created survives — neither the child nor the tab
        // root — so repeated presses of `o` cannot accumulate orphan tabs. The
        // unrelated user pane is of course untouched.
        assert_eq!(
            fake.pane_ids(),
            vec!["w1:foreign".to_string()],
            "attempt {attempt} left the rescue's tab behind"
        );
    }
    assert_eq!(runs_fingerprint(&d, card_id), before);
}

#[test]
fn run_focus_rescue_is_idempotent_and_never_leaves_two_panes() {
    let fake = fake_rescue_herdr(RescueFakeFaults::default());
    let d = test_daemon_with_herdr_spawner(Config::default(), fake.socket.clone());
    let (card_id, run_id) = add_rescuable_run(&d, "pi", Some("pi"), Some("conv-1"), true);

    let first = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap();
    assert_eq!(first["action"], "rescued");
    let pane = first["pane_id"].as_str().unwrap().to_string();

    // Pressing `o` again finds the pane the first rescue created (by its
    // name/label) and focuses it instead of splitting a second one.
    let second = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap();
    assert_eq!(second["action"], "focused_rescued_pane");
    assert_eq!(second["pane_id"], pane);
    assert_eq!(fake.count("pane.split"), 1, "a second pane was created");
    assert_eq!(fake.count("agent.start"), 1, "the harness was restarted");
}

#[test]
fn run_focus_refuses_to_rescue_a_harness_without_resume_support() {
    let fake = fake_rescue_herdr(RescueFakeFaults::default());
    let mut config = Config::default();
    // Declared, but WITHOUT `resume = true`: the default is unsupported.
    config.harness.insert(
        "custom".to_string(),
        HarnessDef {
            argv: vec!["bash".into(), "custom.sh".into()],
            ..Default::default()
        },
    );
    let d = test_daemon_with_herdr_spawner(config, fake.socket.clone());
    let (card_id, run_id) = add_rescuable_run(&d, "custom", None, Some("conv-1"), true);
    let before = runs_fingerprint(&d, card_id);

    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 3);
    let msg = err.to_string();
    assert!(msg.contains("custom"), "names the harness: {msg}");
    assert!(msg.contains("resum"), "explains why: {msg}");
    // Never a fresh conversation as a fallback, and never a db write.
    assert_eq!(fake.count("agent.start"), 0);
    assert_eq!(fake.count("pane.split"), 0);
    assert_eq!(runs_fingerprint(&d, card_id), before);
}

#[test]
fn run_focus_rescues_a_configured_harness_that_opts_into_resume() {
    let fake = fake_rescue_herdr(RescueFakeFaults::default());
    let mut config = Config::default();
    config.harness.insert(
        "custom".to_string(),
        HarnessDef {
            argv: vec!["bash".into(), "custom.sh".into()],
            resume: true,
            ..Default::default()
        },
    );
    let d = test_daemon_with_herdr_spawner(config, fake.socket.clone());
    let (card_id, run_id) = add_rescuable_run(&d, "custom", None, Some("conv-1"), true);

    // A configured harness is unmanaged, so its launch goes through the
    // `herdr pane run` bridge rather than `agent.start`; the daemon reaches
    // the rename+launch step, which is as far as this hermetic fake goes.
    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap_err();
    // Not a capability refusal: the opt-in was accepted and placement happened.
    assert_eq!(err.code(), 4, "{err}");
    assert_eq!(fake.count("pane.split"), 1, "the rescue pane was created");
    // Failure is non-destructive: the pane it created is closed again. (This
    // fake cannot emulate the external `herdr pane run` bridge a configured
    // harness needs, so the launch itself always fails here; what matters is
    // that the opt-in was honoured and the cleanup ran.)
    assert!(fake.count("pane.close") >= 1);
}

#[test]
fn run_focus_refuses_to_rescue_a_run_without_a_recorded_conversation_id() {
    let fake = fake_rescue_herdr(RescueFakeFaults::default());
    let d = test_daemon_with_herdr_spawner(Config::default(), fake.socket.clone());
    let (card_id, run_id) = add_rescuable_run(&d, "pi", Some("pi"), None, true);
    let before = runs_fingerprint(&d, card_id);

    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 2);
    let msg = err.to_string();
    assert!(msg.contains("conversation id"), "message: {msg}");
    assert!(msg.contains("w1:p9"), "names the dead pane: {msg}");
    assert_eq!(fake.count("pane.split"), 0);
    assert_eq!(runs_fingerprint(&d, card_id), before);
}

#[test]
fn run_focus_rescue_reports_pane_creation_failure_without_writing_anything() {
    let fake = fake_rescue_herdr(RescueFakeFaults {
        split_fails: true,
        ..Default::default()
    });
    let d = test_daemon_with_herdr_spawner(Config::default(), fake.socket.clone());
    let (card_id, run_id) = add_rescuable_run(&d, "pi", Some("pi"), Some("conv-1"), true);
    let before = runs_fingerprint(&d, card_id);

    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 4);
    assert!(err.to_string().contains(&run_id.to_string()), "{err}");
    assert_eq!(fake.count("agent.start"), 0);
    assert_eq!(runs_fingerprint(&d, card_id), before);
}

#[test]
fn run_focus_rescue_closes_its_pane_when_the_harness_will_not_start() {
    let fake = fake_rescue_herdr(RescueFakeFaults {
        agent_start_fails: true,
        ..Default::default()
    });
    let d = test_daemon_with_herdr_spawner(Config::default(), fake.socket.clone());
    let (card_id, run_id) = add_rescuable_run(&d, "pi", Some("pi"), Some("conv-1"), true);
    let before = runs_fingerprint(&d, card_id);

    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 4);
    assert!(
        err.to_string().contains("harness refused to start"),
        "{err}"
    );
    // Non-destructive: the half-built rescue pane is closed again. Here the card
    // tab already existed, so that child is the only thing this rescue created.
    // The case where placement also had to create the tab — and must therefore
    // remove the tab too, or repeated presses of `o` orphan one each time — is
    // `run_focus_rescue_that_created_a_card_tab_removes_it_again_on_failure`.
    assert_eq!(fake.count("pane.close"), 1);
    assert_eq!(
        fake.pane_ids(),
        vec!["w1:anchor".to_string()],
        "only the pre-existing card-tab anchor may survive"
    );
    assert_eq!(runs_fingerprint(&d, card_id), before);
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
            resume: false,
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

#[test]
fn space_list_rejects_a_socket_with_the_wrong_protocol() {
    let herdr = fake_herdr_with_protocol(16);
    // Seed the listing: resolving the default session otherwise shells out to
    // `herdr session list --json`, which makes this assert on whatever herdr is
    // on PATH rather than on the protocol gate — green locally, red in CI.
    let d = test_daemon_with_registry(
        Config::default(),
        Some(SessionRegistry::with_entries(
            herdr.socket.clone(),
            vec![SessionEntry {
                name: "default".to_string(),
                default: true,
                running: true,
                socket_path: herdr.socket.display().to_string(),
            }],
        )),
    );
    let err = handle_request(&d, "space.list", json!({})).unwrap_err();
    assert_eq!(err.code(), 4);
    let msg = err.to_string();
    assert!(
        msg.contains("Herdr 0.7.5 with protocol 17 is required"),
        "message: {msg}"
    );
    // The gate is the first and only request: workspace.list never happens.
    assert_eq!(herdr.methods(), vec!["ping"]);
}

#[test]
fn run_focus_rejects_a_socket_with_the_wrong_protocol() {
    let herdr = fake_herdr_with_protocol(16);
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path().join("origin.sock");
    std::os::unix::fs::symlink(&herdr.socket, &origin).unwrap();
    let d = test_daemon_with_registry(
        Config::default(),
        Some(SessionRegistry::new(herdr.socket.clone())),
    );
    let (card_id, run_id) = add_run_with_pane(&d, Some("w1:p1"));
    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":origin}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 4);
    let msg = err.to_string();
    assert!(
        msg.contains("Herdr 0.7.5 with protocol 17 is required"),
        "message: {msg}"
    );
    // The liveness probe for the recorded pane must not reach an incompatible
    // socket, so the whole focus stops at the gate.
    assert_eq!(herdr.methods(), vec!["ping"]);
}
