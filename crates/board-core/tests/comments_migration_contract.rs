//! RED migration contract for the v12 -> v14 upgrade (comment audit schema
//! plus the projects/selection schema).

use board_core::db::Db;
use rusqlite::Connection;

// Keep the fixture deliberately independent from the current fresh-schema
// path.  This is the public `Db::open` migration boundary, not a private
// migration-function test.
const V12_SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;
CREATE TABLE boards (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  scope_path TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX idx_boards_scope_path ON boards(scope_path) WHERE scope_path IS NOT NULL;
CREATE TABLE columns (
  id INTEGER PRIMARY KEY,
  board_id INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  position INTEGER NOT NULL,
  system_prompt TEXT,
  trigger TEXT NOT NULL DEFAULT 'manual' CHECK (trigger IN ('manual','auto')),
  on_success_column_id INTEGER REFERENCES columns(id) ON DELETE SET NULL,
  on_fail_column_id INTEGER REFERENCES columns(id) ON DELETE SET NULL,
  fresh_session INTEGER NOT NULL DEFAULT 0,
  harness_override TEXT,
  model_override TEXT,
  effort_override TEXT,
  permission_override TEXT,
  timeout_minutes INTEGER,
  UNIQUE (board_id, name)
);
CREATE TABLE cards (
  id INTEGER PRIMARY KEY,
  board_id INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
  column_id INTEGER NOT NULL REFERENCES columns(id),
  position INTEGER NOT NULL,
  title TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  harness TEXT NOT NULL DEFAULT 'pi',
  model TEXT,
  effort TEXT CHECK (effort IN (NULL,'off','minimal','low','medium','high','xhigh','max')),
  permission_mode TEXT,
  session TEXT,
  space_kind TEXT NOT NULL DEFAULT 'workspace' CHECK (space_kind IN ('workspace','new_workspace')),
  space_ref TEXT,
  space_cwd TEXT,
  status TEXT NOT NULL DEFAULT 'idle'
    CHECK (status IN ('idle','queued','running','blocked','failed','awaiting','done')),
  awaiting_reason TEXT,
  session_id TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
  archived_at TEXT,
  CHECK (
    (status = 'awaiting' AND awaiting_reason IS NOT NULL
      AND awaiting_reason IN ('agent_done','idle_expired'))
    OR (status <> 'awaiting' AND awaiting_reason IS NULL)
  )
);
CREATE TABLE comments (
  id INTEGER PRIMARY KEY,
  card_id INTEGER NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  author TEXT NOT NULL,
  body TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE runs (
  id INTEGER PRIMARY KEY,
  card_id INTEGER NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  column_id INTEGER NOT NULL REFERENCES columns(id),
  harness TEXT NOT NULL,
  argv_json TEXT NOT NULL,
  prompt_snapshot TEXT NOT NULL,
  system_prompt_snapshot TEXT,
  launch_spec_json TEXT,
  herdr_workspace_id TEXT,
  herdr_pane_id TEXT,
  herdr_anchor_pane_id TEXT,
  session_id TEXT,
  session TEXT,
  started_at TEXT,
  timeout_deadline_at_ms INTEGER,
  timeout_paused_at_ms INTEGER,
  ended_at TEXT,
  outcome TEXT CHECK (outcome IN (NULL,'ok','fail','cancelled','lost')),
  result_summary TEXT,
  log_path TEXT
);
CREATE INDEX idx_cards_column ON cards(column_id, position);
CREATE INDEX idx_comments_card ON comments(card_id, created_at);
CREATE INDEX idx_runs_card ON runs(card_id, started_at);
CREATE UNIQUE INDEX idx_runs_one_open_per_card ON runs(card_id) WHERE ended_at IS NULL;
CREATE INDEX idx_runs_queued_fifo ON runs(id) WHERE started_at IS NULL AND ended_at IS NULL;
CREATE INDEX idx_runs_active_open ON runs(id) WHERE started_at IS NOT NULL AND ended_at IS NULL;
"#;

#[test]
fn v12_to_v13_preserves_board_card_comment_and_run_bytes() {
    let tmp = tempfile::NamedTempFile::new().expect("temporary database path");
    let path = tmp.path().to_path_buf();
    {
        let conn = Connection::open(&path).expect("v12 database");
        conn.execute_batch(V12_SCHEMA).expect("v12 schema");
        conn.execute(
            "INSERT INTO boards(id,name,scope_path,created_at)
             VALUES(1,'Global',NULL,'2025-01-02 03:04:05')",
            [],
        )
        .expect("board");
        conn.execute(
            "INSERT INTO columns(id,board_id,name,position,trigger,fresh_session)
             VALUES(1,1,'Todo',0,'manual',0)",
            [],
        )
        .expect("column");
        conn.execute(
            "INSERT INTO cards(id,board_id,column_id,position,title,description,harness,
             status,created_at,updated_at)
             VALUES(7,1,1,0,'legacy title','legacy description','pi','idle',
                    '2025-01-02 03:04:05','2025-01-02 03:04:06')",
            [],
        )
        .expect("card");
        conn.execute(
            "INSERT INTO comments(id,card_id,author,body,created_at)
             VALUES(11,7,'agent:19','legacy comment','2025-01-02 03:04:07')",
            [],
        )
        .expect("comment");
        conn.execute(
            "INSERT INTO runs(id,card_id,column_id,harness,argv_json,prompt_snapshot,
             started_at,ended_at,outcome,result_summary)
             VALUES(13,7,1,'pi','[\"pi\"]','legacy prompt',
                    '2025-01-02 03:04:08','2025-01-02 03:05:08','ok','legacy result')",
            [],
        )
        .expect("run");
        conn.execute_batch("PRAGMA user_version = 12;")
            .expect("v12 marker");
    }

    let db = Db::open(&path).expect("v12 -> v14 migration");
    assert_eq!(db.user_version().expect("schema version"), 14);
    // The legacy Global board becomes the Global project's first board `main`.
    let board = db.get_board(1).expect("board");
    assert_eq!(board.name, "main");
    assert_eq!(board.project_id, 1);
    assert_eq!(board.scope_path, None);
    let global = db.get_project(1).expect("global project");
    assert_eq!(global.name, "Global");
    assert_eq!(global.scope_path, None);
    assert_eq!(
        db.get_card(7)
            .expect("card")
            .expect("card exists")
            .description,
        "legacy description"
    );
    assert_eq!(
        db.get_comment(11)
            .expect("comment")
            .expect("comment exists")
            .body,
        "legacy comment"
    );
    assert_eq!(
        db.list_runs(7).expect("runs")[0].prompt_snapshot,
        "legacy prompt"
    );

    // A legacy row must participate in the new audit path without changing
    // its owner or bytes when the first edit occurs after migration.
    db.update_comment(11, "edited after migration")
        .expect("edit migrated comment");
    let history = db
        .list_comment_history(11)
        .expect("migrated comment history");
    assert!(history
        .iter()
        .any(|entry| { entry.author == "agent:19" && entry.body == "legacy comment" }));
}
