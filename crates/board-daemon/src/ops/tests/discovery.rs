use super::*;

#[test]
fn run_focus_rescues_into_a_new_workspace_when_the_recorded_workspace_is_gone() {
    // The run's pane AND its workspace were closed. The rescue must resolve the
    // card's CURRENT space config (new_workspace: label + cwd) and create a
    // fresh workspace in the run's own session, then resume the conversation in
    // an ephemeral pane there. A second focus reuses both the workspace (by
    // label) and the pane (by its marker) — never creating a second of either.
    let fake = fake_rescue_herdr(RescueFakeFaults {
        workspace_gone: true,
        ..Default::default()
    });
    let d = test_daemon_with_herdr_spawner(Config::default(), fake.socket.clone());
    let (card_id, run_id) = add_rescuable_run_with_space(
        &d,
        "pi",
        Some("pi"),
        Some("conv-1"),
        true,
        Some((
            SpaceKind::NewWorkspace,
            "ws-new".to_string(),
            "/tmp/ws-cwd".to_string(),
        )),
    );
    let before = runs_fingerprint(&d, card_id);

    let result = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap();

    assert_eq!(result["action"], "rescued");
    assert_eq!(result["recorded_pane_id"], "w1:p9");
    // The recorded workspace `w1` is gone; the replacement is a fresh `w2`.
    let pane_id = result["pane_id"].as_str().unwrap();
    assert!(pane_id.starts_with("w2:"), "pane_id: {pane_id}");
    assert_eq!(result["session_id"], "conv-1");

    // The replacement workspace was created from the card's CURRENT space
    // config — label and cwd — inside the run's own session.
    let creates = fake.workspace_creates();
    assert_eq!(creates.len(), 1, "exactly one workspace.create");
    assert_eq!(creates[0]["params"]["label"], "ws-new");
    assert_eq!(creates[0]["params"]["cwd"], "/tmp/ws-cwd");
    assert_eq!(fake.workspace_ids(), vec!["w2".to_string()]);
    // The harness was resumed in the fresh workspace with the persisted
    // conversation id and the recorded execution, exactly like an in-workspace
    // rescue: one agent.start, resume argv, no task re-send.
    let starts = fake.agent_starts();
    assert_eq!(starts.len(), 1);
    let args: Vec<String> = starts[0]["params"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    let resume_at = args
        .iter()
        .position(|a| a == "--session-id")
        .expect("--session-id");
    assert_eq!(args[resume_at + 1], "conv-1");
    assert!(args.contains(&"recorded-model".to_string()));
    assert!(!args.iter().any(|a| a.contains("the original task")));
    assert_eq!(
        fake.count("agent.prompt"),
        0,
        "the task must not be re-sent"
    );
    // The rescued pane is in the fresh workspace and carries the run marker.
    let live = fake.pane_ids();
    assert_eq!(live, vec![pane_id.to_string()], "panes: {live:?}");
    let starts = fake.agent_starts();
    assert!(starts[0]["params"]["name"]
        .as_str()
        .unwrap()
        .ends_with("-rescue"));

    // Second focus: the recorded workspace is still gone, but the resolution
    // now finds the replacement by its label and the scan finds the pane the
    // first rescue left there. Nothing new is created.
    let again = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap();
    assert_eq!(again["action"], "focused_rescued_pane");
    assert_eq!(again["pane_id"], pane_id);
    assert_eq!(
        fake.workspace_creates().len(),
        1,
        "a second workspace was created"
    );
    assert_eq!(fake.workspace_ids(), vec!["w2".to_string()]);
    assert_eq!(
        fake.count("pane.split"),
        1,
        "a second rescue pane was created"
    );
    assert_eq!(fake.count("agent.start"), 1, "the harness was restarted");

    // THE central constraint still holds: nothing was written to the database.
    assert_eq!(runs_fingerprint(&d, card_id), before);
    assert_eq!(d.store.lock().list_runs(card_id).unwrap().len(), 1);
}

#[test]
fn run_focus_rescue_failure_closes_the_workspace_it_created() {
    // The workspace was created by this very rescue and the harness then
    // refused to start. A failed rescue must leave NO partial resources: the
    // pane, the adopted card tab AND the workspace it created are all undone,
    // so a later press of `o` resolves and creates a fresh workspace instead of
    // colliding with an empty one that has no live pane cwd.
    let fake = fake_rescue_herdr(RescueFakeFaults {
        workspace_gone: true,
        agent_start_fails: true,
        ..Default::default()
    });
    let d = test_daemon_with_herdr_spawner(Config::default(), fake.socket.clone());
    let (card_id, run_id) = add_rescuable_run_with_space(
        &d,
        "pi",
        Some("pi"),
        Some("conv-1"),
        true,
        Some((
            SpaceKind::NewWorkspace,
            "ws-new".to_string(),
            "/tmp/ws-cwd".to_string(),
        )),
    );
    let before = runs_fingerprint(&d, card_id);

    for attempt in 1..=2 {
        let err = handle_request(
            &d,
            "run.focus",
            json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
        )
        .unwrap_err();
        assert_eq!(err.code(), 4, "attempt {attempt}: {err}");
        assert!(
            err.to_string().contains("harness refused to start"),
            "attempt {attempt}: {err}"
        );
        // A fresh workspace per attempt, and it is closed again on failure —
        // never accumulated, never left empty for the next attempt to trip on.
        assert_eq!(fake.workspace_creates().len(), attempt, "attempt {attempt}");
        assert_eq!(fake.count("workspace.close"), attempt, "attempt {attempt}");
        assert_eq!(
            fake.workspace_ids(),
            Vec::<String>::new(),
            "attempt {attempt}"
        );
        assert_eq!(fake.pane_ids(), Vec::<String>::new(), "attempt {attempt}");
    }
    assert_eq!(runs_fingerprint(&d, card_id), before);
}

#[test]
fn run_focus_rescue_refuses_when_the_card_config_cannot_replace_the_workspace() {
    // The recorded workspace is gone AND the card's current space config points
    // at a workspace that no longer exists (a `workspace`-kind card that still
    // references the closed id). The rescue must refuse explicitly, naming both
    // the dead workspace and the config failure — and create nothing.
    let fake = fake_rescue_herdr(RescueFakeFaults {
        workspace_gone: true,
        ..Default::default()
    });
    let d = test_daemon_with_herdr_spawner(Config::default(), fake.socket.clone());
    let (card_id, run_id) = add_rescuable_run_with_space(
        &d,
        "pi",
        Some("pi"),
        Some("conv-1"),
        true,
        Some((SpaceKind::Workspace, "w1".to_string(), String::new())),
    );
    let before = runs_fingerprint(&d, card_id);

    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"run_id":run_id,"origin_socket":fake.socket}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 4);
    let msg = err.to_string();
    assert!(
        msg.contains("recorded workspace w1"),
        "names the dead workspace: {msg}"
    );
    assert!(
        msg.contains("current space configuration"),
        "names the config dead end: {msg}"
    );
    assert!(
        msg.contains("w1"),
        "names the unresolvable space_ref: {msg}"
    );
    // Nothing was created and nothing was written.
    assert_eq!(fake.workspace_creates().len(), 0);
    assert_eq!(fake.count("pane.split"), 0);
    assert_eq!(runs_fingerprint(&d, card_id), before);
}

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
    // The card's tab and shell anchor survive the dead pane; the rescue splits
    // a fresh child from the anchor, and because this is a MANAGED rescue the
    // anchor is then closed too (the same anchorless convergence dispatch
    // applies), leaving exactly the rescued harness pane in the card tab.
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

    // A managed rescue converges its tab to exactly one harness pane: the
    // pre-existing shell anchor is closed once the resume launch succeeded
    // (`pane_not_found` would count as closed; any other failure warns and
    // keeps the successful rescue). The unrelated user pane and the rescued
    // harness pane are the only panes left.
    let closes = fake.herdr.requests_for("pane.close");
    assert_eq!(
        closes
            .iter()
            .map(|request| request["params"]["pane_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["w1:anchor"],
        "the successful managed rescue closes exactly the card-tab anchor"
    );
    assert_eq!(
        fake.pane_ids(),
        vec!["w1:foreign".to_string(), "w1:rescued1".to_string()],
        "the rescued harness pane and the unrelated user pane survive"
    );

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
    // Pane-first placement puts the run environment on
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
    assert_eq!(
        env.get("BOARD_RUN_ID"),
        Some(&String::new()),
        "a rescued pane must explicitly clear an inherited run credential: {env:?}"
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
        vec!["w1:anchor".to_string(), "w1:foreign".to_string()],
        "only the pre-existing card-tab anchor and the unrelated user pane may survive"
    );
    assert_eq!(runs_fingerprint(&d, card_id), before);
}

#[test]
fn harness_list_builtin_only() {
    let d = test_daemon(Config::default());
    let v = handle_request(&d, "harness.list", json!({})).unwrap();
    let names: Vec<String> = serde_json::from_value(v["harnesses"].clone()).unwrap();
    assert_eq!(
        names,
        vec![
            "pi".to_string(),
            "claude".to_string(),
            "codex".to_string(),
            "opencode".to_string(),
            "antigravity".to_string(),
        ]
    );
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
    assert_eq!(
        names,
        vec!["pi", "claude", "codex", "opencode", "antigravity", "fake"]
    );
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
    // Omitted standard levels remain supported; explicit extended levels are
    // included, all in canonical order.
    let efforts: Vec<&str> = models[0]["efforts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    assert_eq!(
        efforts,
        vec!["off", "minimal", "low", "medium", "high", "xhigh"]
    );
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
fn harness_capabilities_codex_overlays_live_catalog_from_codex_home() {
    // A CODEX_HOME with models_cache.json → the daemon overlays the visible
    // model slugs (per-model supported_reasoning_levels) onto the codex
    // catalog, exactly like the pi overlay. `hide` models and levels the
    // board does not know (`ultra`) never reach the wire.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models_cache.json"),
        r#"{"models": [
          {"slug": "gpt-5.6-sol", "visibility": "list",
           "supported_reasoning_levels": [
             {"effort": "low"}, {"effort": "medium"}, {"effort": "high"},
             {"effort": "xhigh"}, {"effort": "max"}, {"effort": "ultra"}]},
          {"slug": "gpt-5.4", "visibility": "list",
           "supported_reasoning_levels": [{"effort": "none"}, {"effort": "low"}]},
          {"slug": "codex-auto-review", "visibility": "hide",
           "supported_reasoning_levels": [{"effort": "low"}]}
        ]}"#,
    )
    .unwrap();
    let config = Config {
        codex_home: Some(dir.path().to_path_buf()),
        ..Config::default()
    };
    let d = test_daemon(config);

    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "codex" })).unwrap();
    let models = v["models"].as_array().unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["gpt-5.4", "gpt-5.6-sol"]);
    let sol = models.iter().find(|m| m["id"] == "gpt-5.6-sol").unwrap();
    let efforts: Vec<&str> = sol["efforts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    assert_eq!(
        efforts,
        vec!["low", "medium", "high", "xhigh", "max"],
        "ultra is filtered, not added to the protocol"
    );
    let gpt54 = models.iter().find(|m| m["id"] == "gpt-5.4").unwrap();
    let efforts: Vec<&str> = gpt54["efforts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    assert_eq!(
        efforts,
        vec!["off", "low"],
        "codex `none` maps to board `off`"
    );
    // model_freeform stays true: arbitrary model strings are still accepted.
    assert_eq!(v["model_freeform"], true);
    // The preset approval vocabulary is untouched by the overlay.
    let presets: Vec<&str> = v["permission_modes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(
        presets,
        vec!["ask-for-approval", "approve-for-me", "full-access"]
    );
}

#[test]
fn harness_capabilities_codex_falls_back_to_static_without_codex_home() {
    // No codex_home (tests) → static free-form catalog (models: []).
    let d = test_daemon(Config::default());
    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "codex" })).unwrap();
    assert!(v["models"].as_array().unwrap().is_empty());
}

#[test]
fn harness_capabilities_codex_falls_back_to_static_on_malformed_cache() {
    // A malformed models_cache.json must not break codex capabilities: the
    // daemon keeps the static free-form catalog.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("models_cache.json"), "{not json").unwrap();
    let config = Config {
        codex_home: Some(dir.path().to_path_buf()),
        ..Config::default()
    };
    let d = test_daemon(config);
    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "codex" })).unwrap();
    assert!(v["models"].as_array().unwrap().is_empty());
}

/// A fake `opencode` executable printing the verbose model catalog shape.
fn fixture_opencode_bin(dir: &tempfile::TempDir, stdout: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let script = format!("#!/bin/sh\ncat <<'HBEOF'\n{stdout}\nHBEOF\n");
    let bin = dir.path().join("opencode-fixture");
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    bin
}

/// Mirror of `opencode models --verbose` (repeated `provider/model` header
/// lines + one JSON object each, with a per-model `variants` map).
/// `opencode/nemotron-3-ultra-free` declares `variants: {}` for real
/// (verified live): a valid model that stays listed with empty efforts.
const OPENCODE_VERBOSE_FIXTURE: &str = r#"opencode/nemotron-3-ultra-free
{
  "id": "nemotron-3-ultra-free",
  "providerID": "opencode",
  "variants": {}
}
opencode/deepseek-v4-flash-free
{
  "id": "deepseek-v4-flash-free",
  "variants": {
    "low": {"reasoningEffort": "low"},
    "high": {"reasoningEffort": "high"},
    "max": {"reasoningEffort": "max"}
  }
}
openai/gpt-5.6-sol
{
  "id": "gpt-5.6-sol",
  "variants": {
    "low": {"reasoningEffort": "low"},
    "thinking": {"reasoningEffort": "thinking"}
  }
}
"#;

#[test]
fn harness_capabilities_opencode_overlays_live_catalog_from_cli() {
    // An `opencode_bin` resolving to a working CLI → the daemon overlays the
    // live model catalog (per-model variant efforts) onto the opencode
    // capabilities, exactly like the pi/codex overlays. Variants the board
    // does not know (`thinking`) never reach the wire; a valid model with no
    // variants (nemotron) stays listed with EMPTY efforts.
    let dir = tempfile::tempdir().unwrap();
    let bin = fixture_opencode_bin(&dir, OPENCODE_VERBOSE_FIXTURE);
    let config = Config {
        opencode_bin: Some(bin.to_str().unwrap().to_string()),
        ..Config::default()
    };
    let d = test_daemon(config);

    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "opencode" })).unwrap();
    assert_eq!(v["harness"], "opencode");
    let models = v["models"].as_array().unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec![
            "openai/gpt-5.6-sol",
            "opencode/deepseek-v4-flash-free",
            "opencode/nemotron-3-ultra-free",
        ]
    );
    let nemotron = models
        .iter()
        .find(|m| m["id"] == "opencode/nemotron-3-ultra-free")
        .unwrap();
    assert_eq!(
        nemotron["efforts"].as_array().unwrap().len(),
        0,
        "nemotron really has variants {{}} → listed with NO board efforts"
    );
    let deepseek = models
        .iter()
        .find(|m| m["id"] == "opencode/deepseek-v4-flash-free")
        .unwrap();
    let efforts: Vec<&str> = deepseek["efforts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    assert_eq!(
        efforts,
        vec!["low", "high", "max"],
        "verified live variant keys map onto board efforts in canonical order"
    );
    // model_freeform stays true: arbitrary model strings are still accepted.
    assert_eq!(v["model_freeform"], true);
    // The permission vocabulary is untouched by the overlay.
    let modes: Vec<&str> = v["permission_modes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(modes, vec!["default", "auto-approve"]);
}

#[test]
fn harness_capabilities_opencode_falls_back_to_static_without_bin() {
    // No opencode_bin (tests) → the static fallback catalog, which truthfully
    // lists opencode/nemotron-3-ultra-free (empty efforts — real `variants:
    // {}`) plus the fixture model opencode/deepseek-v4-flash-free
    // (low/high/max).
    let d = test_daemon(Config::default());
    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "opencode" })).unwrap();
    let models = v["models"].as_array().unwrap();
    assert_eq!(models.len(), 2, "static fallback models are defined");
    assert_eq!(models[0]["id"], "opencode/nemotron-3-ultra-free");
    assert_eq!(
        models[0]["efforts"].as_array().unwrap().len(),
        0,
        "nemotron offers no board effort"
    );
    assert_eq!(models[1]["id"], "opencode/deepseek-v4-flash-free");
    let efforts: Vec<&str> = models[1]["efforts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    assert_eq!(efforts, vec!["low", "high", "max"]);
}

#[test]
fn harness_capabilities_opencode_falls_back_to_static_on_failing_cli() {
    // A configured bin that fails (missing executable, non-zero exit) must
    // not break opencode capabilities: the daemon keeps the static fallback
    // catalog.
    let config = Config {
        opencode_bin: Some("/nonexistent/opencode-binary".to_string()),
        ..Config::default()
    };
    let d = test_daemon(config);
    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "opencode" })).unwrap();
    assert_eq!(v["models"][0]["id"], "opencode/nemotron-3-ultra-free");
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
    let herdr = fake_herdr_with_protocol(board_herdr::SUPPORTED_HERDR_PROTOCOL - 1);
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
        msg.contains(&format!(
            "Herdr {} with protocol {} is required",
            board_herdr::SUPPORTED_HERDR_VERSION,
            board_herdr::SUPPORTED_HERDR_PROTOCOL
        )),
        "message: {msg}"
    );
    // The gate is the first and only request: workspace.list never happens.
    assert_eq!(herdr.methods(), vec!["ping"]);
}

#[test]
fn run_focus_rejects_a_socket_with_the_wrong_protocol() {
    let herdr = fake_herdr_with_protocol(board_herdr::SUPPORTED_HERDR_PROTOCOL - 1);
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
        msg.contains(&format!(
            "Herdr {} with protocol {} is required",
            board_herdr::SUPPORTED_HERDR_VERSION,
            board_herdr::SUPPORTED_HERDR_PROTOCOL
        )),
        "message: {msg}"
    );
    // The liveness probe for the recorded pane must not reach an incompatible
    // socket, so the whole focus stops at the gate.
    assert_eq!(herdr.methods(), vec!["ping"]);
}

// Antigravity (A7 catalog): the harness is catalog-ONLY — there is
// deliberately no static fallback. The daemon overlays the live
// `agy --output-format json models` catalog (normalized onto base models +
// per-model efforts) when `agy_bin` resolves; a missing/failing CLI yields
// the free-form down state (no models, model_freeform true) so stored
// models keep running.

/// A mirror of the real `agy --output-format json models` envelope: variant
/// ids normalize onto base models, fixed-effort ids stay whole.
const AGY_JSON_FIXTURE: &str = r#"{
  "conversation_id": "",
  "status": "SUCCESS",
  "response": "",
  "command": {
    "name": "models",
    "data": {
      "models": [
        {"id": "gemini-3.7-flash-high", "label": "Gemini 3.7 Flash (High)"},
        {"id": "gemini-3.7-flash-medium", "label": "Gemini 3.7 Flash (Medium)"},
        {"id": "gemini-3.7-flash-low", "label": "Gemini 3.7 Flash (Low)"},
        {"id": "claude-sonnet-4-6", "label": "Claude Sonnet 4.6 (Thinking)"}
      ]
    }
  }
}
"#;

fn fixture_agy_bin(dir: &tempfile::TempDir, stdout: &str) -> std::path::PathBuf {
    let script = format!("#!/bin/sh\ncat <<'HBEOF'\n{stdout}\nHBEOF\n");
    let bin = dir.path().join("agy-fixture");
    std::fs::write(&bin, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    bin
}

#[test]
fn harness_capabilities_antigravity_overlays_live_catalog_from_cli() {
    // An `agy_bin` resolving to a working CLI → the daemon overlays the live
    // normalized catalog and the harness stops being free-form: variant ids
    // merge onto base models, fixed-effort models carry no efforts.
    let dir = tempfile::tempdir().unwrap();
    let bin = fixture_agy_bin(&dir, AGY_JSON_FIXTURE);
    let config = Config {
        agy_bin: Some(bin.to_str().unwrap().to_string()),
        ..Config::default()
    };
    let d = test_daemon(config);

    let v = handle_request(
        &d,
        "harness.capabilities",
        json!({ "harness": "antigravity" }),
    )
    .unwrap();
    assert_eq!(v["harness"], "antigravity");
    let models = v["models"].as_array().unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec!["claude-sonnet-4-6", "gemini-3.7-flash"],
        "variants normalize onto base models, sorted"
    );
    let gemini = models
        .iter()
        .find(|m| m["id"] == "gemini-3.7-flash")
        .unwrap();
    let efforts: Vec<&str> = gemini["efforts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    assert_eq!(efforts, vec!["low", "medium", "high"]);
    let sonnet = models
        .iter()
        .find(|m| m["id"] == "claude-sonnet-4-6")
        .unwrap();
    assert_eq!(
        sonnet["efforts"].as_array().unwrap().len(),
        0,
        "fixed-effort model → no board efforts"
    );
    // Catalog up → authoritative model list.
    assert_eq!(v["model_freeform"], false);
    let modes: Vec<&str> = v["permission_modes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(modes, vec!["current", "sandbox", "always-proceed"]);
}

#[test]
fn harness_capabilities_antigravity_down_without_bin() {
    // No agy_bin (tests) → the free-form down state: no models to offer,
    // model_freeform true (stored models still run), permission ladder intact.
    let d = test_daemon(Config::default());
    let v = handle_request(
        &d,
        "harness.capabilities",
        json!({ "harness": "antigravity" }),
    )
    .unwrap();
    assert_eq!(v["models"].as_array().unwrap().len(), 0);
    assert_eq!(v["model_freeform"], true);
    let modes: Vec<&str> = v["permission_modes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(modes, vec!["current", "sandbox", "always-proceed"]);
}

#[test]
fn harness_capabilities_antigravity_down_on_failing_cli() {
    // A configured bin that fails (missing executable) must not break
    // antigravity capabilities: the harness degrades to the free-form down
    // state, never a stale static list.
    let config = Config {
        agy_bin: Some("/nonexistent/agy-binary".to_string()),
        ..Config::default()
    };
    let d = test_daemon(config);
    let v = handle_request(
        &d,
        "harness.capabilities",
        json!({ "harness": "antigravity" }),
    )
    .unwrap();
    assert_eq!(v["models"].as_array().unwrap().len(), 0);
    assert_eq!(v["model_freeform"], true);
}
