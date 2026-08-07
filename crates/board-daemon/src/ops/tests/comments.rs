use super::*;
use board_core::db::EnqueueRun;

fn event_for(rx: &mut broadcast::Receiver<Event>) -> Event {
    rx.try_recv().expect("expected board change event")
}

#[test]
fn board_rename_emits_a_scoped_board_change() {
    let d = test_daemon(Config::default());
    let board = handle_request(&d, "board.open", json!({"scope_path":"/rename"})).unwrap();
    let board_id = board["board"]["id"].as_i64().unwrap();
    let mut events = d.events_tx.subscribe();

    let renamed = handle_request(
        &d,
        "board.rename",
        json!({"board_id":board_id,"name":"Renamed"}),
    )
    .unwrap();
    assert_eq!(renamed["id"], board_id);
    assert_eq!(renamed["name"], "Renamed");
    assert_eq!(
        event_for(&mut events),
        Event::BoardChanged {
            reason: BoardChangedReason::ColumnChanged,
            board_id: Some(board_id),
            card_id: None,
            column_id: None,
        }
    );
}

#[test]
fn card_list_visibility_is_forwarded_for_board_and_column_queries() {
    let d = test_daemon(Config::default());
    let active = handle_request(&d, "card.create", json!({"title":"active"})).unwrap();
    let archived = handle_request(&d, "card.create", json!({"title":"archived"})).unwrap();
    let archived_id = archived["id"].as_i64().unwrap();
    let column_id = active["column_id"].as_i64().unwrap();
    handle_request(
        &d,
        "card.archive",
        json!({"id":archived_id,"archived":true}),
    )
    .unwrap();

    let list = |params| handle_request(&d, "card.list", params).unwrap();
    assert_eq!(list(json!({})).as_array().unwrap().len(), 1);
    assert_eq!(
        list(json!({"visibility":"all"})).as_array().unwrap().len(),
        2
    );
    let archived_cards = list(json!({
        "column_id": column_id,
        "visibility": "archived"
    }));
    assert_eq!(archived_cards.as_array().unwrap().len(), 1);
    assert_eq!(archived_cards[0]["id"], archived_id);
}

#[test]
fn fake_harness_agent_ids_remain_compatible_without_durable_runs() {
    let d = test_daemon(Config::default());
    let card = d
        .store
        .lock()
        .create_card(&CardCreateParams {
            title: "fake comments".into(),
            harness: Some("fake".into()),
            ..Default::default()
        })
        .unwrap();
    let added = handle_request(
        &d,
        "comment.add",
        json!({
            "card_id": card.id,
            "body": "fake output",
            "actor_run_id": 12345
        }),
    )
    .unwrap();
    assert_eq!(added["author"], "agent:12345");
    let edited = handle_request(
        &d,
        "comment.update",
        json!({
            "id": added["id"],
            "body": "revised fake output",
            "actor_run_id": 12345
        }),
    )
    .unwrap();
    assert_eq!(edited["body"], "revised fake output");
}

#[test]
fn fake_harness_missing_actor_run_is_rejected_when_card_has_open_run() {
    let d = test_daemon(Config::default());
    let card = d
        .store
        .lock()
        .create_card(&CardCreateParams {
            title: "fake comments with open run".into(),
            harness: Some("fake".into()),
            ..Default::default()
        })
        .unwrap();
    {
        let db = d.store.lock();
        db.enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "fake",
            argv_json: "[]",
            prompt_snapshot: "prompt",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    }

    let error = handle_request(
        &d,
        "comment.add",
        json!({
            "card_id": card.id,
            "body": "must not use fixture fallback",
            "actor_run_id": 12345
        }),
    );
    assert!(error.is_err());
    assert!(d.store.lock().list_comments(card.id).unwrap().is_empty());
}

#[test]
fn reused_pane_comment_maps_stale_run_identity_only_when_pane_matches() {
    let d = test_daemon(Config::default());
    let (card_id, stale_run_id, current_run_id) = add_reused_pane_runs(&d);

    let added = handle_request(
        &d,
        "comment.add",
        json!({
            "card_id": card_id,
            "body": "current stage result",
            "author": format!("agent:{stale_run_id}"),
            "actor_run_id": stale_run_id,
            "actor_pane_id": "w1:p-shared"
        }),
    )
    .unwrap();
    assert_eq!(added["author"], format!("agent:{current_run_id}"));

    let other = test_daemon(Config::default());
    let (other_card_id, other_stale_run_id, _) = add_reused_pane_runs(&other);
    let denied = handle_request(
        &other,
        "comment.add",
        json!({
            "card_id": other_card_id,
            "body": "must not cross panes",
            "author": format!("agent:{other_stale_run_id}"),
            "actor_run_id": other_stale_run_id,
            "actor_pane_id": "w1:p-different"
        }),
    )
    .unwrap_err();
    assert!(denied.to_string().contains("no longer open"), "{denied}");
    assert!(other
        .store
        .lock()
        .list_comments(other_card_id)
        .unwrap()
        .is_empty());
}

#[test]
fn comment_routes_authorize_agents_and_hide_soft_deleted_comments() {
    let d = test_daemon(Config::default());
    let card = handle_request(&d, "card.create", json!({"title":"comments"})).unwrap();
    let card_id = card["id"].as_i64().unwrap();
    let board_id = card["board_id"].as_i64().unwrap();
    let column_id = card["column_id"].as_i64().unwrap();

    // An author-only add remains valid for configured/fake agents that use the
    // original comment.add payload and do not send actor_run_id.
    let legacy = handle_request(
        &d,
        "comment.add",
        json!({"card_id":card_id,"author":"agent:9001","body":"legacy"}),
    )
    .unwrap();
    let legacy_id = legacy["id"].as_i64().unwrap();

    let run_id = {
        let db = d.store.lock();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id,
                column_id,
                harness: "fake",
                argv_json: "[]",
                prompt_snapshot: "prompt",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, Some("workspace"), Some("pane"), None)
            .unwrap();
        run.id
    };
    let mut events = d.events_tx.subscribe();

    let owned = handle_request(
        &d,
        "comment.add",
        json!({"card_id":card_id,"body":"owned","actor_run_id":run_id}),
    )
    .unwrap();
    let owned_id = owned["id"].as_i64().unwrap();
    assert_eq!(owned["author"], format!("agent:{run_id}"));
    assert_eq!(
        event_for(&mut events),
        Event::BoardChanged {
            reason: BoardChangedReason::CommentAdded,
            board_id: Some(board_id),
            card_id: Some(card_id),
            column_id: None,
        }
    );

    let edited = handle_request(
        &d,
        "comment.update",
        json!({"id":owned_id,"body":"edited","actor_run_id":run_id}),
    )
    .unwrap();
    assert_eq!(edited["body"], "edited");
    assert!(matches!(
        event_for(&mut events),
        Event::BoardChanged {
            card_id: Some(id),
            ..
        } if id == card_id
    ));

    let other_run_id = {
        let db = d.store.lock();
        let other_card = db
            .create_card(&CardCreateParams {
                title: "other card".into(),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: other_card.id,
                column_id: other_card.column_id,
                harness: "fake",
                argv_json: "[]",
                prompt_snapshot: "prompt",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, Some("workspace"), Some("pane-2"), None)
            .unwrap();
        run.id
    };
    let denied = handle_request(
        &d,
        "comment.delete",
        json!({"id":owned_id,"actor_run_id":other_run_id}),
    )
    .unwrap_err();
    assert_eq!(denied.code(), 3);
    crate::testkit::assert_no_events(&mut events);

    // Human callers may edit an agent comment, and deletion is soft.
    handle_request(
        &d,
        "comment.update",
        json!({"id":legacy_id,"body":"human edit"}),
    )
    .unwrap();
    handle_request(&d, "comment.delete", json!({"id":owned_id})).unwrap();
    assert!(
        handle_request(&d, "comment.get", json!({"id":owned_id})).unwrap()["deleted_at"]
            .is_string()
    );
    assert!(
        handle_request(&d, "comment.history", json!({"id":owned_id}))
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["body"] == "edited")
    );
    let detail = handle_request(&d, "card.get", json!({"id":card_id})).unwrap();
    assert!(!detail["comments"]
        .as_array()
        .unwrap()
        .iter()
        .any(|comment| comment["id"] == owned_id));

    let system = handle_request(
        &d,
        "comment.add",
        json!({"card_id":card_id,"author":"system","body":"immutable"}),
    )
    .unwrap();
    assert!(handle_request(
        &d,
        "comment.update",
        json!({"id":system["id"],"body":"changed"}),
    )
    .is_err());
    assert!(handle_request(&d, "comment.delete", json!({"id":system["id"]}),).is_err());
}
