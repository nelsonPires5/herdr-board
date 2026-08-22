use super::{create_file_db, enqueue, mem};
use board_core::db::{Db, EnqueueRun, FinalizeRun, BOARD_ID};
use board_core::protocol::{
    AwaitingReason, CardCreateParams, CardStatus, ColumnCreateParams, Effort, RunOutcome,
    SpaceKind, Trigger,
};
use rusqlite::{types::Value, Connection, OptionalExtension};

const INDEX_SQL: &str =
    "CREATE UNIQUE INDEX idx_runs_one_open_per_card ON runs(card_id) WHERE ended_at IS NULL";
const QUEUED_INDEX_SQL: &str =
    "CREATE INDEX idx_runs_queued_fifo ON runs(id) WHERE started_at IS NULL AND ended_at IS NULL";
const ACTIVE_INDEX_SQL: &str =
    "CREATE INDEX idx_runs_active_open ON runs(id) WHERE started_at IS NOT NULL AND ended_at IS NULL";

fn raw_rows(conn: &Connection, table: &str) -> Vec<Vec<Value>> {
    let mut statement = conn
        .prepare(&format!("SELECT * FROM {table} ORDER BY id"))
        .unwrap();
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            (0..columns)
                .map(|column| row.get(column))
                .collect::<rusqlite::Result<Vec<Value>>>()
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn scheduler_index_sql(conn: &Connection, name: &str) -> Option<String> {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='index' AND name=?1",
        [name],
        |row| row.get(0),
    )
    .ok()
}

// ---------------------------------------------------------------------------
// Schema-v15 archive state: fresh shape, upgrade, and replay safety.
// ---------------------------------------------------------------------------

#[test]
fn fresh_schema_stamps_v15_with_nullable_archive_columns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    drop(Db::open(&path).unwrap());
    let conn = Connection::open(path).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        15
    );
    for table in ["projects", "boards"] {
        let shape: (String, String, i64, Option<String>) = conn
            .query_row(
                &format!(
                    "SELECT name,type,\"notnull\",dflt_value FROM pragma_table_info('{table}')
                     WHERE name='archived_at'"
                ),
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap_or_else(|_| panic!("{table}.archived_at missing"));
        assert_eq!(
            shape,
            ("archived_at".into(), "TEXT".into(), 0, None),
            "{table}.archived_at must be a nullable TEXT with no default"
        );
    }
}

/// A schema-v14 database upgrades to v15 by gaining `archived_at` on projects
/// and boards while every existing row — boards, cards, comments, runs,
/// selection, and recency — survives untouched.
#[test]
fn v14_to_v15_migration_preserves_all_project_and_board_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v14.db");
    let (project_id, board_id, card_id, run_id) = {
        let db = Db::open(&path).unwrap();
        let project = db.get_or_create_project("/tmp/kept-scope").unwrap();
        let board = db.create_board(project.id, "extra").unwrap();
        db.select_board(board.id).unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "kept".into(),
                board_id: Some(board.id),
                ..Default::default()
            })
            .unwrap();
        db.add_comment(card.id, "user", "kept comment").unwrap();
        let run = db
            .enqueue_run_uow(&enqueue(card.id, card.column_id))
            .unwrap();
        drop(db);
        let conn = Connection::open(&path).unwrap();
        let before = (
            raw_rows(&conn, "projects"),
            raw_rows(&conn, "boards"),
            raw_rows(&conn, "columns"),
            raw_rows(&conn, "cards"),
            raw_rows(&conn, "comments"),
            raw_rows(&conn, "runs"),
            // selection/board_selection/recency have no uniform `id` column;
            // pin them by their meaningful fields instead.
            conn.query_row("SELECT project_id FROM selection WHERE id=1", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
            conn.query_row(
                "SELECT board_id FROM board_selection WHERE project_id=?1",
                [project.id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
            conn.query_row("SELECT count(*) FROM project_recents", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
            conn.query_row(
                "SELECT count(*) FROM board_recents WHERE project_id=?1",
                [project.id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
        );
        conn.execute_batch(
            "ALTER TABLE projects DROP COLUMN archived_at;
             ALTER TABLE boards DROP COLUMN archived_at;
             PRAGMA user_version = 14;",
        )
        .unwrap();
        drop(conn);
        // Reopen twice: the upgrade must be stable across reopen.
        for reopen in 0..2 {
            let db = Db::open(&path).unwrap();
            assert_eq!(db.user_version().unwrap(), 15, "reopen {reopen}");
            assert_eq!(
                db.get_project(project.id).unwrap().archived_at,
                None,
                "reopen {reopen}"
            );
            assert_eq!(
                db.get_board(board.id).unwrap().archived_at,
                None,
                "reopen {reopen}"
            );
            drop(db);
        }
        let conn = Connection::open(&path).unwrap();
        // The v15 ALTERs add trailing nullable columns, so the untouched
        // prefix bytes of every pre-existing row compare equal via raw_rows on
        // the tables the migration rewrites (none — it is pure ALTER).
        assert_eq!(
            (
                raw_rows(&conn, "columns"),
                raw_rows(&conn, "cards"),
                raw_rows(&conn, "comments"),
                raw_rows(&conn, "runs"),
                conn.query_row("SELECT project_id FROM selection WHERE id=1", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
                conn.query_row(
                    "SELECT board_id FROM board_selection WHERE project_id=?1",
                    [project.id],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap(),
                conn.query_row("SELECT count(*) FROM project_recents", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
                conn.query_row(
                    "SELECT count(*) FROM board_recents WHERE project_id=?1",
                    [project.id],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap(),
            ),
            (
                before.2.clone(),
                before.3.clone(),
                before.4.clone(),
                before.5.clone(),
                before.6,
                before.7,
                before.8,
                before.9,
            ),
            "v15 must not rewrite a single related row"
        );
        // project/board rows keep their identity columns byte for byte (the
        // new trailing column reads NULL for every pre-existing row).
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM projects WHERE archived_at IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM boards WHERE archived_at IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
            0
        );
        drop(conn);
        (project.id, board.id, card.id, run.id)
    };
    // Sanity: the fixture data is fully readable through the Db API.
    let db = Db::open(&path).unwrap();
    assert_eq!(db.get_project(project_id).unwrap().name, "kept-scope");
    assert_eq!(db.get_board(board_id).unwrap().name, "extra");
    assert_eq!(db.get_card(card_id).unwrap().unwrap().title, "kept");
    assert_eq!(db.list_comments(card_id).unwrap()[0].body, "kept comment");
    assert_eq!(db.get_run(run_id).unwrap().id, run_id);
    assert_eq!(
        db.selected_board_for(project_id).unwrap().unwrap().id,
        board_id
    );
}

/// Replaying the v15 step over an already-upgraded shape (a stale stamp) must
/// be a guarded no-op, and a hard failure must leave `user_version` untouched
/// and stay stable on retry.
#[test]
fn v15_migration_replay_and_failure_are_stable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replay.db");
    drop(Db::open(&path).unwrap());
    // Replay: rewind the stamp over the finished shape.
    Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA user_version = 14;")
        .unwrap();
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 15);
    drop(db);

    // Failure: `projects` cannot take a column (a view), so the upgrade
    // aborts without advancing the stamp and retries behave identically.
    let dir2 = tempfile::tempdir().unwrap();
    let path2 = dir2.path().join("malformed-v14.db");
    drop(Db::open(&path2).unwrap());
    let conn = Connection::open(&path2).unwrap();
    conn.execute_batch(
        "DROP TABLE projects;
         CREATE VIEW projects AS SELECT 1 AS id;
         PRAGMA user_version = 14;",
    )
    .unwrap();
    drop(conn);
    for attempt in 0..2 {
        let error = match Db::open(&path2) {
            Ok(_) => panic!("attempt {attempt}: malformed v14 unexpectedly migrated"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("view"), "{error}");
        let conn = Connection::open(&path2).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            14
        );
    }
}

#[test]
fn migration_seeds_board_and_todo_column() {
    let db = mem();
    assert_eq!(db.user_version().unwrap(), 15);
    let board = db.get_board(BOARD_ID).unwrap();
    // The Global project keeps the legacy board id 1, renamed `main` by v14;
    // the Global identity now lives on the project itself.
    assert_eq!(db.get_project(1).unwrap().name, "Global");
    assert_eq!(board.name, "main");
    assert_eq!(board.scope_path, None);
    let cols = db.list_columns(BOARD_ID).unwrap();
    assert_eq!(cols.len(), 1);
    assert_eq!(cols[0].name, "Todo");
    assert_eq!(cols[0].position, 0);
    assert_eq!(cols[0].trigger, Trigger::Manual);
}

#[test]
fn fresh_v12_launch_and_anchor_columns_have_exact_nullable_defaults() {
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
    let anchor_shape: (String, String, i64, Option<String>) = conn
        .query_row(
            "SELECT name,type,\"notnull\",dflt_value FROM pragma_table_info('runs') WHERE name='herdr_anchor_pane_id'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        anchor_shape,
        ("herdr_anchor_pane_id".into(), "TEXT".into(), 0, None)
    );
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
fn v11_rows_gain_nullable_anchor_column_without_backfill() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    {
        let db = Db::open(&path).unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "v11 anchor compatibility".into(),
                ..Default::default()
            })
            .unwrap();
        db.enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "pi",
            argv_json: "[\"pi\"]",
            prompt_snapshot: "legacy",
            system_prompt_snapshot: Some("system"),
            launch_spec_json: Some(
                "{\"version\":1,\"execution\":{\"argv\":[],\"env\":[],\"agent_kind\":null,\"initial_prompt\":null,\"system_prompt\":null}}",
            ),
            session_id: None,
            session: Some("default"),
        })
        .unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "ALTER TABLE runs DROP COLUMN herdr_anchor_pane_id; PRAGMA user_version=11;",
        )
        .unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 15);
    assert_eq!(db.list_runs(1).unwrap()[0].herdr_anchor_pane_id, None);
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
        assert_eq!(db.user_version().unwrap(), 15);
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
    // Open via Db → runs the v2 through v14 migrations.
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 15);
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
            // A v3-era boards table had no scope_path column (added in v5);
            // the v14 boards shape already has none (it lives on projects),
            // so only the pre-Global name and the version stamp need rewinding.
            "UPDATE boards SET name='main' WHERE id=1;
             PRAGMA user_version = 3;",
        )
        .unwrap();
    }

    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 15);
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

/// A file stamped *above* the current schema version — one written by a newer
/// board — must not be downgraded, re-seeded, or rewritten when an older binary
/// opens it. `migrate()` only runs below `SCHEMA_VERSION`, so the open is a
/// no-op today: it succeeds silently and leaves both the version and the rows
/// exactly as found. There is deliberately no future-version *refusal* pinned
/// here; this test records the behaviour that exists.
#[test]
fn migration_does_not_downgrade_future_schema_version() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let card_id = {
        let db = Db::open(&path).unwrap();
        assert_eq!(db.user_version().unwrap(), 15);
        db.create_card(&CardCreateParams {
            title: "written by a newer board".into(),
            ..Default::default()
        })
        .unwrap()
        .id
    };
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version = 99;").unwrap();
    }
    let before = {
        let conn = Connection::open(&path).unwrap();
        (
            raw_rows(&conn, "boards"),
            raw_rows(&conn, "columns"),
            raw_rows(&conn, "cards"),
        )
    };

    let db = Db::open(&path).unwrap();
    assert_eq!(
        db.user_version().unwrap(),
        99,
        "a future user_version must never be stamped back down"
    );
    assert_eq!(
        db.get_card(card_id).unwrap().unwrap().title,
        "written by a newer board"
    );
    assert_eq!(
        db.list_columns(BOARD_ID).unwrap().len(),
        1,
        "a future-version file must not be re-seeded"
    );
    drop(db);

    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        (
            raw_rows(&conn, "boards"),
            raw_rows(&conn, "columns"),
            raw_rows(&conn, "cards"),
        ),
        before,
        "opening a future-version file must not rewrite a single row"
    );
}

/// Re-stamping a *past* version onto an otherwise current file replays
/// migrations 8→14 over the shape they already produced. Every step must be
/// guarded, so the replay is a no-op that lands back on the current version.
/// (This is the behaviour `migration_does_not_downgrade_future_schema_version`
/// used to exercise before it was rewritten into a genuine future-version test;
/// `migration_idempotent_on_reopen` covers the reopen case, where `migrate()`
/// is skipped entirely.)
#[test]
fn migration_replay_from_a_past_version_stamp_is_a_no_op() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    {
        let db = Db::open(&path).unwrap();
        assert_eq!(db.user_version().unwrap(), 15);
    }
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version = 8;").unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 15);
    assert_eq!(db.list_columns(BOARD_ID).unwrap().len(), 1);
    assert_eq!(db.get_board(BOARD_ID).unwrap().name, "main");
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
            // A v2-era boards table had no scope_path column (added in v5);
            // the v14 boards shape already has none (it lives on projects),
            // so only the pre-Global name and the version stamp need rewinding.
            "ALTER TABLE cards DROP COLUMN archived_at;
             UPDATE boards SET name='main' WHERE id=1;
             PRAGMA user_version = 2;",
        )
        .unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 15);
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
    assert_eq!(db.user_version().unwrap(), 15);
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
    assert_eq!(db.user_version().unwrap(), 15);
    // v5 still renames the legacy board to Global; v14 moves that identity
    // onto the Global project and renames the board itself back to `main`.
    assert_eq!(db.get_project(1).unwrap().name, "Global");
    assert_eq!(global.name, "main");
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
    assert_eq!(db.user_version().unwrap(), 15);
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

// ---------------------------------------------------------------------------
// Schema-v14 index shape and the migration paths that must not touch bytes.
// ---------------------------------------------------------------------------

#[test]
fn fresh_v12_has_exact_partial_scheduler_indexes_and_query_plans() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 15);
    drop(db);
    let conn = Connection::open(path).unwrap();
    for (name, expected) in [
        ("idx_runs_queued_fifo", QUEUED_INDEX_SQL),
        ("idx_runs_active_open", ACTIVE_INDEX_SQL),
    ] {
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='index' AND name=?1",
                [name],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sql, expected);
    }
    for (sql, index) in [
        (
            "EXPLAIN QUERY PLAN SELECT id, card_id FROM runs WHERE started_at IS NULL AND ended_at IS NULL ORDER BY id",
            "idx_runs_queued_fifo",
        ),
        (
            "EXPLAIN QUERY PLAN SELECT id, card_id FROM runs WHERE started_at IS NOT NULL AND ended_at IS NULL ORDER BY id",
            "idx_runs_active_open",
        ),
    ] {
        let detail: String = conn.query_row(sql, [], |row| row.get(3)).unwrap();
        assert!(detail.contains(index), "unexpected plan: {detail}");
    }
}

#[test]
fn v9_file_fixture_upgrades_through_v14_without_changing_existing_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v9.db");
    let (card_id, run_id) = {
        let db = Db::open(&path).unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "v9 exact \0 title  ".into(),
                description: Some("line one\nline two  ".into()),
                ..Default::default()
            })
            .unwrap();
        // Historical v9→v14 migration fixture: enqueue_run_uow writes a
        // current-shape row; after manual downgrade to user_version=9 the
        // migration path still re-adds indexes and must preserve every byte.
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "pi",
                argv_json: r#"["pi","exact\\nargv"]"#,
                prompt_snapshot: "prompt\nbytes\0  ",
                system_prompt_snapshot: Some("system\nbytes  "),
                launch_spec_json: None,
                session_id: Some("session-id"),
                session: Some("session-name"),
            })
            .unwrap();
        (card.id, run.id)
    };
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP INDEX idx_runs_queued_fifo;
         DROP INDEX idx_runs_active_open;
         PRAGMA user_version=9;",
    )
    .unwrap();
    let before_cards = raw_rows(&conn, "cards");
    let before_runs = raw_rows(&conn, "runs");
    drop(conn);

    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 15);
    // v14 rebuilds `boards` (id preserved, name becomes `main`); cards and
    // runs keep every byte.
    assert_eq!(db.get_board(BOARD_ID).unwrap().id, BOARD_ID);
    assert_eq!(db.get_board(BOARD_ID).unwrap().name, "main");
    assert_eq!(db.get_card(card_id).unwrap().unwrap().id, card_id);
    assert_eq!(db.get_run(run_id).unwrap().id, run_id);
    drop(db);

    for reopen in 0..2 {
        let conn = Connection::open(&path).unwrap();
        assert_eq!(raw_rows(&conn, "cards"), before_cards, "reopen {reopen}");
        assert_eq!(raw_rows(&conn, "runs"), before_runs, "reopen {reopen}");
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            15
        );
        assert_eq!(
            scheduler_index_sql(&conn, "idx_runs_queued_fifo").as_deref(),
            Some(QUEUED_INDEX_SQL)
        );
        assert_eq!(
            scheduler_index_sql(&conn, "idx_runs_active_open").as_deref(),
            Some(ACTIVE_INDEX_SQL)
        );
        drop(conn);
        drop(Db::open(&path).unwrap());
    }
}

#[test]
fn v10_to_v11_migration_failure_is_atomic_and_stable_on_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("malformed-v10.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE VIEW runs AS SELECT 1 AS id; PRAGMA user_version=10;")
        .unwrap();
    drop(conn);

    for attempt in 0..2 {
        let error = match Db::open(&path) {
            Ok(_) => panic!("attempt {attempt}: malformed v10 unexpectedly migrated"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("Cannot add a column to a view"), "{error}");
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            10
        );
        let has_column: i64 = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('runs') WHERE name='launch_spec_json')",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(has_column, 0);
    }
}

#[test]
fn v10_conflicting_index_failure_is_atomic_and_stable_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conflict.db");
    drop(Db::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP INDEX idx_runs_queued_fifo;
         DROP INDEX idx_runs_active_open;
         CREATE INDEX idx_runs_active_open ON runs(card_id);
         PRAGMA user_version=9;",
    )
    .unwrap();
    drop(conn);

    for attempt in 0..2 {
        let error = match Db::open(&path) {
            Ok(_) => panic!("attempt {attempt}: conflicting migration unexpectedly succeeded"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("idx_runs_active_open"), "{error}");
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            9
        );
        assert_eq!(scheduler_index_sql(&conn, "idx_runs_queued_fifo"), None);
        assert_eq!(
            scheduler_index_sql(&conn, "idx_runs_active_open").as_deref(),
            Some("CREATE INDEX idx_runs_active_open ON runs(card_id)")
        );
    }
}

#[test]
fn v8_migration_rejects_duplicate_open_runs_without_advancing_version_or_index() {
    let (_dir, path, card) = create_file_db("duplicate");
    let db = Db::open(&path).unwrap();
    let second_card = db
        .create_card(&CardCreateParams {
            title: "second duplicate".into(),
            ..Default::default()
        })
        .unwrap();
    drop(db);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("DROP INDEX idx_runs_one_open_per_card; PRAGMA user_version=7;")
        .unwrap();
    for (duplicate_card, prompt) in [
        (&card, "first"),
        (&card, "second"),
        (&card, "third"),
        (&second_card, "fourth"),
        (&second_card, "fifth"),
    ] {
        conn.execute(
            "INSERT INTO runs(card_id,column_id,harness,argv_json,prompt_snapshot)
             VALUES(?1,?2,'pi','[]',?3)",
            (duplicate_card.id, duplicate_card.column_id, prompt),
        )
        .unwrap();
    }
    let old_version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let run_ids: Vec<i64> = conn
        .prepare("SELECT id FROM runs ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    drop(conn);

    let error = match Db::open(&path) {
        Ok(_) => panic!("migration unexpectedly succeeded"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains(&format!("card {}", card.id)), "{error}");
    assert!(
        error.contains(&format!("card {}", second_card.id)),
        "{error}"
    );
    for run_id in &run_ids {
        assert!(error.contains(&run_id.to_string()), "{error}");
    }
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        old_version
    );
    assert_eq!(old_version, 7);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='idx_runs_one_open_per_card'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
}

#[test]
fn v8_upgrade_retains_a_single_open_run_byte_for_byte() {
    let (_dir, path, card) = create_file_db("single open retained");
    let db = Db::open(&path).unwrap();
    let before = db
        .enqueue_run_uow(&enqueue(card.id, card.column_id))
        .unwrap();
    drop(db);
    Connection::open(&path)
        .unwrap()
        .execute_batch("DROP INDEX idx_runs_one_open_per_card; PRAGMA user_version=7;")
        .unwrap();

    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 15);
    assert_eq!(db.get_run(before.id).unwrap(), before);
}

#[test]
fn fresh_and_v7_upgrade_install_exact_partial_unique_index_sql() {
    for from_v7 in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.db");
        let db = Db::open(&path).unwrap();
        drop(db);
        if from_v7 {
            Connection::open(&path)
                .unwrap()
                .execute_batch("DROP INDEX idx_runs_one_open_per_card; PRAGMA user_version=7;")
                .unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.user_version().unwrap(), 15);
        drop(db);
        let sql: String = Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='index' AND name='idx_runs_one_open_per_card'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sql, INDEX_SQL);
    }
}
