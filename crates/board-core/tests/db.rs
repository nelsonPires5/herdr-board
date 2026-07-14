//! Db migrations, seed, CRUD, and position management.

use board_core::db::{Db, BOARD_ID};
use board_core::protocol::{CardCreateParams, ColumnCreateParams, RunOutcome, SpaceKind, Trigger};

fn mem() -> Db {
    Db::open_in_memory().unwrap()
}

#[test]
fn migration_seeds_board_and_todo_column() {
    let db = mem();
    assert_eq!(db.user_version().unwrap(), 1);
    let board = db.get_board(BOARD_ID).unwrap();
    assert_eq!(board.name, "main");
    let cols = db.list_columns(BOARD_ID).unwrap();
    assert_eq!(cols.len(), 1);
    assert_eq!(cols[0].name, "Todo");
    assert_eq!(cols[0].position, 0);
    assert_eq!(cols[0].trigger, Trigger::Manual);
}

#[test]
fn migration_idempotent_on_reopen() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    {
        let db = Db::open(&path).unwrap();
        assert_eq!(db.list_columns(BOARD_ID).unwrap().len(), 1);
    }
    // Reopen: must not re-seed (still exactly one board, one column).
    {
        let db = Db::open(&path).unwrap();
        assert_eq!(db.user_version().unwrap(), 1);
        assert_eq!(db.list_columns(BOARD_ID).unwrap().len(), 1);
        assert_eq!(db.get_board(BOARD_ID).unwrap().name, "main");
    }
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
fn card_defaults_go_into_todo() {
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "X".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(card.column_id, db.default_column_id(BOARD_ID).unwrap());
    assert_eq!(card.harness, "claude");
    assert_eq!(card.space_kind, SpaceKind::Workspace);
}

#[test]
fn comments_and_runs_roundtrip() {
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "X".into(),
            ..Default::default()
        })
        .unwrap();
    db.add_comment(card.id, "user", "hello").unwrap();
    db.add_comment(card.id, "agent:1", "did it").unwrap();
    assert_eq!(db.list_comments(card.id).unwrap().len(), 2);

    let run = db
        .create_run(
            card.id,
            card.column_id,
            "claude",
            "[]",
            "prompt",
            Some("sess"),
        )
        .unwrap();
    assert!(run.started_at.is_none());
    assert_eq!(db.count_queued_runs().unwrap(), 1);

    db.start_run(run.id, Some("w4"), Some("p9")).unwrap();
    assert_eq!(db.count_active_runs().unwrap(), 1);
    let active = db.active_run_for_card(card.id).unwrap().unwrap();
    assert_eq!(active.herdr_pane_id.as_deref(), Some("p9"));

    db.finish_run(run.id, RunOutcome::Ok, Some("done")).unwrap();
    assert_eq!(db.count_active_runs().unwrap(), 0);
    let done = db.get_run(run.id).unwrap();
    assert_eq!(done.outcome, Some(RunOutcome::Ok));
    assert!(done.ended_at.is_some());
}

#[test]
fn queued_runs_by_space_key_fifo() {
    let db = mem();
    let c1 = db
        .create_card(&CardCreateParams {
            title: "1".into(),
            space_kind: Some(SpaceKind::Workspace),
            space_ref: Some("w4".into()),
            ..Default::default()
        })
        .unwrap();
    let c2 = db
        .create_card(&CardCreateParams {
            title: "2".into(),
            space_kind: Some(SpaceKind::Workspace),
            space_ref: Some("w4".into()),
            ..Default::default()
        })
        .unwrap();
    let other = db
        .create_card(&CardCreateParams {
            title: "3".into(),
            space_kind: Some(SpaceKind::Workspace),
            space_ref: Some("w9".into()),
            ..Default::default()
        })
        .unwrap();
    db.create_run(c1.id, c1.column_id, "claude", "[]", "p", None)
        .unwrap();
    db.create_run(c2.id, c2.column_id, "claude", "[]", "p", None)
        .unwrap();
    db.create_run(other.id, other.column_id, "claude", "[]", "p", None)
        .unwrap();

    let w4 = db
        .queued_runs_by_space(SpaceKind::Workspace, Some("w4"))
        .unwrap();
    assert_eq!(w4.len(), 2);
    assert!(w4[0].id < w4[1].id); // FIFO by run id
    let w9 = db
        .queued_runs_by_space(SpaceKind::Workspace, Some("w9"))
        .unwrap();
    assert_eq!(w9.len(), 1);
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
