//! Db migrations, seed, CRUD, and position management.

use board_core::db::{Db, BOARD_ID};
use board_core::protocol::{CardCreateParams, ColumnCreateParams, RunOutcome, SpaceKind, Trigger};
use rusqlite::Connection;

fn mem() -> Db {
    Db::open_in_memory().unwrap()
}

#[test]
fn migration_seeds_board_and_todo_column() {
    let db = mem();
    assert_eq!(db.user_version().unwrap(), 2);
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
        assert_eq!(db.user_version().unwrap(), 2);
        assert_eq!(db.list_columns(BOARD_ID).unwrap().len(), 1);
        assert_eq!(db.get_board(BOARD_ID).unwrap().name, "main");
    }
}

/// A v1 database (legacy `cards` shape with `cwd`/`worktree` kinds and
/// `worktree_base`) must upgrade to v2: kinds converted to `workspace`,
/// `worktree_base` gone, and the new `session`/`space_cwd`/`runs.session`
/// columns present.
#[test]
fn migration_v2_upgrades_v1_database() {
    const V1_SCHEMA: &str = "
    CREATE TABLE boards (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
      created_at TEXT NOT NULL DEFAULT (datetime('now')));
    CREATE TABLE columns (id INTEGER PRIMARY KEY, board_id INTEGER NOT NULL,
      name TEXT NOT NULL, position INTEGER NOT NULL, system_prompt TEXT,
      trigger TEXT NOT NULL DEFAULT 'manual', on_success_column_id INTEGER,
      on_fail_column_id INTEGER, fresh_session INTEGER NOT NULL DEFAULT 0,
      harness_override TEXT, model_override TEXT, effort_override TEXT,
      permission_override TEXT, timeout_minutes INTEGER, UNIQUE (board_id, name));
    CREATE TABLE cards (id INTEGER PRIMARY KEY, board_id INTEGER NOT NULL,
      column_id INTEGER NOT NULL, position INTEGER NOT NULL, title TEXT NOT NULL,
      description TEXT NOT NULL DEFAULT '', harness TEXT NOT NULL DEFAULT 'claude',
      model TEXT, effort TEXT, permission_mode TEXT,
      space_kind TEXT NOT NULL DEFAULT 'workspace'
        CHECK (space_kind IN ('workspace','cwd','worktree')),
      space_ref TEXT, worktree_base TEXT,
      status TEXT NOT NULL DEFAULT 'idle', session_id TEXT,
      created_at TEXT NOT NULL DEFAULT (datetime('now')),
      updated_at TEXT NOT NULL DEFAULT (datetime('now')));
    CREATE TABLE comments (id INTEGER PRIMARY KEY, card_id INTEGER NOT NULL,
      author TEXT NOT NULL, body TEXT NOT NULL,
      created_at TEXT NOT NULL DEFAULT (datetime('now')));
    CREATE TABLE runs (id INTEGER PRIMARY KEY, card_id INTEGER NOT NULL,
      column_id INTEGER NOT NULL, harness TEXT NOT NULL, argv_json TEXT NOT NULL,
      prompt_snapshot TEXT NOT NULL, herdr_workspace_id TEXT, herdr_pane_id TEXT,
      session_id TEXT, started_at TEXT, ended_at TEXT, outcome TEXT,
      result_summary TEXT, log_path TEXT);
    ";
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    {
        // Hand-build a v1 DB with one legacy `worktree` and one `cwd` card.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        conn.execute("INSERT INTO boards (id, name) VALUES (1, 'main')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO columns (board_id, name, position, trigger, fresh_session)
             VALUES (1, 'Todo', 0, 'manual', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (board_id,column_id,position,title,space_kind,space_ref,worktree_base)
             VALUES (1,1,0,'wt','worktree','/repo','main')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (board_id,column_id,position,title,space_kind,space_ref)
             VALUES (1,1,1,'cw','cwd','/some/dir')",
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 1;").unwrap();
    }
    // Open via Db → runs the v2 migration.
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 2);
    let cards = db.list_cards(BOARD_ID).unwrap();
    assert_eq!(cards.len(), 2);
    for c in &cards {
        assert_eq!(c.space_kind, SpaceKind::Workspace, "legacy kind converted");
        assert!(c.session.is_none());
        assert!(c.space_cwd.is_none());
    }
    // space_ref is preserved as-is (best-effort conversion).
    assert!(cards
        .iter()
        .any(|c| c.space_ref.as_deref() == Some("/repo")));
    assert!(cards
        .iter()
        .any(|c| c.space_ref.as_deref() == Some("/some/dir")));
    // runs.session now exists and defaults NULL.
    let card = &cards[0];
    let run = db
        .create_run(card.id, card.column_id, "claude", "[]", "p", None, None)
        .unwrap();
    assert!(run.session.is_none());
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
            None,
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
    db.create_run(c1.id, c1.column_id, "claude", "[]", "p", None, None)
        .unwrap();
    db.create_run(c2.id, c2.column_id, "claude", "[]", "p", None, None)
        .unwrap();
    db.create_run(other.id, other.column_id, "claude", "[]", "p", None, None)
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
