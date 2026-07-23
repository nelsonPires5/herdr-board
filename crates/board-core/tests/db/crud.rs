use super::mem;
use board_core::db::{Db, EnqueueRun, FinalizeRun, BOARD_ID};
use board_core::protocol::{
    AwaitingReason, CardCreateParams, CardStatus, ColumnCreateParams, ColumnUpdateParams, Effort,
    Patch, RunOutcome, SpaceKind, Trigger,
};
use rusqlite::Connection;

#[test]
fn nullable_updates_set_then_clear_and_survive_reopen() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let (column_id, card_id) = {
        let db = Db::open(&path).unwrap();
        let column = db
            .create_column(&ColumnCreateParams {
                name: "Configured".into(),
                system_prompt: Some("instructions".into()),
                on_success_column_id: Some(db.default_column_id(BOARD_ID).unwrap()),
                on_fail_column_id: Some(db.default_column_id(BOARD_ID).unwrap()),
                harness_override: Some("pi".into()),
                model_override: Some("model".into()),
                effort_override: Some("high".into()),
                permission_override: Some("manual".into()),
                timeout_minutes: Some(15),
                ..Default::default()
            })
            .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "Patch me".into(),
                model: Some("model".into()),
                effort: Some(Effort::High),
                permission_mode: Some("manual".into()),
                session: Some("session".into()),
                space_ref: Some("workspace".into()),
                space_cwd: Some("/repo".into()),
                ..Default::default()
            })
            .unwrap();

        db.update_column(&ColumnUpdateParams {
            id: column.id,
            system_prompt: Patch::Set("updated instructions".into()),
            on_success_column_id: Patch::Set(column.id),
            on_fail_column_id: Patch::Set(column.id),
            harness_override: Patch::Set("claude".into()),
            model_override: Patch::Set("updated-model".into()),
            effort_override: Patch::Set("medium".into()),
            permission_override: Patch::Set("auto".into()),
            timeout_minutes: Patch::Set(30),
            ..Default::default()
        })
        .unwrap();
        db.update_card(&board_core::protocol::CardUpdateParams {
            id: card.id,
            model: Patch::Set("updated-model".into()),
            effort: Patch::Set(Effort::Medium),
            permission_mode: Patch::Set("auto".into()),
            session: Patch::Set("updated-session".into()),
            space_ref: Patch::Set("updated-workspace".into()),
            space_cwd: Patch::Set("/updated-repo".into()),
            ..Default::default()
        })
        .unwrap();

        // An omitted nullable member is an explicit Unchanged patch, not a
        // request to clear the value that was just stored.
        let unchanged_column = db
            .update_column(&ColumnUpdateParams {
                id: column.id,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            unchanged_column.system_prompt.as_deref(),
            Some("updated instructions")
        );
        let unchanged_card = db
            .update_card(&board_core::protocol::CardUpdateParams {
                id: card.id,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(unchanged_card.model.as_deref(), Some("updated-model"));

        db.update_column(&ColumnUpdateParams {
            id: column.id,
            system_prompt: Patch::Clear,
            on_success_column_id: Patch::Clear,
            on_fail_column_id: Patch::Clear,
            harness_override: Patch::Clear,
            model_override: Patch::Clear,
            effort_override: Patch::Clear,
            permission_override: Patch::Clear,
            timeout_minutes: Patch::Clear,
            ..Default::default()
        })
        .unwrap();
        db.update_card(&board_core::protocol::CardUpdateParams {
            id: card.id,
            model: Patch::Clear,
            effort: Patch::Clear,
            permission_mode: Patch::Clear,
            session: Patch::Clear,
            space_ref: Patch::Clear,
            space_cwd: Patch::Clear,
            ..Default::default()
        })
        .unwrap();
        (column.id, card.id)
    };

    let db = Db::open(&path).unwrap();
    let column = db.get_column(column_id).unwrap().unwrap();
    assert!(column.system_prompt.is_none());
    assert!(column.on_success_column_id.is_none());
    assert!(column.on_fail_column_id.is_none());
    assert!(column.harness_override.is_none());
    assert!(column.model_override.is_none());
    assert!(column.effort_override.is_none());
    assert!(column.permission_override.is_none());
    assert!(column.timeout_minutes.is_none());
    let card = db.get_card(card_id).unwrap().unwrap();
    assert!(card.model.is_none());
    assert!(card.effort.is_none());
    assert!(card.permission_mode.is_none());
    assert!(card.session.is_none());
    assert!(card.space_ref.is_none());
    assert!(card.space_cwd.is_none());
}

#[test]
fn column_create_and_reorder_compaction() {
    let db = mem();
    // Todo is at 0. Add Plan, Execute, Review appended.
    let plan = db
        .create_column(&ColumnCreateParams {
            name: "Plan".into(),
            trigger: Some(Trigger::Auto),
            ..Default::default()
        })
        .unwrap();
    let _exec = db
        .create_column(&ColumnCreateParams {
            name: "Execute".into(),
            ..Default::default()
        })
        .unwrap();
    let review = db
        .create_column(&ColumnCreateParams {
            name: "Review".into(),
            ..Default::default()
        })
        .unwrap();
    let cols = db.list_columns(BOARD_ID).unwrap();
    assert_eq!(
        cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        vec!["Todo", "Plan", "Execute", "Review"]
    );
    // Positions are contiguous 0..n.
    assert_eq!(
        cols.iter().map(|c| c.position).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );

    // Move Review to position 1.
    let after = db.reorder_column(review.id, 1).unwrap();
    assert_eq!(
        after.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        vec!["Todo", "Review", "Plan", "Execute"]
    );
    assert_eq!(
        after.iter().map(|c| c.position).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    let _ = plan;
}

#[test]
fn card_create_move_and_position_compaction() {
    let db = mem();
    let todo = db.default_column_id(BOARD_ID).unwrap();
    let done = db
        .create_column(&ColumnCreateParams {
            name: "Done".into(),
            ..Default::default()
        })
        .unwrap();

    let a = db
        .create_card(&CardCreateParams {
            title: "A".into(),
            ..Default::default()
        })
        .unwrap();
    let b = db
        .create_card(&CardCreateParams {
            title: "B".into(),
            ..Default::default()
        })
        .unwrap();
    let c = db
        .create_card(&CardCreateParams {
            title: "C".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!((a.position, b.position, c.position), (0, 1, 2));

    // Move B out to Done; Todo compacts to [A(0), C(1)].
    db.move_card(b.id, done.id, None).unwrap();
    let todo_cards = db.list_cards_in_column(todo).unwrap();
    assert_eq!(
        todo_cards
            .iter()
            .map(|c| (c.title.clone(), c.position))
            .collect::<Vec<_>>(),
        vec![("A".into(), 0), ("C".into(), 1)]
    );

    // Insert into Done at position 0 by moving C there.
    db.move_card(c.id, done.id, Some(0)).unwrap();
    let done_cards = db.list_cards_in_column(done.id).unwrap();
    assert_eq!(
        done_cards
            .iter()
            .map(|c| (c.title.clone(), c.position))
            .collect::<Vec<_>>(),
        vec![("C".into(), 0), ("B".into(), 1)]
    );
}

#[test]
fn default_card_harness_is_pi() {
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "X".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(card.column_id, db.default_column_id(BOARD_ID).unwrap());
    assert_eq!(card.harness, "pi");
    assert_eq!(card.space_kind, SpaceKind::Workspace);
}

#[test]
fn card_archive_and_restore_roundtrip() {
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "Archive me".into(),
            ..Default::default()
        })
        .unwrap();
    assert!(card.archived_at.is_none());

    let archived = db.set_card_archived(card.id, true).unwrap();
    assert!(archived.archived_at.is_some());
    assert!(db.get_card(card.id).unwrap().unwrap().archived_at.is_some());

    let restored = db.set_card_archived(card.id, false).unwrap();
    assert!(restored.archived_at.is_none());
}

#[test]
fn finalize_run_uow_compacts_source_and_target_column_positions() {
    let db = mem();
    let source = db.default_column_id(BOARD_ID).unwrap();
    let target = db
        .create_column(&ColumnCreateParams {
            name: "Target".into(),
            ..Default::default()
        })
        .unwrap();
    let cards: Vec<_> = ["A", "B", "C"]
        .into_iter()
        .map(|title| {
            db.create_card(&CardCreateParams {
                title: title.into(),
                ..Default::default()
            })
            .unwrap()
        })
        .collect();
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: cards[1].id,
            column_id: source,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "prompt",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();

    db.finalize_run_uow(&FinalizeRun {
        run_id: run.id,
        outcome: RunOutcome::Ok,
        summary: None,
        comments: &[],
        target_column_id: Some(target.id),
        final_status: CardStatus::Idle,
        final_awaiting_reason: None,
        next: None,
    })
    .unwrap();

    let source_cards = db.list_cards_in_column(source).unwrap();
    assert_eq!(
        source_cards
            .iter()
            .map(|card| (card.title.as_str(), card.position))
            .collect::<Vec<_>>(),
        vec![("A", 0), ("C", 1)]
    );
    let target_cards = db.list_cards_in_column(target.id).unwrap();
    assert_eq!(target_cards.len(), 1);
    assert_eq!(target_cards[0].id, cards[1].id);
    assert_eq!(target_cards[0].position, 0);
}

#[test]
fn delete_column_moves_cards() {
    let db = mem();
    let todo = db.default_column_id(BOARD_ID).unwrap();
    let extra = db
        .create_column(&ColumnCreateParams {
            name: "Extra".into(),
            ..Default::default()
        })
        .unwrap();
    let card = db
        .create_card(&CardCreateParams {
            title: "A".into(),
            column_id: Some(extra.id),
            ..Default::default()
        })
        .unwrap();
    db.delete_column(extra.id, Some(todo)).unwrap();
    assert!(db.get_column(extra.id).unwrap().is_none());
    let moved = db.get_card(card.id).unwrap().unwrap();
    assert_eq!(moved.column_id, todo);
}

#[test]
fn board_open_is_idempotent_and_scopes_are_independent() {
    let db = mem();
    let one = db.open_board("/repos/team/project").unwrap();
    let same = db.open_board("/repos/team/project").unwrap();
    let other = db.open_board("/other/project").unwrap();

    assert_eq!(one, same);
    assert_ne!(one.id, other.id);
    assert_eq!(one.name, "/repos/team/project");
    assert_eq!(one.scope_path.as_deref(), Some("/repos/team/project"));
    assert_eq!(db.list_columns(one.id).unwrap().len(), 1);
    assert_eq!(db.list_columns(other.id).unwrap().len(), 1);
    assert_eq!(db.list_columns(one.id).unwrap()[0].name, "Todo");
    assert_eq!(db.list_boards().unwrap()[0].id, BOARD_ID);
}

#[test]
fn scope_path_unique_index_rejects_duplicates() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    {
        let db = Db::open(&path).unwrap();
        db.open_board("/repo").unwrap();
        assert_eq!(db.list_boards().unwrap().len(), 2);
        db.open_board("/repo").unwrap();
        assert_eq!(db.list_boards().unwrap().len(), 2);
    }
    let conn = Connection::open(path).unwrap();
    let duplicate = conn.execute(
        "INSERT INTO boards(name,scope_path) VALUES('/other-name','/repo')",
        [],
    );
    assert!(
        duplicate.is_err(),
        "partial unique index must reject duplicate scope paths"
    );
}

#[test]
fn scoped_crud_rejects_cross_board_references() {
    let db = mem();
    let alpha = db.open_board("/alpha").unwrap();
    let beta = db.open_board("/beta").unwrap();
    let alpha_done = db
        .create_column(&ColumnCreateParams {
            board_id: Some(alpha.id),
            name: "Done".into(),
            ..Default::default()
        })
        .unwrap();
    let beta_todo = db.default_column_id(beta.id).unwrap();
    let card = db
        .create_card(&CardCreateParams {
            board_id: Some(alpha.id),
            title: "alpha card".into(),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(card.board_id, alpha.id);
    assert!(db
        .create_card(&CardCreateParams {
            board_id: Some(alpha.id),
            column_id: Some(beta_todo),
            title: "cross".into(),
            ..Default::default()
        })
        .is_err());
    assert!(db.move_card(card.id, beta_todo, None).is_err());
    assert!(db.delete_column(alpha_done.id, Some(beta_todo)).is_err());
    assert!(db
        .update_column(&ColumnUpdateParams {
            id: alpha_done.id,
            on_success_column_id: Patch::Set(beta_todo),
            ..Default::default()
        })
        .is_err());
    assert_eq!(
        db.get_card(card.id).unwrap().unwrap().column_id,
        db.default_column_id(alpha.id).unwrap()
    );
}

#[test]
fn all_cards_and_latest_run_with_pane_include_scoped_boards() {
    let db = mem();
    let board = db.open_board("/scoped").unwrap();
    let card = db
        .create_card(&CardCreateParams {
            board_id: Some(board.id),
            title: "scoped".into(),
            ..Default::default()
        })
        .unwrap();
    let enqueue = |harness: &str, prompt: &str| {
        db.enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness,
            argv_json: "[]",
            prompt_snapshot: prompt,
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap()
    };
    let finish = |run_id: i64| {
        db.finalize_run_uow(&FinalizeRun {
            run_id,
            outcome: RunOutcome::Ok,
            summary: None,
            comments: &[],
            target_column_id: None,
            final_status: CardStatus::Done,
            final_awaiting_reason: None,
            next: None,
        })
        .unwrap();
    };
    let no_pane = enqueue("pi", "p");
    db.promote_run_uow(no_pane.id, Some("w"), None, None)
        .unwrap();
    finish(no_pane.id);
    let older = enqueue("pi", "p");
    db.promote_run_uow(older.id, Some("w"), Some("p-old"), None)
        .unwrap();
    finish(older.id);
    let latest = enqueue("pi", "p");
    db.promote_run_uow(latest.id, Some("w"), Some("p-new"), None)
        .unwrap();
    finish(latest.id);
    let newest_without_pane = enqueue("pi", "p");
    db.promote_run_uow(newest_without_pane.id, Some("w"), None, None)
        .unwrap();

    assert!(db.list_all_cards().unwrap().iter().any(|c| c.id == card.id));
    assert_eq!(
        db.latest_run_with_pane(card.id)
            .unwrap()
            .unwrap()
            .herdr_pane_id
            .as_deref(),
        Some("p-new")
    );
}

#[test]
fn awaiting_reason_set_and_cleared_with_status() {
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "A".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(card.status, CardStatus::Idle);
    assert!(card.awaiting_reason.is_none());

    // Entering awaiting records the reason.
    let card = db
        .set_card_awaiting(card.id, AwaitingReason::AgentDone)
        .unwrap();
    assert_eq!(card.status, CardStatus::Awaiting);
    assert_eq!(card.awaiting_reason, Some(AwaitingReason::AgentDone));
    // Persisted, not just on the returned struct.
    let fetched = db.get_card(card.id).unwrap().unwrap();
    assert_eq!(fetched.awaiting_reason, Some(AwaitingReason::AgentDone));

    // Re-entering refreshes the reason (explicit done supersedes idle expiry).
    let card = db
        .set_card_awaiting(card.id, AwaitingReason::IdleExpired)
        .unwrap();
    assert_eq!(card.awaiting_reason, Some(AwaitingReason::IdleExpired));

    // Any non-awaiting status clears the reason.
    let card = db.set_card_status(card.id, CardStatus::Running).unwrap();
    assert_eq!(card.status, CardStatus::Running);
    assert!(card.awaiting_reason.is_none());

    // `done` is accepted by the schema.
    let card = db.set_card_status(card.id, CardStatus::Done).unwrap();
    assert_eq!(card.status, CardStatus::Done);
    assert!(card.awaiting_reason.is_none());

    let err = db
        .set_card_status(card.id, CardStatus::Awaiting)
        .unwrap_err();
    assert!(err.to_string().contains("set_card_awaiting"));
}

/// A v5 database (old `status` CHECK without `awaiting`/`done`, no
/// `awaiting_reason` column) must upgrade to v6 via a table rebuild: all rows
/// preserved, the new statuses accepted, and `awaiting_reason` NULL (no
/// backfill of idle cards to `done`).

#[test]
fn current_schema_enforces_awaiting_reason_invariant_for_raw_rows() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let db = Db::open(&path).unwrap();
    let column_id = db.default_column_id(BOARD_ID).unwrap();
    drop(db);

    let conn = Connection::open(path).unwrap();
    conn.execute(
        "INSERT INTO cards (board_id,column_id,position,title,status,awaiting_reason)
         VALUES (1,?1,0,'valid awaiting','awaiting','idle_expired')",
        [column_id],
    )
    .unwrap();
    for (title, status, reason) in [
        ("missing reason", "awaiting", None),
        ("invalid reason", "awaiting", Some("other")),
        ("reason while done", "done", Some("agent_done")),
    ] {
        assert!(conn
            .execute(
                "INSERT INTO cards (board_id,column_id,position,title,status,awaiting_reason)
                 VALUES (1,?1,1,?2,?3,?4)",
                rusqlite::params![column_id, title, status, reason],
            )
            .is_err());
    }
}

#[test]
fn delete_column_rolls_back_card_moves_when_delete_fails() {
    let db = mem();
    let todo = db.default_column_id(BOARD_ID).unwrap();
    let source = db
        .create_column(&ColumnCreateParams {
            name: "Source".into(),
            ..Default::default()
        })
        .unwrap();
    let card = db
        .create_card(&CardCreateParams {
            title: "must stay".into(),
            column_id: Some(source.id),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: source.id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "p",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    db.finalize_run_uow(&FinalizeRun {
        run_id: run.id,
        outcome: RunOutcome::Fail,
        summary: None,
        comments: &[],
        target_column_id: None,
        final_status: CardStatus::Failed,
        final_awaiting_reason: None,
        next: None,
    })
    .unwrap();

    // The historical run still references the source column, so its delete is
    // rejected by the FK after the card move has begun.
    assert!(db.delete_column(source.id, Some(todo)).is_err());
    assert_eq!(
        db.get_card(card.id).unwrap().unwrap().column_id,
        source.id,
        "the preceding move must roll back with the failed delete"
    );
    assert!(db.get_column(source.id).unwrap().is_some());
}
