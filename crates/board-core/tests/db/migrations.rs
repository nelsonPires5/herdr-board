use super::mem;
use board_core::db::{Db, EnqueueRun, FinalizeRun, BOARD_ID};
use board_core::protocol::{
    AwaitingReason, CardCreateParams, CardStatus, ColumnCreateParams, Effort, RunOutcome,
    SpaceKind, Trigger,
};
use rusqlite::{Connection, OptionalExtension};

#[test]
fn migration_seeds_board_and_todo_column() {
    let db = mem();
    assert_eq!(db.user_version().unwrap(), 11);
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
fn fresh_v11_launch_spec_column_has_exact_nullable_default() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(Db::open(&path).unwrap());
    let conn = Connection::open(path).unwrap();
    let shape: (String, String, i64, Option<String>) = conn
        .query_row(
            "SELECT name,type,\"notnull\",dflt_value FROM pragma_table_info('runs') WHERE name='launch_spec_json'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(shape, ("launch_spec_json".into(), "TEXT".into(), 0, None));
    let default_value: Option<String> = conn
        .query_row("SELECT launch_spec_json FROM runs LIMIT 1", [], |row| {
            row.get(0)
        })
        .optional()
        .unwrap()
        .flatten();
    assert_eq!(default_value, None);
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
        assert_eq!(db.user_version().unwrap(), 11);
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
    assert_eq!(db.user_version().unwrap(), 11);
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
    db.finalize_run_uow(&FinalizeRun {
        run_id: preserved.id,
        outcome: RunOutcome::Ok,
        summary: None,
        comments: &[],
        target_column_id: None,
        final_status: CardStatus::Done,
        final_awaiting_reason: None,
        next: None,
    })
    .unwrap();
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "claude",
            argv_json: "[]",
            prompt_snapshot: "p",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
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
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "claude",
                argv_json: "[]",
                prompt_snapshot: "prompt",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: Some("session"),
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, None, None, None).unwrap();
        // v4 CHECK constraint only allows idle/queued/running/blocked/failed;
        // 'done' was added later, so finalize with Idle for backward compat.
        db.finalize_run_uow(&FinalizeRun {
            run_id: run.id,
            outcome: RunOutcome::Ok,
            summary: None,
            comments: &[],
            target_column_id: None,
            final_status: CardStatus::Idle,
            final_awaiting_reason: None,
            next: None,
        })
        .unwrap();
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
    assert_eq!(db.user_version().unwrap(), 11);
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
        assert_eq!(db.user_version().unwrap(), 11);
    }
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version = 8;").unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 11);
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
    assert_eq!(db.user_version().unwrap(), 11);
    let cards = db.list_cards(BOARD_ID).unwrap();
    assert_eq!(cards.len(), 1);
    assert!(cards[0].archived_at.is_none());
}

#[test]
fn v10_to_v11_preserves_existing_run_bytes_and_null_across_reopen() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let run_id = {
        let db = Db::open(&path).unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "v10".into(),
                ..Default::default()
            })
            .unwrap();
        db.enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "pi",
            argv_json: r#"["pi","exact\\n\u0000  "]"#,
            prompt_snapshot: "prompt\n\0  ",
            system_prompt_snapshot: Some("system\n  "),
            launch_spec_json: None,
            session_id: Some("sid"),
            session: Some("herdr-session"),
        })
        .unwrap()
        .id
    };
    let conn = Connection::open(&path).unwrap();
    let before: (String, String, String, String) = conn
        .query_row(
            "SELECT argv_json,prompt_snapshot,system_prompt_snapshot,session FROM runs WHERE id=?1",
            [run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    conn.execute_batch("ALTER TABLE runs DROP COLUMN launch_spec_json; PRAGMA user_version=10;")
        .unwrap();
    drop(conn);
    for _ in 0..2 {
        let db = Db::open(&path).unwrap();
        let run = db.get_run(run_id).unwrap();
        assert_eq!(run.launch_spec, None);
        drop(db);
        let conn = Connection::open(&path).unwrap();
        let after: (String, String, String, String, Option<String>) = conn.query_row(
            "SELECT argv_json,prompt_snapshot,system_prompt_snapshot,session,launch_spec_json FROM runs WHERE id=?1",
            [run_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
        assert_eq!(
            (&after.0, &after.1, &after.2, &after.3),
            (&before.0, &before.1, &before.2, &before.3)
        );
        assert_eq!(after.4, None);
    }
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
        // v6→v7 migration fixture: enqueue at v11, then manually drop
        // system_prompt_snapshot and downgrade to user_version=6 so the
        // migration path re-adds the column.
        db.enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "pi",
            argv_json: argv,
            prompt_snapshot: prompt,
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
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
    assert_eq!(db.user_version().unwrap(), 11);
    let run = &db.list_runs(1).unwrap()[0];
    assert_eq!(run.argv_json, argv);
    assert_eq!(run.prompt_snapshot, prompt);
    assert_eq!(run.system_prompt_snapshot, None);
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
    assert_eq!(db.user_version().unwrap(), 11);
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
    assert_eq!(db.user_version().unwrap(), 11);
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
fn v8_to_v9_derives_timeout_state_once_from_durable_history() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let (running_id, awaiting_id, unlimited_id, ended_id);
    {
        let db = Db::open(&path).unwrap();
        let timed = db
            .create_column(&ColumnCreateParams {
                name: "timed".into(),
                timeout_minutes: Some(7),
                ..Default::default()
            })
            .unwrap();
        let unlimited = db
            .create_column(&ColumnCreateParams {
                name: "unlimited".into(),
                ..Default::default()
            })
            .unwrap();
        let make = |title: &str, column_id: i64| {
            db.create_card(&CardCreateParams {
                title: title.into(),
                column_id: Some(column_id),
                ..Default::default()
            })
            .unwrap()
        };
        let running = make("running", timed.id);
        let awaiting = make("awaiting", timed.id);
        let unlimited_card = make("unlimited", unlimited.id);
        let ended = make("ended", timed.id);
        running_id = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: running.id,
                column_id: timed.id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap()
            .id;
        awaiting_id = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: awaiting.id,
                column_id: timed.id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap()
            .id;
        unlimited_id = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: unlimited_card.id,
                column_id: unlimited.id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap()
            .id;
        ended_id = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: ended.id,
                column_id: timed.id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap()
            .id;
    }
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "UPDATE runs SET started_at='2026-01-02 03:04:05', timeout_deadline_at_ms=NULL, timeout_paused_at_ms=NULL;
             UPDATE runs SET ended_at='2026-01-02 03:05:05', outcome='ok' WHERE id={ended_id};
             UPDATE cards SET status='running' WHERE id=(SELECT card_id FROM runs WHERE id={running_id});
             UPDATE cards SET status='awaiting', awaiting_reason='agent_done', updated_at='2026-01-02 03:06:07' WHERE id=(SELECT card_id FROM runs WHERE id={awaiting_id});
             UPDATE cards SET status='running' WHERE id=(SELECT card_id FROM runs WHERE id={unlimited_id});
             PRAGMA user_version=8;"
        )).unwrap();
    }
    let expected_start_ms: i64 = Connection::open_in_memory()
        .unwrap()
        .query_row("SELECT unixepoch('2026-01-02 03:04:05') * 1000", [], |r| {
            r.get(0)
        })
        .unwrap();
    let expected_pause_ms: i64 = Connection::open_in_memory()
        .unwrap()
        .query_row("SELECT unixepoch('2026-01-02 03:06:07') * 1000", [], |r| {
            r.get(0)
        })
        .unwrap();
    {
        let db = Db::open(&path).unwrap();
        assert_eq!(
            db.get_run(running_id).unwrap().timeout_deadline_at_ms,
            Some(expected_start_ms + 420_000)
        );
        let awaiting = db.get_run(awaiting_id).unwrap();
        assert_eq!(
            awaiting.timeout_deadline_at_ms,
            Some(expected_start_ms + 420_000)
        );
        assert_eq!(awaiting.timeout_paused_at_ms, Some(expected_pause_ms));
        assert_eq!(
            db.get_run(unlimited_id).unwrap().timeout_deadline_at_ms,
            None
        );
        assert_eq!(db.get_run(ended_id).unwrap().timeout_deadline_at_ms, None);
    }
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE runs SET timeout_deadline_at_ms=123 WHERE id=?1",
        [running_id],
    )
    .unwrap();
    drop(conn);
    assert_eq!(
        Db::open(&path)
            .unwrap()
            .get_run(running_id)
            .unwrap()
            .timeout_deadline_at_ms,
        Some(123)
    );
}
