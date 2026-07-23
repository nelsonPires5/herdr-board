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
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
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
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
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
