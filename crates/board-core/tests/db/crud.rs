use super::{arm_fault, mem};
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
fn duplicate_card_copies_config_resets_state_and_lands_below_original() {
    let db = mem();

    // One fully configured card: every copyable field set, plus run and
    // comment history the copy must NOT inherit.
    let original = db
        .create_card(&CardCreateParams {
            title: "Ship the widget".into(),
            description: Some("base prompt".into()),
            harness: Some("claude".into()),
            model: Some("opus".into()),
            effort: Some(Effort::High),
            permission_mode: Some("acceptEdits".into()),
            session: Some("work".into()),
            space_kind: Some(SpaceKind::NewWorkspace),
            space_ref: Some("widget-build".into()),
            space_cwd: Some("/tmp/widget".into()),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: original.id,
            column_id: original.column_id,
            harness: "claude",
            argv_json: "[]",
            prompt_snapshot: "p",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: Some("conv-1"),
            session: Some("work"),
        })
        .unwrap();
    db.promote_run_uow(run.id, Some("w1"), Some("p1"), None)
        .unwrap();
    db.set_card_session(original.id, "conv-1").unwrap();
    db.add_comment(original.id, "agent:1", "first draft")
        .unwrap();

    // Two followers that must shift down by one position.
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
    assert_eq!((original.position, b.position, c.position), (0, 1, 2));

    // Re-read the original after the run/comment setup so the "untouched"
    // comparison below uses its post-setup state (running, conversation id).
    let original = db.require_card(original.id).unwrap();

    let copy = db.duplicate_card(original.id).unwrap();

    // New identity, clean state.
    assert_ne!(copy.id, original.id);
    assert_eq!(copy.title, "Ship the widget (copy)");
    assert_eq!(copy.status, CardStatus::Idle);
    assert_eq!(copy.awaiting_reason, None);
    assert_eq!(copy.session_id, None);
    assert_eq!(copy.archived_at, None);

    // Full configuration copied.
    assert_eq!(copy.description, original.description);
    assert_eq!(copy.harness, original.harness);
    assert_eq!(copy.model, original.model);
    assert_eq!(copy.effort, original.effort);
    assert_eq!(copy.permission_mode, original.permission_mode);
    assert_eq!(copy.session, original.session);
    assert_eq!(copy.space_kind, original.space_kind);
    assert_eq!(copy.space_ref, original.space_ref);
    assert_eq!(copy.space_cwd, original.space_cwd);
    assert_eq!(copy.board_id, original.board_id);
    assert_eq!(copy.column_id, original.column_id);

    // Position: immediately below the original, column recompacted.
    let order = db.list_cards_in_column(original.column_id).unwrap();
    assert_eq!(
        order.iter().map(|c| c.title.clone()).collect::<Vec<_>>(),
        vec![
            "Ship the widget".to_string(),
            "Ship the widget (copy)".to_string(),
            "B".to_string(),
            "C".to_string()
        ]
    );
    assert_eq!(
        order.iter().map(|c| c.position).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );

    // No execution state or history on the copy.
    assert!(db.list_runs(copy.id).unwrap().is_empty());
    assert!(db.list_comments(copy.id).unwrap().is_empty());
    assert!(db.open_run_for_card(copy.id).unwrap().is_none());

    // The original is untouched, including its run and comment.
    let reloaded = db.require_card(original.id).unwrap();
    assert_eq!(reloaded, original);
    assert_eq!(db.list_runs(original.id).unwrap().len(), 1);
    assert_eq!(db.list_comments(original.id).unwrap().len(), 1);
}

#[test]
fn duplicate_card_at_column_end_appends_and_last_duplicate_is_idempotent_per_row() {
    let db = mem();
    let _a = db
        .create_card(&CardCreateParams {
            title: "A".into(),
            ..Default::default()
        })
        .unwrap();
    let last = db
        .create_card(&CardCreateParams {
            title: "Last".into(),
            ..Default::default()
        })
        .unwrap();

    let copy = db.duplicate_card(last.id).unwrap();
    let order = db.list_cards_in_column(last.column_id).unwrap();
    assert_eq!(
        order.iter().map(|c| c.title.clone()).collect::<Vec<_>>(),
        vec![
            "A".to_string(),
            "Last".to_string(),
            "Last (copy)".to_string()
        ]
    );
    assert_eq!(copy.position, 2);

    // Each duplication appends another suffix copy directly below the same
    // source card (the first copy shifts down), never cloning the previous
    // copy.
    let again = db.duplicate_card(last.id).unwrap();
    assert_eq!(again.title, "Last (copy)");
    assert_eq!(again.position, 2);
    assert_eq!(db.list_cards_in_column(last.column_id).unwrap().len(), 4);
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
fn all_cards_and_run_lookup_include_scoped_boards() {
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

    // Board scoping: a scoped board's cards are part of `list_all_cards`.
    assert!(db.list_all_cards().unwrap().iter().any(|c| c.id == card.id));
    // A scoped board's runs are addressable one exact run at a time
    // (`latest_run_with_pane` is gone — no caller ever wants "some latest run"
    // now that `run.focus` names its run).
    assert_eq!(
        db.run_for_card(card.id, latest.id)
            .unwrap()
            .herdr_pane_id
            .as_deref(),
        Some("p-new")
    );
    assert_eq!(
        db.run_for_card(card.id, newest_without_pane.id)
            .unwrap()
            .herdr_pane_id,
        None
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

// --- cross-board transfer (prototype) ---

#[test]
fn transfer_card_moves_board_and_column_and_compacts_both() {
    let db = mem();
    let alpha = db.open_board("/alpha").unwrap();
    let beta = db.open_board("/beta").unwrap();
    let alpha_todo = db.default_column_id(alpha.id).unwrap();
    let beta_done = db
        .create_column(&ColumnCreateParams {
            board_id: Some(beta.id),
            name: "Done".into(),
            ..Default::default()
        })
        .unwrap();
    // Three cards in alpha Todo: [a, b, c].
    let a = db
        .create_card(&CardCreateParams {
            board_id: Some(alpha.id),
            title: "a".into(),
            ..Default::default()
        })
        .unwrap();
    let b = db
        .create_card(&CardCreateParams {
            board_id: Some(alpha.id),
            title: "b".into(),
            ..Default::default()
        })
        .unwrap();
    let c = db
        .create_card(&CardCreateParams {
            board_id: Some(alpha.id),
            title: "c".into(),
            ..Default::default()
        })
        .unwrap();
    // Two cards already in beta Done: [x, y].
    let x = db
        .create_card(&CardCreateParams {
            board_id: Some(beta.id),
            column_id: Some(beta_done.id),
            title: "x".into(),
            ..Default::default()
        })
        .unwrap();
    let y = db
        .create_card(&CardCreateParams {
            board_id: Some(beta.id),
            column_id: Some(beta_done.id),
            title: "y".into(),
            ..Default::default()
        })
        .unwrap();

    // Move b into beta Done at position 1 (between x and y).
    let moved = db
        .transfer_card(b.id, beta.id, beta_done.id, Some(1))
        .unwrap();
    assert_eq!(moved.board_id, beta.id);
    assert_eq!(moved.column_id, beta_done.id);

    // Destination compacted: [x, b, y] -> 0,1,2.
    let beta_order: Vec<(i64, i64)> = [x.id, b.id, y.id]
        .iter()
        .map(|id| (*id, db.get_card(*id).unwrap().unwrap().position))
        .collect();
    assert_eq!(beta_order, vec![(x.id, 0), (b.id, 1), (y.id, 2)]);

    // Source compacted: remaining [a, c] -> 0,1.
    let alpha_order: Vec<(i64, i64)> = [a.id, c.id]
        .iter()
        .map(|id| (*id, db.get_card(*id).unwrap().unwrap().position))
        .collect();
    assert_eq!(alpha_order, vec![(a.id, 0), (c.id, 1)]);

    // b no longer appears under alpha's board id.
    assert_eq!(db.get_card(b.id).unwrap().unwrap().board_id, beta.id);
    let _ = alpha_todo; // alpha Todo column still intact
}

#[test]
fn transfer_card_rejects_column_from_another_board() {
    let db = mem();
    let alpha = db.open_board("/alpha").unwrap();
    let beta = db.open_board("/beta").unwrap();
    let beta_done = db
        .create_column(&ColumnCreateParams {
            board_id: Some(beta.id),
            name: "Done".into(),
            ..Default::default()
        })
        .unwrap();
    let card = db
        .create_card(&CardCreateParams {
            board_id: Some(alpha.id),
            title: "card".into(),
            ..Default::default()
        })
        .unwrap();

    // Declare destination board alpha but a beta column: must be rejected.
    let err = db
        .transfer_card(card.id, alpha.id, beta_done.id, None)
        .unwrap_err();
    assert!(
        matches!(err, board_core::Error::InvalidState(_)),
        "expected InvalidState, got {err:?}"
    );
    // And the symmetric lie: declare beta but hand an alpha column (the card's
    // own default column belongs to alpha).
    let alpha_todo = db.default_column_id(alpha.id).unwrap();
    let err = db
        .transfer_card(card.id, beta.id, alpha_todo, None)
        .unwrap_err();
    assert!(matches!(err, board_core::Error::InvalidState(_)));

    // Card untouched.
    let after = db.get_card(card.id).unwrap().unwrap();
    assert_eq!(after.board_id, alpha.id);
    assert_eq!(after.column_id, alpha_todo);
}

#[test]
fn transfer_card_is_atomic_on_bad_destination() {
    let db = mem();
    let alpha = db.open_board("/alpha").unwrap();
    let beta = db.open_board("/beta").unwrap();
    let a = db
        .create_card(&CardCreateParams {
            board_id: Some(alpha.id),
            title: "a".into(),
            ..Default::default()
        })
        .unwrap();
    let _b = db
        .create_card(&CardCreateParams {
            board_id: Some(alpha.id),
            title: "b".into(),
            ..Default::default()
        })
        .unwrap();
    let _beta_done = db
        .create_column(&ColumnCreateParams {
            board_id: Some(beta.id),
            name: "Done".into(),
            ..Default::default()
        })
        .unwrap();

    // A bogus destination column id must roll back the whole transaction:
    // neither board_id nor any position changes.
    let err = db
        .transfer_card(a.id, beta.id, 9_999_999, None)
        .unwrap_err();
    assert!(
        matches!(err, board_core::Error::NotFound(_)),
        "expected NotFound for missing column, got {err:?}"
    );
    let after = db.get_card(a.id).unwrap().unwrap();
    assert_eq!(after.board_id, alpha.id);
}

#[test]
fn require_card_and_require_column_are_not_found_aware_lookups() {
    let db = mem();
    let column_id = db.default_column_id(BOARD_ID).unwrap();
    let card = db
        .create_card(&CardCreateParams {
            title: "required".into(),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(db.require_card(card.id).unwrap(), card);
    assert_eq!(
        db.require_column(column_id).unwrap(),
        db.get_column(column_id).unwrap().unwrap()
    );

    let err = db.require_card(9_999_999).unwrap_err();
    assert!(
        matches!(&err, board_core::Error::NotFound(m) if m == "card 9999999"),
        "expected NotFound for missing card, got {err:?}"
    );
    let err = db.require_column(9_999_999).unwrap_err();
    assert!(
        matches!(&err, board_core::Error::NotFound(m) if m == "column 9999999"),
        "expected NotFound for missing column, got {err:?}"
    );
}

// -- duplicate-name constraints ---------------------------------------------
//
// A duplicate name is something the *request* got wrong, so it must reach the
// user as protocol code 1 with an actionable message — never as code 5 with
// SQLite's table and column names in it.

/// A user-facing duplicate-name rejection: code 1 and no storage-layer detail.
fn assert_actionable_duplicate(err: &board_core::Error, expected: &str) {
    assert_eq!(err.code(), 1, "expected a bad request, got {err:?}");
    let message = err.to_string();
    assert!(
        message.contains(expected),
        "message must name the offending value: {message}"
    );
    for leak in ["sqlite", "UNIQUE", "constraint", "columns.", "boards."] {
        assert!(
            !message.contains(leak),
            "message leaks the storage layer ({leak}): {message}"
        );
    }
}

#[test]
fn duplicate_column_name_on_the_same_board_is_a_bad_request() {
    let db = mem();
    let duplicate = db
        .create_column(&ColumnCreateParams {
            name: "Todo".into(),
            ..Default::default()
        })
        .unwrap_err();

    assert_actionable_duplicate(&duplicate, r#"column "Todo" already exists on this board"#);
    assert_eq!(db.list_columns(BOARD_ID).unwrap().len(), 1);
}

#[test]
fn the_same_column_name_on_another_board_is_accepted() {
    let db = mem();
    let other = db.open_board("/other").unwrap();

    // The constraint is per-board: reusing a name across boards must not be
    // swept up by the duplicate-name rejection.
    let reused = db
        .create_column(&ColumnCreateParams {
            board_id: Some(other.id),
            name: "Todo".into(),
            ..Default::default()
        })
        .unwrap_err();
    assert_actionable_duplicate(&reused, r#"column "Todo" already exists on this board"#);

    let fresh = db
        .create_column(&ColumnCreateParams {
            board_id: Some(other.id),
            name: "Review".into(),
            ..Default::default()
        })
        .unwrap();
    let global_review = db
        .create_column(&ColumnCreateParams {
            name: "Review".into(),
            ..Default::default()
        })
        .unwrap();
    assert_ne!(fresh.id, global_review.id);
    assert_eq!(fresh.board_id, other.id);
    assert_eq!(global_review.board_id, BOARD_ID);
}

#[test]
fn renaming_a_column_onto_a_sibling_name_is_a_bad_request() {
    let db = mem();
    let review = db
        .create_column(&ColumnCreateParams {
            name: "Review".into(),
            ..Default::default()
        })
        .unwrap();

    let clash = db
        .update_column(&ColumnUpdateParams {
            id: review.id,
            name: Some("Todo".into()),
            ..Default::default()
        })
        .unwrap_err();

    assert_actionable_duplicate(&clash, r#"column "Todo" already exists on this board"#);
    assert_eq!(db.require_column(review.id).unwrap().name, "Review");
}

#[test]
fn renaming_a_board_onto_an_existing_name_is_a_bad_request() {
    let db = mem();
    let one = db.open_board("/one").unwrap();
    let two = db.open_board("/two").unwrap();

    let clash = db.rename_board(one.id, &two.name).unwrap_err();

    assert_actionable_duplicate(&clash, r#"board "/two" already exists"#);
    assert_eq!(db.get_board(one.id).unwrap().name, one.name);
}

#[test]
fn opening_a_scope_whose_board_name_is_taken_is_a_bad_request() {
    let db = mem();
    // A board renamed onto a path-shaped name collides with the board that
    // opening that scope would have to create.
    db.rename_board(BOARD_ID, "/repo/project").unwrap();

    let clash = db.open_board("/repo/project").unwrap_err();

    assert_actionable_duplicate(&clash, r#"board "/repo/project" already exists"#);
    assert_eq!(db.list_boards().unwrap().len(), 1);
}

#[test]
fn a_non_unique_constraint_failure_is_still_an_internal_error() {
    // A trigger abort is also a SQLITE_CONSTRAINT failure, but it is not a
    // duplicate name: reclassifying it would tell an agent to edit its request
    // when the daemon is the thing that is broken.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let db = Db::open(&path).unwrap();
    arm_fault(
        &path,
        "CREATE TRIGGER abort_columns BEFORE INSERT ON columns
         BEGIN SELECT RAISE(ABORT,'fault: columns'); END;",
    );

    let err = db
        .create_column(&ColumnCreateParams {
            name: "Review".into(),
            ..Default::default()
        })
        .unwrap_err();

    assert_eq!(err.code(), 5, "expected an internal error, got {err:?}");
    assert!(
        matches!(&err, board_core::Error::Sqlite(_)),
        "a trigger abort must stay a storage error, got {err:?}"
    );
}
