//! Db migrations, seed, CRUD, and position management.

use board_core::db::{Db, BOARD_ID};
use board_core::protocol::{
    AwaitingReason, CardCreateParams, CardStatus, ColumnCreateParams, ColumnUpdateParams, Effort,
    Patch, RunOutcome, SpaceKind, Trigger,
};
use rusqlite::Connection;

fn mem() -> Db {
    Db::open_in_memory().unwrap()
}

#[test]
fn migration_seeds_board_and_todo_column() {
    let db = mem();
    assert_eq!(db.user_version().unwrap(), 8);
    let board = db.get_board(BOARD_ID).unwrap();
    assert_eq!(board.name, "Global");
    assert_eq!(board.scope_path, None);
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
        assert_eq!(db.user_version().unwrap(), 8);
        assert_eq!(db.list_columns(BOARD_ID).unwrap().len(), 1);
        assert_eq!(db.get_board(BOARD_ID).unwrap().name, "Global");
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
    CREATE TABLE columns (id INTEGER PRIMARY KEY,
      board_id INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
      name TEXT NOT NULL, position INTEGER NOT NULL, system_prompt TEXT,
      trigger TEXT NOT NULL DEFAULT 'manual', on_success_column_id INTEGER,
      on_fail_column_id INTEGER, fresh_session INTEGER NOT NULL DEFAULT 0,
      harness_override TEXT, model_override TEXT, effort_override TEXT,
      permission_override TEXT, timeout_minutes INTEGER, UNIQUE (board_id, name));
    CREATE TABLE cards (id INTEGER PRIMARY KEY,
      board_id INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
      column_id INTEGER NOT NULL REFERENCES columns(id),
      position INTEGER NOT NULL, title TEXT NOT NULL,
      description TEXT NOT NULL DEFAULT '', harness TEXT NOT NULL DEFAULT 'claude',
      model TEXT, effort TEXT, permission_mode TEXT,
      space_kind TEXT NOT NULL DEFAULT 'workspace'
        CHECK (space_kind IN ('workspace','cwd','worktree')),
      space_ref TEXT, worktree_base TEXT,
      status TEXT NOT NULL DEFAULT 'idle', session_id TEXT,
      created_at TEXT NOT NULL DEFAULT (datetime('now')),
      updated_at TEXT NOT NULL DEFAULT (datetime('now')));
    CREATE TABLE comments (id INTEGER PRIMARY KEY,
      card_id INTEGER NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
      author TEXT NOT NULL, body TEXT NOT NULL,
      created_at TEXT NOT NULL DEFAULT (datetime('now')));
    CREATE TABLE runs (id INTEGER PRIMARY KEY,
      card_id INTEGER NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
      column_id INTEGER NOT NULL REFERENCES columns(id),
      harness TEXT NOT NULL, argv_json TEXT NOT NULL,
      prompt_snapshot TEXT NOT NULL, herdr_workspace_id TEXT, herdr_pane_id TEXT,
      session_id TEXT, started_at TEXT, ended_at TEXT, outcome TEXT,
      result_summary TEXT, log_path TEXT);
    CREATE INDEX idx_cards_column ON cards(column_id, position);
    CREATE INDEX idx_comments_card ON comments(card_id, created_at);
    CREATE INDEX idx_runs_card ON runs(card_id, started_at);
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
        conn.execute(
            "INSERT INTO comments (card_id,author,body) VALUES (1,'user','preserved')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runs (card_id,column_id,harness,argv_json,prompt_snapshot)
             VALUES (1,1,'claude','[]','preserved prompt')",
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 1;").unwrap();
    }
    // Open via Db → runs the v2 through v7 migrations.
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 8);
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
    // Related rows survive both cards rebuilds, and runs.session defaults NULL.
    let card = cards.iter().find(|c| c.title == "wt").unwrap();
    assert_eq!(db.list_comments(card.id).unwrap()[0].body, "preserved");
    let preserved = db.list_runs(card.id).unwrap()[0].clone();
    assert_eq!(preserved.prompt_snapshot, "preserved prompt");
    db.finish_run(preserved.id, RunOutcome::Ok, None).unwrap();
    let run = db
        .create_run(card.id, card.column_id, "claude", "[]", "p", None, None)
        .unwrap();
    assert!(run.session.is_none());
    let card_id = card.id;
    drop(db);

    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    let index_names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='index'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for expected in ["idx_cards_column", "idx_comments_card", "idx_runs_card"] {
        assert!(index_names.iter().any(|name| name == expected));
    }
    let violations: Vec<String> = conn
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(violations.is_empty());

    conn.execute("DELETE FROM cards WHERE id=?1", [card_id])
        .unwrap();
    let comments: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM comments WHERE card_id=?1",
            [card_id],
            |r| r.get(0),
        )
        .unwrap();
    let runs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM runs WHERE card_id=?1",
            [card_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!((comments, runs), (0, 0));
}

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
fn migration_v4_preserves_claude_cards_and_accepts_pi_efforts() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    {
        let db = Db::open(&path).unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "existing".into(),
                harness: Some("claude".into()),
                ..Default::default()
            })
            .unwrap();
        db.add_comment(card.id, "user", "preserved").unwrap();
        let run = db
            .create_run(
                card.id,
                card.column_id,
                "claude",
                "[]",
                "prompt",
                Some("session"),
                None,
            )
            .unwrap();
        db.start_run(run.id, None, None).unwrap();
        db.finish_run(run.id, RunOutcome::Ok, None).unwrap();
    }
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP INDEX idx_boards_scope_path;
             ALTER TABLE boards DROP COLUMN scope_path;
             UPDATE boards SET name='main' WHERE id=1;
             PRAGMA user_version = 3;",
        )
        .unwrap();
    }

    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 8);
    let existing = db.list_cards(BOARD_ID).unwrap();
    assert_eq!(existing[0].harness, "claude");
    assert_eq!(db.list_comments(existing[0].id).unwrap().len(), 1);
    assert_eq!(db.list_runs(existing[0].id).unwrap().len(), 1);
    let pi = db
        .create_card(&CardCreateParams {
            title: "pi".into(),
            effort: Some(Effort::Minimal),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(pi.harness, "pi");
    assert_eq!(pi.effort, Some(Effort::Minimal));
}

#[test]
fn migration_does_not_downgrade_future_schema_version() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    {
        let db = Db::open(&path).unwrap();
        assert_eq!(db.user_version().unwrap(), 8);
    }
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version = 8;").unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 8);
}

#[test]
fn migration_v3_adds_archived_at_to_v2_database() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    {
        let db = Db::open(&path).unwrap();
        db.create_card(&CardCreateParams {
            title: "pre-v3".into(),
            ..Default::default()
        })
        .unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "ALTER TABLE cards DROP COLUMN archived_at;
             DROP INDEX idx_boards_scope_path;
             ALTER TABLE boards DROP COLUMN scope_path;
             UPDATE boards SET name='main' WHERE id=1;
             PRAGMA user_version = 2;",
        )
        .unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 8);
    let cards = db.list_cards(BOARD_ID).unwrap();
    assert_eq!(cards.len(), 1);
    assert!(cards[0].archived_at.is_none());
}

#[test]
fn run_system_prompt_snapshot_roundtrips_across_file_reopen() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let exact = "old instructions\\n\\nsecond line\\ntrailing spaces  ";
    let (card_id, run_id) = {
        let db = Db::open(&path).unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "snapshot".into(),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .create_run_with_prompt_snapshots(
                card.id,
                card.column_id,
                "pi",
                r#"["pi","--model","x"]"#,
                "Card task:\nwork",
                Some(exact),
                None,
                None,
            )
            .unwrap();
        (card.id, run.id)
    };
    let db = Db::open(&path).unwrap();
    let run = db.get_run(run_id).unwrap();
    assert_eq!(run.card_id, card_id);
    assert_eq!(run.system_prompt_snapshot.as_deref(), Some(exact));
}

#[test]
fn v6_to_v7_migration_preserves_legacy_queued_run_byte_for_byte() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let argv = r#"["pi","--append-system-prompt","legacy\\nexact","Card task:\\nhello"]"#;
    let prompt = "legacy prompt\\nwith exact bytes  ";
    {
        let db = Db::open(&path).unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "legacy".into(),
                harness: Some("pi".into()),
                ..Default::default()
            })
            .unwrap();
        db.create_run(card.id, card.column_id, "pi", argv, prompt, None, None)
            .unwrap();
        // Make this a genuine v6 shape: the migration must add the nullable
        // column rather than relying on a pre-existing empty value.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "ALTER TABLE runs DROP COLUMN system_prompt_snapshot;
             PRAGMA user_version = 6;",
        )
        .unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 8);
    let run = &db.list_runs(1).unwrap()[0];
    assert_eq!(run.argv_json, argv);
    assert_eq!(run.prompt_snapshot, prompt);
    assert_eq!(run.system_prompt_snapshot, None);
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
    let no_pane = db
        .create_run(card.id, card.column_id, "pi", "[]", "p", None, None)
        .unwrap();
    db.start_run(no_pane.id, Some("w"), None).unwrap();
    db.finish_run(no_pane.id, RunOutcome::Ok, None).unwrap();
    let older = db
        .create_run(card.id, card.column_id, "pi", "[]", "p", None, None)
        .unwrap();
    db.start_run(older.id, Some("w"), Some("p-old")).unwrap();
    db.finish_run(older.id, RunOutcome::Ok, None).unwrap();
    let latest = db
        .create_run(card.id, card.column_id, "pi", "[]", "p", None, None)
        .unwrap();
    db.start_run(latest.id, Some("w"), Some("p-new")).unwrap();
    db.finish_run(latest.id, RunOutcome::Ok, None).unwrap();
    let newest_without_pane = db
        .create_run(card.id, card.column_id, "pi", "[]", "p", None, None)
        .unwrap();
    db.start_run(newest_without_pane.id, Some("w"), None)
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
fn migration_v5_preserves_global_data_and_renames_it() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE boards (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
              created_at TEXT NOT NULL DEFAULT (datetime('now')));
            CREATE TABLE columns (id INTEGER PRIMARY KEY, board_id INTEGER NOT NULL,
              name TEXT NOT NULL, position INTEGER NOT NULL, system_prompt TEXT,
              trigger TEXT NOT NULL DEFAULT 'manual', on_success_column_id INTEGER,
              on_fail_column_id INTEGER, fresh_session INTEGER NOT NULL DEFAULT 0,
              harness_override TEXT, model_override TEXT, effort_override TEXT,
              permission_override TEXT, timeout_minutes INTEGER, UNIQUE(board_id,name));
            CREATE TABLE cards (id INTEGER PRIMARY KEY, board_id INTEGER NOT NULL,
              column_id INTEGER NOT NULL, position INTEGER NOT NULL, title TEXT NOT NULL,
              description TEXT NOT NULL DEFAULT '', harness TEXT NOT NULL DEFAULT 'pi',
              model TEXT, effort TEXT, permission_mode TEXT, session TEXT,
              space_kind TEXT NOT NULL DEFAULT 'workspace', space_ref TEXT, space_cwd TEXT,
              status TEXT NOT NULL DEFAULT 'idle', session_id TEXT,
              created_at TEXT NOT NULL DEFAULT (datetime('now')),
              updated_at TEXT NOT NULL DEFAULT (datetime('now')), archived_at TEXT);
            CREATE TABLE comments (id INTEGER PRIMARY KEY, card_id INTEGER NOT NULL,
              author TEXT NOT NULL, body TEXT NOT NULL,
              created_at TEXT NOT NULL DEFAULT (datetime('now')));
            CREATE TABLE runs (id INTEGER PRIMARY KEY, card_id INTEGER NOT NULL,
              column_id INTEGER NOT NULL, harness TEXT NOT NULL, argv_json TEXT NOT NULL,
              prompt_snapshot TEXT NOT NULL, herdr_workspace_id TEXT, herdr_pane_id TEXT,
              session_id TEXT, session TEXT, started_at TEXT, ended_at TEXT, outcome TEXT,
              result_summary TEXT, log_path TEXT);
            INSERT INTO boards(id,name) VALUES(1,'main');
            INSERT INTO columns(id,board_id,name,position) VALUES(1,1,'Todo',0);
            INSERT INTO cards(id,board_id,column_id,position,title) VALUES(1,1,1,0,'kept');
            INSERT INTO comments(card_id,author,body) VALUES(1,'user','kept comment');
            INSERT INTO runs(card_id,column_id,harness,argv_json,prompt_snapshot,herdr_pane_id)
              VALUES(1,1,'pi','[]','kept prompt','p1');
            PRAGMA user_version=4;
            "#,
        )
        .unwrap();
    }

    let db = Db::open(&path).unwrap();
    let global = db.get_board(BOARD_ID).unwrap();
    assert_eq!(db.user_version().unwrap(), 8);
    assert_eq!(global.name, "Global");
    assert!(global.scope_path.is_none());
    let cards = db.list_cards(BOARD_ID).unwrap();
    assert_eq!(cards[0].title, "kept");
    assert_eq!(
        db.list_comments(cards[0].id).unwrap()[0].body,
        "kept comment"
    );
    assert_eq!(
        db.list_runs(cards[0].id).unwrap()[0]
            .herdr_pane_id
            .as_deref(),
        Some("p1")
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
fn migration_v6_rebuilds_cards_check_and_preserves_data() {
    const V5_SCHEMA: &str = "
    CREATE TABLE boards (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
      scope_path TEXT,
      created_at TEXT NOT NULL DEFAULT (datetime('now')));
    CREATE UNIQUE INDEX idx_boards_scope_path ON boards(scope_path)
      WHERE scope_path IS NOT NULL;
    CREATE TABLE columns (id INTEGER PRIMARY KEY, board_id INTEGER NOT NULL,
      name TEXT NOT NULL, position INTEGER NOT NULL, system_prompt TEXT,
      trigger TEXT NOT NULL DEFAULT 'manual', on_success_column_id INTEGER,
      on_fail_column_id INTEGER, fresh_session INTEGER NOT NULL DEFAULT 0,
      harness_override TEXT, model_override TEXT, effort_override TEXT,
      permission_override TEXT, timeout_minutes INTEGER, UNIQUE (board_id, name));
    CREATE TABLE cards (id INTEGER PRIMARY KEY, board_id INTEGER NOT NULL,
      column_id INTEGER NOT NULL, position INTEGER NOT NULL, title TEXT NOT NULL,
      description TEXT NOT NULL DEFAULT '', harness TEXT NOT NULL DEFAULT 'pi',
      model TEXT, effort TEXT, permission_mode TEXT, session TEXT,
      space_kind TEXT NOT NULL DEFAULT 'workspace'
        CHECK (space_kind IN ('workspace','new_workspace')),
      space_ref TEXT, space_cwd TEXT,
      status TEXT NOT NULL DEFAULT 'idle'
        CHECK (status IN ('idle','queued','running','blocked','failed')),
      session_id TEXT,
      created_at TEXT NOT NULL DEFAULT (datetime('now')),
      updated_at TEXT NOT NULL DEFAULT (datetime('now')), archived_at TEXT);
    CREATE INDEX idx_cards_column ON cards(column_id, position);
    CREATE TABLE comments (id INTEGER PRIMARY KEY, card_id INTEGER NOT NULL,
      author TEXT NOT NULL, body TEXT NOT NULL,
      created_at TEXT NOT NULL DEFAULT (datetime('now')));
    CREATE TABLE runs (id INTEGER PRIMARY KEY, card_id INTEGER NOT NULL,
      column_id INTEGER NOT NULL, harness TEXT NOT NULL, argv_json TEXT NOT NULL,
      prompt_snapshot TEXT NOT NULL, herdr_workspace_id TEXT, herdr_pane_id TEXT,
      session_id TEXT, session TEXT, started_at TEXT, ended_at TEXT, outcome TEXT,
      result_summary TEXT, log_path TEXT);
    ";
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(V5_SCHEMA).unwrap();
        conn.execute("INSERT INTO boards (id, name) VALUES (1, 'Global')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO columns (id, board_id, name, position) VALUES (1, 1, 'Todo', 0)",
            [],
        )
        .unwrap();
        // One blocked card (a non-default status must survive the rebuild) and
        // one plain idle card.
        conn.execute(
            "INSERT INTO cards (id,board_id,column_id,position,title,status)
             VALUES (1,1,1,0,'kept','blocked')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (id,board_id,column_id,position,title)
             VALUES (2,1,1,1,'idle-card')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO comments (card_id,author,body) VALUES (1,'user','kept comment')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO runs (card_id,column_id,harness,argv_json,prompt_snapshot,outcome)
             VALUES (1,1,'pi','[]','kept prompt','ok')",
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 5;").unwrap();
    }

    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 8);
    let cards = db.list_cards(BOARD_ID).unwrap();
    assert_eq!(cards.len(), 2);
    let kept = &cards[0];
    assert_eq!(kept.title, "kept");
    assert_eq!(kept.status, CardStatus::Blocked);
    // No backfill: existing rows get awaiting_reason NULL and idle stays idle.
    assert!(kept.awaiting_reason.is_none());
    assert_eq!(cards[1].status, CardStatus::Idle);
    assert!(cards[1].awaiting_reason.is_none());
    // Related tables untouched.
    assert_eq!(db.list_comments(kept.id).unwrap()[0].body, "kept comment");
    assert_eq!(
        db.list_runs(kept.id).unwrap()[0].outcome,
        Some(RunOutcome::Ok)
    );

    // The new CHECK accepts only invariant-preserving status/reason pairs.
    let card = db
        .set_card_awaiting(kept.id, AwaitingReason::AgentDone)
        .unwrap();
    assert_eq!(card.status, CardStatus::Awaiting);
    let card = db.set_card_status(card.id, CardStatus::Done).unwrap();
    assert_eq!(card.status, CardStatus::Done);
    assert!(card.awaiting_reason.is_none());
    drop(db);

    let conn = Connection::open(path).unwrap();
    assert!(conn
        .execute(
            "UPDATE cards SET status='awaiting', awaiting_reason=NULL WHERE id=1",
            [],
        )
        .is_err());
    assert!(conn
        .execute(
            "UPDATE cards SET status='awaiting', awaiting_reason='bogus' WHERE id=1",
            [],
        )
        .is_err());
    assert!(conn
        .execute(
            "UPDATE cards SET status='done', awaiting_reason='agent_done' WHERE id=1",
            [],
        )
        .is_err());
}

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
        .create_run(card.id, source.id, "pi", "[]", "p", None, None)
        .unwrap();
    db.finish_run(run.id, RunOutcome::Fail, None).unwrap();

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
