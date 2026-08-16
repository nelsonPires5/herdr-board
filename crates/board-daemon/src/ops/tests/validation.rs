use super::*;

#[test]
fn merged_invalid_updates_are_atomic_and_emit_no_event() {
    let d = test_daemon(Config::default());
    let mut events = d.events_tx.subscribe();
    let created = handle_request(
        &d,
        "card.create",
        json!({
            "title": "valid settings",
            "harness": "claude",
            "model": "sonnet",
            "effort": "high",
            "permission_mode": "manual",
            "space_kind": "new_workspace",
            "space_ref": "feature",
            "space_cwd": "/repo"
        }),
    )
    .unwrap();
    let card_id = created["id"].as_i64().unwrap();
    let _ = events.try_recv().expect("create event");

    let err = handle_request(
        &d,
        "card.update",
        json!({
            "id": card_id,
            "space_kind": "new_workspace",
            "space_cwd": null
        }),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    testkit::assert_no_events(&mut events);
    let unchanged = d.store.lock().get_card(card_id).unwrap().unwrap();
    assert_eq!(unchanged.space_ref.as_deref(), Some("feature"));
    assert_eq!(unchanged.space_cwd.as_deref(), Some("/repo"));
}

#[test]
fn invalid_column_update_keeps_dependents_and_emits_no_event() {
    let d = test_daemon(Config::default());
    let mut events = d.events_tx.subscribe();
    let created = handle_request(
        &d,
        "column.create",
        json!({
            "name": "validated",
            "harness_override": "claude",
            "model_override": "sonnet",
            "effort_override": "high",
            "permission_override": "manual"
        }),
    )
    .unwrap();
    let id = created["id"].as_i64().unwrap();
    let _ = events.try_recv().expect("create event");
    let err = handle_request(
        &d,
        "column.update",
        json!({"id": id, "harness_override": null}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    testkit::assert_no_events(&mut events);
    let unchanged = d.store.lock().get_column(id).unwrap().unwrap();
    assert_eq!(unchanged.harness_override.as_deref(), Some("claude"));
    assert_eq!(unchanged.effort_override.as_deref(), Some("high"));
}

#[test]
fn card_create_rejects_pi_permission_mode() {
    let d = test_daemon(Config::default());
    let err = handle_request(
        &d,
        "card.create",
        json!({ "title": "bad", "harness": "pi", "permission_mode": "acceptEdits" }),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    assert!(err.to_string().contains("permission mode"));
}

#[test]
fn switching_card_to_pi_rejects_incompatible_permission() {
    let d = test_daemon(Config::default());
    let created = handle_request(
        &d,
        "card.create",
        json!({
            "title": "switch",
            "harness": "claude",
            "permission_mode": "acceptEdits"
        }),
    )
    .unwrap();
    let err = handle_request(
        &d,
        "card.update",
        json!({ "id": created["id"], "harness": "pi" }),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    let unchanged = d
        .store
        .lock()
        .get_card(created["id"].as_i64().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.harness, "claude");
    assert_eq!(unchanged.permission_mode.as_deref(), Some("acceptEdits"));
}

#[test]
fn switching_card_from_pi_to_claude_rejects_incompatible_effort() {
    let d = test_daemon(Config::default());
    let created = handle_request(
        &d,
        "card.create",
        json!({ "title": "switch", "harness": "pi", "effort": "off" }),
    )
    .unwrap();
    let err = handle_request(
        &d,
        "card.update",
        json!({ "id": created["id"], "harness": "claude" }),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    let unchanged = d
        .store
        .lock()
        .get_card(created["id"].as_i64().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.harness, "pi");
    assert_eq!(unchanged.effort, Some(Effort::Off));
}

#[test]
fn duplicate_column_name_is_a_bad_request_over_the_rpc() {
    let d = test_daemon(Config::default());
    let mut events = d.events_tx.subscribe();

    let err = handle_request(&d, "column.create", json!({"name": "Todo"})).unwrap_err();

    // Code 1, not 5: retrying this exact request can never succeed, so a
    // scripting agent must be told to change it rather than to retry.
    assert_eq!(err.code(), 1);
    let message = err.to_string();
    assert!(
        message.contains(r#"column "Todo" already exists on this board"#),
        "{message}"
    );
    assert!(!message.contains("sqlite"), "{message}");
    assert!(!message.contains("columns.board_id"), "{message}");
    testkit::assert_no_events(&mut events);
    assert_eq!(d.store.lock().list_columns(BOARD_ID).unwrap().len(), 1);
}

#[test]
fn duplicate_board_rename_is_a_bad_request_over_the_rpc() {
    let d = test_daemon(Config::default());
    let scoped = handle_request(&d, "board.open", json!({"scope_path": "/repo"})).unwrap();
    let board_id = scoped["board"]["id"].as_i64().unwrap();
    let mut events = d.events_tx.subscribe();

    // Every project's first board is `main`; a sibling board shares the
    // project, so renaming onto the sibling's name is a same-project
    // case-insensitive duplicate.
    let sibling = handle_request(
        &d,
        "board.create",
        json!({"project_id": scoped["board"]["project_id"], "name": "Backlog"}),
    )
    .unwrap();
    assert_eq!(sibling["board"]["name"], "Backlog");

    let err = handle_request(
        &d,
        "board.rename",
        json!({"board_id": board_id, "name": "BACKLOG"}),
    )
    .unwrap_err();

    assert_eq!(err.code(), 1);
    let message = err.to_string();
    assert!(
        message.contains(r#"board "BACKLOG" already exists in this project"#),
        "{message}"
    );
    assert!(!message.contains("sqlite"), "{message}");
    testkit::assert_no_events(&mut events);
    assert_eq!(d.store.lock().get_board(board_id).unwrap().name, "main");
}

// Antigravity (A7 validation): the daemon validates antigravity cards against
// a FRESH live catalog probe on the edit/enqueue paths. Catalog up → a stored
// model the CLI no longer lists fails the create/edit with an actionable
// error (fail-closed). Catalog down (no bin / failing bin) → free-form:
// stored models keep running, only new selection is constrained.
//
// The catalog-up probes shell out to a fixture `agy` for real, mirroring the
// discovery overlay tests.

fn agy_catalog_fixture(dir: &tempfile::TempDir, stdout: &str) -> std::path::PathBuf {
    let script = format!("#!/bin/sh\ncat <<'HBEOF'\n{stdout}\nHBEOF\n");
    let bin = dir.path().join("agy-fixture");
    std::fs::write(&bin, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    bin
}

const AGY_CATALOG_UP_FIXTURE: &str = r#"{
  "conversation_id": "",
  "status": "SUCCESS",
  "response": "",
  "command": {
    "name": "models",
    "data": {
      "models": [
        {"id": "gemini-3.7-flash-high", "label": "Gemini 3.7 Flash (High)"},
        {"id": "gemini-3.7-flash-medium", "label": "Gemini 3.7 Flash (Medium)"},
        {"id": "gemini-3.7-flash-low", "label": "Gemini 3.7 Flash (Low)"}
      ]
    }
  }
}
"#;

#[test]
fn antigravity_card_create_rejects_removed_model_when_catalog_up() {
    // The live catalog no longer lists the stored model: card.create must
    // fail closed with an actionable InvalidModel error (code 1), not silently
    // downgrade or accept.
    let dir = tempfile::tempdir().unwrap();
    let bin = agy_catalog_fixture(&dir, AGY_CATALOG_UP_FIXTURE);
    let config = Config {
        agy_bin: Some(bin.to_str().unwrap().to_string()),
        ..Config::default()
    };
    let d = test_daemon(config);
    let err = handle_request(
        &d,
        "card.create",
        json!({
            "title": "removed model",
            "harness": "antigravity",
            "model": "model-removed-from-catalog",
            "permission_mode": "sandbox"
        }),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1, "{err:?}");
    let message = err.to_string();
    assert!(
        message.contains("model-removed-from-catalog"),
        "the error must name the model: {message}"
    );
}

#[test]
fn antigravity_card_create_accepts_stored_model_when_catalog_down() {
    // Catalog down (no agy_bin in tests): free-form — a stored model that
    // the unreachable catalog cannot prove gone keeps running.
    let d = test_daemon(Config::default());
    let created = handle_request(
        &d,
        "card.create",
        json!({
            "title": "stored model",
            "harness": "antigravity",
            "model": "gemini-3.7-flash",
            "effort": "high",
            "permission_mode": "always-proceed"
        }),
    )
    .unwrap();
    assert_eq!(created["harness"], "antigravity");
    assert_eq!(created["model"], "gemini-3.7-flash");
    assert_eq!(created["permission_mode"], "always-proceed");
}

#[test]
fn antigravity_card_create_rejects_unknown_permission_mode() {
    // Only the three verified modes are board-facing; anything else is
    // rejected even when the catalog is down.
    let d = test_daemon(Config::default());
    let err = handle_request(
        &d,
        "card.create",
        json!({
            "title": "bad permission",
            "harness": "antigravity",
            "permission_mode": "full-access"
        }),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1, "{err:?}");
    let message = err.to_string();
    assert!(
        message.contains("full-access"),
        "the error must name the mode: {message}"
    );
}
