//! rusqlite store: migrations, CRUD, position management, and the queries the
//! daemon needs. `boardd` is the only writer; access is serialized upstream, so
//! this type is deliberately synchronous.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::model::{Board, Card, Column, Comment, Run};
use crate::protocol::{
    AwaitingReason, CardCreateParams, CardStatus, CardUpdateParams, ColumnCreateParams,
    ColumnUpdateParams, Effort, Patch, RunOutcome, SpaceKind, Trigger,
};
use crate::{Error, Result};

/// Embedded current schema (repo-root `schema.sql`, kept at the latest shape =
/// [`SCHEMA_VERSION`]). A fresh DB is created straight from this.
const SCHEMA_SQL: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../schema.sql"));

/// The latest schema version embedded in [`SCHEMA_SQL`].
const SCHEMA_VERSION: i64 = 8;

/// v1 → v2 migration. SQLite cannot alter a CHECK constraint or drop a column
/// in place, so `cards` is rebuilt. Legacy `space_kind` values `cwd`/`worktree`
/// are best-effort converted to `workspace` (keeping `space_ref` as-is — for a
/// former `cwd` the ref was a path, now interpreted as a workspace id/label; a
/// former `worktree` loses its `worktree_base`, worktrees no longer being a
/// board concept). New columns `cards.session`, `cards.space_cwd`, and
/// `runs.session` are added (NULL = daemon's default session); `worktree_base`
/// is dropped.
const V2_MIGRATION_SQL: &str = "
PRAGMA foreign_keys = OFF;
BEGIN;
CREATE TABLE cards_v2 (
  id              INTEGER PRIMARY KEY,
  board_id        INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
  column_id       INTEGER NOT NULL REFERENCES columns(id),
  position        INTEGER NOT NULL,
  title           TEXT NOT NULL,
  description     TEXT NOT NULL DEFAULT '',
  harness         TEXT NOT NULL DEFAULT 'claude',
  model           TEXT,
  effort          TEXT CHECK (effort IN (NULL,'low','medium','high','xhigh','max')),
  permission_mode TEXT,
  session         TEXT,
  space_kind      TEXT NOT NULL DEFAULT 'workspace'
                    CHECK (space_kind IN ('workspace','new_workspace')),
  space_ref       TEXT,
  space_cwd       TEXT,
  status          TEXT NOT NULL DEFAULT 'idle'
                    CHECK (status IN ('idle','queued','running','blocked','failed')),
  session_id      TEXT,
  created_at      TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
INSERT INTO cards_v2
  (id,board_id,column_id,position,title,description,harness,model,effort,
   permission_mode,session,space_kind,space_ref,space_cwd,status,session_id,
   created_at,updated_at)
  SELECT id,board_id,column_id,position,title,description,harness,model,effort,
    permission_mode, NULL,
    CASE space_kind WHEN 'cwd' THEN 'workspace' WHEN 'worktree' THEN 'workspace'
                    ELSE space_kind END,
    space_ref, NULL, status, session_id, created_at, updated_at
  FROM cards;
DROP TABLE cards;
ALTER TABLE cards_v2 RENAME TO cards;
CREATE INDEX idx_cards_column ON cards(column_id, position);
ALTER TABLE runs ADD COLUMN session TEXT;
COMMIT;
PRAGMA foreign_keys = ON;
";

/// v2 → v3 migration: archived cards remain in their column and preserve all
/// history; a NULL timestamp means the card is active.
const V3_MIGRATION_SQL: &str = "ALTER TABLE cards ADD COLUMN archived_at TEXT;";

/// v3 → v4 migration: admit Pi's `off`/`minimal` thinking values. Rebuilding is
/// required because SQLite cannot alter a CHECK constraint in place. Existing
/// rows (including their stored harness) are copied unchanged; only the default
/// for future direct SQL inserts becomes Pi.
const V4_MIGRATION_SQL: &str = "
PRAGMA foreign_keys = OFF;
BEGIN;
CREATE TABLE cards_v4 (
  id              INTEGER PRIMARY KEY,
  board_id        INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
  column_id       INTEGER NOT NULL REFERENCES columns(id),
  position        INTEGER NOT NULL,
  title           TEXT NOT NULL,
  description     TEXT NOT NULL DEFAULT '',
  harness         TEXT NOT NULL DEFAULT 'pi',
  model           TEXT,
  effort          TEXT CHECK (effort IN (NULL,'off','minimal','low','medium','high','xhigh','max')),
  permission_mode TEXT,
  session         TEXT,
  space_kind      TEXT NOT NULL DEFAULT 'workspace'
                    CHECK (space_kind IN ('workspace','new_workspace')),
  space_ref       TEXT,
  space_cwd       TEXT,
  status          TEXT NOT NULL DEFAULT 'idle'
                    CHECK (status IN ('idle','queued','running','blocked','failed')),
  session_id      TEXT,
  created_at      TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
  archived_at     TEXT
);
INSERT INTO cards_v4
  (id,board_id,column_id,position,title,description,harness,model,effort,
   permission_mode,session,space_kind,space_ref,space_cwd,status,session_id,
   created_at,updated_at,archived_at)
  SELECT id,board_id,column_id,position,title,description,harness,model,effort,
    permission_mode,session,space_kind,space_ref,space_cwd,status,session_id,
    created_at,updated_at,archived_at
  FROM cards;
DROP TABLE cards;
ALTER TABLE cards_v4 RENAME TO cards;
CREATE INDEX idx_cards_column ON cards(column_id, position);
COMMIT;
PRAGMA foreign_keys = ON;
";

/// v4 → v5 migration: preserve board id 1 and all related rows as Global,
/// while adding canonical filesystem scope identity for independent boards.
const V5_MIGRATION_SQL: &str = "
ALTER TABLE boards ADD COLUMN scope_path TEXT;
UPDATE boards SET name='Global' WHERE id=1;
CREATE UNIQUE INDEX idx_boards_scope_path ON boards(scope_path) WHERE scope_path IS NOT NULL;
";

/// v5 → v6 migration: admit the `awaiting`/`done` card statuses and add
/// `cards.awaiting_reason`. Rebuilding is required because SQLite cannot alter
/// a CHECK constraint in place (same pattern as v4). Existing rows are copied
/// unchanged with `awaiting_reason = NULL` — idle cards with a last `ok` run
/// are deliberately NOT backfilled to `done`.
const V6_MIGRATION_SQL: &str = "
PRAGMA foreign_keys = OFF;
BEGIN;
CREATE TABLE cards_v6 (
  id              INTEGER PRIMARY KEY,
  board_id        INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
  column_id       INTEGER NOT NULL REFERENCES columns(id),
  position        INTEGER NOT NULL,
  title           TEXT NOT NULL,
  description     TEXT NOT NULL DEFAULT '',
  harness         TEXT NOT NULL DEFAULT 'pi',
  model           TEXT,
  effort          TEXT CHECK (effort IN (NULL,'off','minimal','low','medium','high','xhigh','max')),
  permission_mode TEXT,
  session         TEXT,
  space_kind      TEXT NOT NULL DEFAULT 'workspace'
                    CHECK (space_kind IN ('workspace','new_workspace')),
  space_ref       TEXT,
  space_cwd       TEXT,
  status          TEXT NOT NULL DEFAULT 'idle'
                    CHECK (status IN ('idle','queued','running','blocked','failed','awaiting','done')),
  awaiting_reason TEXT,
  session_id      TEXT,
  created_at      TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
  archived_at     TEXT,
  CHECK (
    (status = 'awaiting' AND awaiting_reason IS NOT NULL
      AND awaiting_reason IN ('agent_done','idle_expired'))
    OR
    (status <> 'awaiting' AND awaiting_reason IS NULL)
  )
);
INSERT INTO cards_v6
  (id,board_id,column_id,position,title,description,harness,model,effort,
   permission_mode,session,space_kind,space_ref,space_cwd,status,awaiting_reason,
   session_id,created_at,updated_at,archived_at)
  SELECT id,board_id,column_id,position,title,description,harness,model,effort,
    permission_mode,session,space_kind,space_ref,space_cwd,status,NULL,
    session_id,created_at,updated_at,archived_at
  FROM cards;
DROP TABLE cards;
ALTER TABLE cards_v6 RENAME TO cards;
CREATE INDEX idx_cards_column ON cards(column_id, position);
COMMIT;
PRAGMA foreign_keys = ON;
";

/// v6 → v7 migration: snapshot the fully resolved system prompt for future
/// queued runs. Existing rows deliberately remain NULL so their persisted
/// all-in-one argv keeps its legacy launch semantics.
const V7_MIGRATION_SQL: &str = "ALTER TABLE runs ADD COLUMN system_prompt_snapshot TEXT;";

/// v7 → v8 migration: enforce the lifecycle invariant in SQLite, including
/// direct SQL writers. Duplicate open runs are ambiguous and are never guessed
/// away by migration.
const V8_MIGRATION_SQL: &str =
    "CREATE UNIQUE INDEX idx_runs_one_open_per_card ON runs(card_id) WHERE ended_at IS NULL";

/// Deterministic statement boundaries used by crash-atomicity tests. Hooks are
/// opt-in per [`Db`] instance and receive no row or effect DTO.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleFaultPoint {
    EnqueueAfterRunInsert,
    PromoteAfterRunUpdate,
    FinalizeAfterRunUpdate,
}

/// All values needed to insert one queued run. Prompt building and external I/O
/// happen before this value enters the transaction.
#[derive(Debug, Clone)]
pub struct EnqueueRun<'a> {
    pub card_id: i64,
    pub column_id: i64,
    pub harness: &'a str,
    pub argv_json: &'a str,
    pub prompt_snapshot: &'a str,
    pub system_prompt_snapshot: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub session: Option<&'a str>,
}

/// Optional next run inserted atomically with finalization (an auto hop).
pub struct FinalizeRun<'a> {
    pub run_id: i64,
    pub outcome: RunOutcome,
    pub summary: Option<&'a str>,
    pub comment: Option<(&'a str, &'a str)>,
    pub target_column_id: Option<i64>,
    pub final_status: CardStatus,
    pub final_awaiting_reason: Option<AwaitingReason>,
    pub next: Option<EnqueueRun<'a>>,
}

/// Durable values callers may use for effects only after commit.
#[derive(Debug, Clone)]
pub struct FinalizeEffects {
    pub card: Card,
    pub finished_run: Run,
    pub next_run: Option<Run>,
}

/// The preserved Global board id and legacy protocol default.
pub const BOARD_ID: i64 = 1;

/// SQLite-backed board store.
pub struct Db {
    conn: Connection,
    lifecycle_fault_hook: Option<Box<dyn Fn(LifecycleFaultPoint) -> Result<()> + Send + Sync>>,
}

fn conv_err(field: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid enum value in {field}"),
        )),
    )
}

impl Db {
    /// Open (or create) a file-backed db: makes parent dirs, enables WAL +
    /// foreign keys, and runs migrations.
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Db::init(conn, None)
    }

    /// Open a file DB with an opt-in lifecycle statement hook. This is exposed
    /// only for deterministic fault tests; production callers use [`Db::open`].
    #[doc(hidden)]
    pub fn open_with_lifecycle_fault_hook(
        path: &Path,
        hook: impl Fn(LifecycleFaultPoint) -> Result<()> + Send + Sync + 'static,
    ) -> Result<Db> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Db::init(conn, Some(Box::new(hook)))
    }

    /// Open an in-memory db (tests, `FakeBoardClient`).
    pub fn open_in_memory() -> Result<Db> {
        Db::init(Connection::open_in_memory()?, None)
    }

    fn init(
        conn: Connection,
        lifecycle_fault_hook: Option<Box<dyn Fn(LifecycleFaultPoint) -> Result<()> + Send + Sync>>,
    ) -> Result<Db> {
        conn.pragma_update(None, "foreign_keys", true)?;
        let db = Db {
            conn,
            lifecycle_fault_hook,
        };
        db.migrate()?;
        Ok(db)
    }

    fn lifecycle_fault(&self, point: LifecycleFaultPoint) -> Result<()> {
        if let Some(hook) = &self.lifecycle_fault_hook {
            hook(point)?;
        }
        Ok(())
    }

    /// Apply migrations gated on `PRAGMA user_version`. Idempotent.
    ///
    /// - A fresh DB (`version 0`) is built straight from [`SCHEMA_SQL`] (the
    ///   current shape) plus the seed (`board id=1 'Global'`, column `Todo` manual
    ///   position 0) and stamped [`SCHEMA_VERSION`] — no per-version replay.
    /// - Existing DBs replay the required migrations in order.
    fn migrate(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            // Fresh DB: schema.sql already reflects the latest shape.
            self.conn.execute_batch(SCHEMA_SQL)?;
            self.conn.execute(
                "INSERT INTO boards (id, name, scope_path) VALUES (?1, 'Global', NULL)",
                params![BOARD_ID],
            )?;
            self.conn.execute(
                "INSERT INTO columns (board_id, name, position, trigger, fresh_session)
                 VALUES (?1, 'Todo', 0, 'manual', 0)",
                params![BOARD_ID],
            )?;
            self.conn
                .execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        } else if version < SCHEMA_VERSION {
            // Check the v8 invariant before any earlier shape migration can
            // persist. Ambiguous lifecycle data must leave both shape and
            // user_version untouched.
            if version < 8 {
                let duplicates = {
                    let mut statement = self.conn.prepare(
                        "SELECT card_id, group_concat(id, ',')
                         FROM (SELECT card_id, id FROM runs WHERE ended_at IS NULL
                               ORDER BY card_id, id)
                         GROUP BY card_id HAVING count(*) > 1 ORDER BY card_id",
                    )?;
                    let rows = statement
                        .query_map([], |row| {
                            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    rows
                };
                if !duplicates.is_empty() {
                    let details = duplicates
                        .iter()
                        .map(|(card_id, run_ids)| format!("card {card_id} runs [{run_ids}]"))
                        .collect::<Vec<_>>()
                        .join("; ");
                    return Err(Error::InvalidState(format!(
                        "schema v8 migration blocked by duplicate open runs: {details}"
                    )));
                }
            }
            if version < 2 {
                // Existing v1 DB: upgrade the space model in place.
                self.conn.execute_batch(V2_MIGRATION_SQL)?;
            }
            if version < 3 {
                self.conn.execute_batch(V3_MIGRATION_SQL)?;
            }
            if version < 4 {
                self.conn.execute_batch(V4_MIGRATION_SQL)?;
            }
            if version < 5 {
                self.conn.execute_batch(V5_MIGRATION_SQL)?;
            }
            if version < 6 {
                self.conn.execute_batch(V6_MIGRATION_SQL)?;
            }
            let tx = self.conn.unchecked_transaction()?;
            if version < 7 {
                let has_snapshot: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('runs')
                                   WHERE name='system_prompt_snapshot')",
                    [],
                    |row| row.get(0),
                )?;
                if !has_snapshot {
                    tx.execute_batch(V7_MIGRATION_SQL)?;
                }
            }
            if version < 8 {
                let existing_index_sql: Option<String> = tx
                    .query_row(
                        "SELECT sql FROM sqlite_master
                         WHERE type='index' AND name='idx_runs_one_open_per_card'",
                        [],
                        |row| row.get(0),
                    )
                    .optional()?;
                match existing_index_sql {
                    None => tx.execute_batch(V8_MIGRATION_SQL)?,
                    Some(sql) if sql == V8_MIGRATION_SQL => {}
                    Some(sql) => {
                        return Err(Error::InvalidState(format!(
                            "schema v8 migration blocked: idx_runs_one_open_per_card has unexpected SQL: {sql}"
                        )));
                    }
                }
            }
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// The current schema version.
    pub fn user_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    // -- board ---------------------------------------------------------------

    pub fn get_board(&self, id: i64) -> Result<Board> {
        self.conn
            .query_row(
                "SELECT id, name, scope_path FROM boards WHERE id=?1",
                params![id],
                row_to_board,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("board {id}")),
                other => Error::Sqlite(other),
            })
    }

    pub fn list_boards(&self) -> Result<Vec<Board>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, scope_path FROM boards
             ORDER BY CASE WHEN scope_path IS NULL THEN 0 ELSE 1 END, scope_path, id",
        )?;
        let rows = stmt
            .query_map([], row_to_board)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Get or create the independent board for an already-canonical scope path.
    /// New boards contain exactly one manual `Todo` column.
    pub fn open_board(&self, scope_path: &str) -> Result<Board> {
        if scope_path.trim().is_empty() {
            return Err(Error::BadRequest("scope_path must not be empty".into()));
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO boards(name,scope_path) VALUES(?1,?1)",
            params![scope_path],
        )?;
        let board = tx.query_row(
            "SELECT id,name,scope_path FROM boards WHERE scope_path=?1",
            params![scope_path],
            row_to_board,
        )?;
        tx.execute(
            "INSERT INTO columns(board_id,name,position,trigger,fresh_session)
             SELECT ?1,'Todo',0,'manual',0
             WHERE NOT EXISTS(SELECT 1 FROM columns WHERE board_id=?1)",
            params![board.id],
        )?;
        tx.commit()?;
        Ok(board)
    }

    // -- columns -------------------------------------------------------------

    pub fn list_columns(&self, board_id: i64) -> Result<Vec<Column>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM columns WHERE board_id=?1 ORDER BY position, id")?;
        let rows = stmt
            .query_map(params![board_id], row_to_column)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_column(&self, id: i64) -> Result<Option<Column>> {
        opt(self.conn.query_row(
            "SELECT * FROM columns WHERE id=?1",
            params![id],
            row_to_column,
        ))
    }

    fn require_column(&self, id: i64) -> Result<Column> {
        self.get_column(id)?
            .ok_or_else(|| Error::NotFound(format!("column {id}")))
    }

    /// The default (first) column of a board — the seed `Todo`.
    pub fn default_column_id(&self, board_id: i64) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT id FROM columns WHERE board_id=?1 ORDER BY position, id LIMIT 1",
                params![board_id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Error::NotFound("no columns".into()),
                other => Error::Sqlite(other),
            })
    }

    pub fn create_column(&self, p: &ColumnCreateParams) -> Result<Column> {
        let board_id = p.board_id.unwrap_or(BOARD_ID);
        self.get_board(board_id)?;
        self.validate_column_targets(board_id, p.on_success_column_id, p.on_fail_column_id)?;
        let end: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position)+1, 0) FROM columns WHERE board_id=?1",
            params![board_id],
            |r| r.get(0),
        )?;
        let trigger = p.trigger.unwrap_or(Trigger::Manual).as_str();
        let fresh = i64::from(p.fresh_session.unwrap_or(false));
        self.conn.execute(
            "INSERT INTO columns
             (board_id,name,position,system_prompt,trigger,on_success_column_id,on_fail_column_id,
              fresh_session,harness_override,model_override,effort_override,permission_override,timeout_minutes)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                board_id,
                p.name,
                end,
                p.system_prompt,
                trigger,
                p.on_success_column_id,
                p.on_fail_column_id,
                fresh,
                p.harness_override,
                p.model_override,
                p.effort_override,
                p.permission_override,
                p.timeout_minutes,
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        if let Some(pos) = p.position {
            self.reorder_column(id, pos)?;
        }
        self.require_column(id)
    }

    pub fn update_column(&self, p: &ColumnUpdateParams) -> Result<Column> {
        let mut c = self.require_column(p.id)?;
        if let Some(v) = &p.name {
            c.name = v.clone();
        }
        match &p.system_prompt {
            Patch::Unchanged => {}
            Patch::Clear => c.system_prompt = None,
            Patch::Set(v) => c.system_prompt = Some(v.clone()),
        }
        if let Some(v) = p.trigger {
            c.trigger = v;
        }
        match p.on_success_column_id {
            Patch::Unchanged => {}
            Patch::Clear => c.on_success_column_id = None,
            Patch::Set(v) => c.on_success_column_id = Some(v),
        }
        match p.on_fail_column_id {
            Patch::Unchanged => {}
            Patch::Clear => c.on_fail_column_id = None,
            Patch::Set(v) => c.on_fail_column_id = Some(v),
        }
        if let Some(v) = p.fresh_session {
            c.fresh_session = v;
        }
        match &p.harness_override {
            Patch::Unchanged => {}
            Patch::Clear => c.harness_override = None,
            Patch::Set(v) => c.harness_override = Some(v.clone()),
        }
        match &p.model_override {
            Patch::Unchanged => {}
            Patch::Clear => c.model_override = None,
            Patch::Set(v) => c.model_override = Some(v.clone()),
        }
        match &p.effort_override {
            Patch::Unchanged => {}
            Patch::Clear => c.effort_override = None,
            Patch::Set(v) => c.effort_override = Some(v.clone()),
        }
        match &p.permission_override {
            Patch::Unchanged => {}
            Patch::Clear => c.permission_override = None,
            Patch::Set(v) => c.permission_override = Some(v.clone()),
        }
        match p.timeout_minutes {
            Patch::Unchanged => {}
            Patch::Clear => c.timeout_minutes = None,
            Patch::Set(v) => c.timeout_minutes = Some(v),
        }
        self.validate_column_targets(c.board_id, c.on_success_column_id, c.on_fail_column_id)?;
        self.conn.execute(
            "UPDATE columns SET name=?1,system_prompt=?2,trigger=?3,on_success_column_id=?4,
             on_fail_column_id=?5,fresh_session=?6,harness_override=?7,model_override=?8,
             effort_override=?9,permission_override=?10,timeout_minutes=?11 WHERE id=?12",
            params![
                c.name,
                c.system_prompt,
                c.trigger.as_str(),
                c.on_success_column_id,
                c.on_fail_column_id,
                i64::from(c.fresh_session),
                c.harness_override,
                c.model_override,
                c.effort_override,
                c.permission_override,
                c.timeout_minutes,
                c.id,
            ],
        )?;
        if let Some(pos) = p.position {
            self.reorder_column(c.id, pos)?;
        }
        self.require_column(c.id)
    }

    fn validate_column_targets(
        &self,
        board_id: i64,
        on_success: Option<i64>,
        on_fail: Option<i64>,
    ) -> Result<()> {
        for target in [on_success, on_fail].into_iter().flatten() {
            let column = self.require_column(target)?;
            if column.board_id != board_id {
                return Err(Error::InvalidState(format!(
                    "column {target} belongs to board {}, expected {board_id}",
                    column.board_id
                )));
            }
        }
        Ok(())
    }

    /// Move a column to `position` and compact the whole board's ordering.
    pub fn reorder_column(&self, id: i64, position: i64) -> Result<Vec<Column>> {
        let board_id = self.require_column(id)?.board_id;
        let mut ids: Vec<i64> = self
            .conn
            .prepare("SELECT id FROM columns WHERE board_id=?1 AND id<>?2 ORDER BY position, id")?
            .query_map(params![board_id, id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let idx = (position.max(0) as usize).min(ids.len());
        ids.insert(idx, id);
        for (i, cid) in ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE columns SET position=?1 WHERE id=?2",
                params![i as i64, cid],
            )?;
        }
        self.list_columns(board_id)
    }

    /// Delete a column, optionally moving its cards to `move_cards_to` first.
    /// Callers should validate with the engine beforehand.
    pub fn delete_column(&self, id: i64, move_cards_to: Option<i64>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let board_id = self.require_column(id)?.board_id;
        if let Some(dst) = move_cards_to {
            let destination = self.require_column(dst)?;
            if destination.board_id != board_id {
                return Err(Error::InvalidState(format!(
                    "destination column {dst} belongs to another board"
                )));
            }
            let card_ids: Vec<i64> = self
                .conn
                .prepare("SELECT id FROM cards WHERE column_id=?1 ORDER BY position, id")?
                .query_map(params![id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for cid in card_ids {
                self.move_card(cid, dst, None)?;
            }
        }
        self.conn
            .execute("DELETE FROM columns WHERE id=?1", params![id])?;
        // Compact remaining columns.
        let ids: Vec<i64> = self
            .conn
            .prepare("SELECT id FROM columns WHERE board_id=?1 ORDER BY position, id")?
            .query_map(params![board_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for (i, cid) in ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE columns SET position=?1 WHERE id=?2",
                params![i as i64, cid],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // -- cards ---------------------------------------------------------------

    pub fn list_cards(&self, board_id: i64) -> Result<Vec<Card>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.* FROM cards c JOIN columns col ON col.id=c.column_id
             WHERE c.board_id=?1 ORDER BY col.position, c.position, c.id",
        )?;
        let rows = stmt
            .query_map(params![board_id], row_to_card)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_all_cards(&self) -> Result<Vec<Card>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.* FROM cards c
             JOIN boards b ON b.id=c.board_id
             JOIN columns col ON col.id=c.column_id
             ORDER BY CASE WHEN b.scope_path IS NULL THEN 0 ELSE 1 END,
                      b.scope_path, col.position, c.position, c.id",
        )?;
        let rows = stmt
            .query_map([], row_to_card)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_cards_in_column(&self, column_id: i64) -> Result<Vec<Card>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM cards WHERE column_id=?1 ORDER BY position, id")?;
        let rows = stmt
            .query_map(params![column_id], row_to_card)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_card(&self, id: i64) -> Result<Option<Card>> {
        opt(self
            .conn
            .query_row("SELECT * FROM cards WHERE id=?1", params![id], row_to_card))
    }

    fn require_card(&self, id: i64) -> Result<Card> {
        self.get_card(id)?
            .ok_or_else(|| Error::NotFound(format!("card {id}")))
    }

    pub fn create_card(&self, p: &CardCreateParams) -> Result<Card> {
        let board_id = p.board_id.unwrap_or(BOARD_ID);
        self.get_board(board_id)?;
        let column_id = match p.column_id {
            Some(c) => c,
            None => self.default_column_id(board_id)?,
        };
        let column = self.require_column(column_id)?;
        if column.board_id != board_id {
            return Err(Error::InvalidState(format!(
                "column {column_id} belongs to board {}, expected {board_id}",
                column.board_id
            )));
        }
        let end: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position)+1, 0) FROM cards WHERE column_id=?1",
            params![column_id],
            |r| r.get(0),
        )?;
        let description = p.description.clone().unwrap_or_default();
        let harness = p
            .harness
            .clone()
            .unwrap_or_else(|| crate::harness::DEFAULT_HARNESS.to_string());
        let space_kind = p.space_kind.unwrap_or(SpaceKind::Workspace).as_str();
        let effort = p.effort.map(|e| e.as_str());
        self.conn.execute(
            "INSERT INTO cards
             (board_id,column_id,position,title,description,harness,model,effort,permission_mode,
              session,space_kind,space_ref,space_cwd,status,session_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'idle',NULL)",
            params![
                board_id,
                column_id,
                end,
                p.title,
                description,
                harness,
                p.model,
                effort,
                p.permission_mode,
                p.session,
                space_kind,
                p.space_ref,
                p.space_cwd,
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        if let Some(pos) = p.position {
            self.move_card(id, column_id, Some(pos))?;
        }
        self.require_card(id)
    }

    pub fn update_card(&self, p: &CardUpdateParams) -> Result<Card> {
        let mut c = self.require_card(p.id)?;
        if let Some(v) = &p.title {
            c.title = v.clone();
        }
        if let Some(v) = &p.description {
            c.description = v.clone();
        }
        if let Some(v) = &p.harness {
            c.harness = v.clone();
            if v == "pi" {
                c.permission_mode = None;
            } else if v == "claude" && matches!(c.effort, Some(Effort::Off | Effort::Minimal)) {
                c.effort = None;
            }
        }
        match &p.model {
            Patch::Unchanged => {}
            Patch::Clear => c.model = None,
            Patch::Set(v) => c.model = Some(v.clone()),
        }
        match p.effort {
            Patch::Unchanged => {}
            Patch::Clear => c.effort = None,
            Patch::Set(v) => c.effort = Some(v),
        }
        match &p.permission_mode {
            Patch::Unchanged => {}
            Patch::Clear => c.permission_mode = None,
            Patch::Set(v) => c.permission_mode = Some(v.clone()),
        }
        match &p.session {
            Patch::Unchanged => {}
            Patch::Clear => c.session = None,
            Patch::Set(v) => c.session = Some(v.clone()),
        }
        if let Some(v) = p.space_kind {
            c.space_kind = v;
        }
        match &p.space_ref {
            Patch::Unchanged => {}
            Patch::Clear => c.space_ref = None,
            Patch::Set(v) => c.space_ref = Some(v.clone()),
        }
        match &p.space_cwd {
            Patch::Unchanged => {}
            Patch::Clear => c.space_cwd = None,
            Patch::Set(v) => c.space_cwd = Some(v.clone()),
        }
        self.conn.execute(
            "UPDATE cards SET title=?1,description=?2,harness=?3,model=?4,effort=?5,
             permission_mode=?6,session=?7,space_kind=?8,space_ref=?9,space_cwd=?10,
             updated_at=datetime('now') WHERE id=?11",
            params![
                c.title,
                c.description,
                c.harness,
                c.model,
                c.effort.map(|e| e.as_str()),
                c.permission_mode,
                c.session,
                c.space_kind.as_str(),
                c.space_ref,
                c.space_cwd,
                c.id,
            ],
        )?;
        self.require_card(c.id)
    }

    pub fn set_card_archived(&self, id: i64, archived: bool) -> Result<Card> {
        self.require_card(id)?;
        if archived {
            self.conn.execute(
                "UPDATE cards SET archived_at=datetime('now'), updated_at=datetime('now') WHERE id=?1",
                params![id],
            )?;
        } else {
            self.conn.execute(
                "UPDATE cards SET archived_at=NULL, updated_at=datetime('now') WHERE id=?1",
                params![id],
            )?;
        }
        self.require_card(id)
    }

    pub fn delete_card(&self, id: i64) -> Result<()> {
        let card = self.require_card(id)?;
        self.conn
            .execute("DELETE FROM cards WHERE id=?1", params![id])?;
        self.renumber_column_cards(card.column_id)?;
        Ok(())
    }

    /// Move a card to `target_column` at `position` (append if `None`), compacting
    /// both the source and target columns.
    pub fn move_card(&self, id: i64, target_column: i64, position: Option<i64>) -> Result<Card> {
        let card = self.require_card(id)?;
        let target = self.require_column(target_column)?;
        if target.board_id != card.board_id {
            return Err(Error::InvalidState(format!(
                "column {target_column} belongs to board {}, card {id} belongs to board {}",
                target.board_id, card.board_id
            )));
        }
        let old_column = card.column_id;
        self.conn.execute(
            "UPDATE cards SET column_id=?1, updated_at=datetime('now') WHERE id=?2",
            params![target_column, id],
        )?;
        // Place within the target column.
        let mut ids: Vec<i64> = self
            .conn
            .prepare("SELECT id FROM cards WHERE column_id=?1 AND id<>?2 ORDER BY position, id")?
            .query_map(params![target_column, id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let idx = position
            .map(|p| p.max(0) as usize)
            .unwrap_or(ids.len())
            .min(ids.len());
        ids.insert(idx, id);
        for (i, cid) in ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE cards SET position=?1 WHERE id=?2",
                params![i as i64, cid],
            )?;
        }
        if old_column != target_column {
            self.renumber_column_cards(old_column)?;
        }
        self.require_card(id)
    }

    fn renumber_column_cards(&self, column_id: i64) -> Result<()> {
        let ids: Vec<i64> = self
            .conn
            .prepare("SELECT id FROM cards WHERE column_id=?1 ORDER BY position, id")?
            .query_map(params![column_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for (i, cid) in ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE cards SET position=?1 WHERE id=?2",
                params![i as i64, cid],
            )?;
        }
        Ok(())
    }

    /// Set the card's status. Any status other than `awaiting` clears
    /// `awaiting_reason` (the reason is only meaningful while awaiting);
    /// use [`Db::set_card_awaiting`] to enter `awaiting` with a reason.
    pub fn set_card_status(&self, id: i64, status: CardStatus) -> Result<Card> {
        if status == CardStatus::Awaiting {
            return Err(Error::InvalidState(
                "enter awaiting with Db::set_card_awaiting so a reason is recorded".into(),
            ));
        }
        self.conn.execute(
            "UPDATE cards SET status=?1, awaiting_reason=NULL, updated_at=datetime('now')
             WHERE id=?2",
            params![status.as_str(), id],
        )?;
        self.require_card(id)
    }

    /// Enter (or re-enter, refreshing the reason) `awaiting` with `reason`.
    /// The active run stays open; the column timeout is paused upstream.
    pub fn set_card_awaiting(&self, id: i64, reason: AwaitingReason) -> Result<Card> {
        self.conn.execute(
            "UPDATE cards SET status='awaiting', awaiting_reason=?1, updated_at=datetime('now')
             WHERE id=?2",
            params![reason.as_str(), id],
        )?;
        self.require_card(id)
    }

    pub fn set_card_column(&self, id: i64, column_id: i64) -> Result<Card> {
        self.move_card(id, column_id, None)
    }

    pub fn set_card_session(&self, id: i64, session_id: &str) -> Result<Card> {
        self.conn.execute(
            "UPDATE cards SET session_id=?1, updated_at=datetime('now') WHERE id=?2",
            params![session_id, id],
        )?;
        self.require_card(id)
    }

    // -- comments ------------------------------------------------------------

    pub fn add_comment(&self, card_id: i64, author: &str, body: &str) -> Result<Comment> {
        self.require_card(card_id)?;
        self.conn.execute(
            "INSERT INTO comments (card_id, author, body) VALUES (?1, ?2, ?3)",
            params![card_id, author, body],
        )?;
        let id = self.conn.last_insert_rowid();
        opt(self.conn.query_row(
            "SELECT * FROM comments WHERE id=?1",
            params![id],
            row_to_comment,
        ))?
        .ok_or_else(|| Error::NotFound(format!("comment {id}")))
    }

    pub fn list_comments(&self, card_id: i64) -> Result<Vec<Comment>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM comments WHERE card_id=?1 ORDER BY created_at, id")?;
        let rows = stmt
            .query_map(params![card_id], row_to_comment)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // -- runs ----------------------------------------------------------------

    /// Create a queued legacy-compatible run (`started_at`/`outcome` NULL).
    ///
    /// The absent system snapshot intentionally gives direct callers the same
    /// shape as a pre-v7 row. Production enqueue uses
    /// [`Db::create_run_with_prompt_snapshots`] instead.
    #[allow(clippy::too_many_arguments)]
    pub fn create_run(
        &self,
        card_id: i64,
        column_id: i64,
        harness: &str,
        argv_json: &str,
        prompt_snapshot: &str,
        session_id: Option<&str>,
        session: Option<&str>,
    ) -> Result<Run> {
        self.create_run_with_prompt_snapshots(
            card_id,
            column_id,
            harness,
            argv_json,
            prompt_snapshot,
            None,
            session_id,
            session,
        )
    }

    /// Create a queued run with both enqueue-time prompt channels persisted in
    /// the same row. `system_prompt_snapshot` is already protocol-trailer
    /// inclusive and is stored byte-for-byte.
    #[allow(clippy::too_many_arguments)]
    pub fn create_run_with_prompt_snapshots(
        &self,
        card_id: i64,
        column_id: i64,
        harness: &str,
        argv_json: &str,
        prompt_snapshot: &str,
        system_prompt_snapshot: Option<&str>,
        session_id: Option<&str>,
        session: Option<&str>,
    ) -> Result<Run> {
        let card = self.require_card(card_id)?;
        let column = self.require_column(column_id)?;
        if column.board_id != card.board_id {
            return Err(Error::InvalidState(format!(
                "run column {column_id} belongs to another board than card {card_id}"
            )));
        }
        self.conn.execute(
            "INSERT INTO runs
             (card_id,column_id,harness,argv_json,prompt_snapshot,system_prompt_snapshot,
              session_id,session)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                card_id,
                column_id,
                harness,
                argv_json,
                prompt_snapshot,
                system_prompt_snapshot,
                session_id,
                session
            ],
        )?;
        self.get_run(self.conn.last_insert_rowid())
    }

    /// Atomically insert a queued run and publish the card's queued state.
    /// No process, socket, notification, or other external I/O occurs here.
    pub fn enqueue_run_uow(&self, p: &EnqueueRun<'_>) -> Result<Run> {
        let card = self.require_card(p.card_id)?;
        let column = self.require_column(p.column_id)?;
        if card.board_id != column.board_id {
            return Err(Error::InvalidState(
                "run column belongs to another board".into(),
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO runs
             (card_id,column_id,harness,argv_json,prompt_snapshot,system_prompt_snapshot,session_id,session)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![p.card_id,p.column_id,p.harness,p.argv_json,p.prompt_snapshot,
                    p.system_prompt_snapshot,p.session_id,p.session],
        )?;
        let id = tx.last_insert_rowid();
        self.lifecycle_fault(LifecycleFaultPoint::EnqueueAfterRunInsert)?;
        let changed = tx.execute(
            "UPDATE cards SET status='queued',awaiting_reason=NULL,session_id=COALESCE(?2,session_id),updated_at=datetime('now') WHERE id=?1",
            params![p.card_id, p.session_id],
        )?;
        if changed != 1 {
            return Err(Error::NotFound(format!("card {}", p.card_id)));
        }
        tx.commit()?;
        self.get_run(id)
    }

    /// Atomically promote an exact queued run and its card to running.
    pub fn promote_run_uow(
        &self,
        run_id: i64,
        workspace_id: Option<&str>,
        pane_id: Option<&str>,
    ) -> Result<Run> {
        let tx = self.conn.unchecked_transaction()?;
        let card_id: i64 = tx
            .query_row(
                "SELECT card_id FROM runs WHERE id=?1 AND started_at IS NULL AND ended_at IS NULL",
                params![run_id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::InvalidState(format!("run {run_id} is not queued"))
                }
                other => Error::Sqlite(other),
            })?;
        tx.execute(
            "UPDATE runs SET started_at=datetime('now'),herdr_workspace_id=?1,herdr_pane_id=?2 WHERE id=?3",
            params![workspace_id,pane_id,run_id],
        )?;
        self.lifecycle_fault(LifecycleFaultPoint::PromoteAfterRunUpdate)?;
        tx.execute(
            "UPDATE cards SET status='running',awaiting_reason=NULL,updated_at=datetime('now') WHERE id=?1",
            params![card_id],
        )?;
        tx.commit()?;
        self.get_run(run_id)
    }

    /// Atomically close a run, append its optional durable comment, transition
    /// the card, and optionally enqueue the already-planned next auto-hop.
    pub fn finalize_run_uow(&self, p: &FinalizeRun<'_>) -> Result<FinalizeEffects> {
        let tx = self.conn.unchecked_transaction()?;
        let card_id: i64 = tx
            .query_row(
                "SELECT card_id FROM runs WHERE id=?1 AND ended_at IS NULL",
                params![p.run_id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::InvalidState(format!("run {} is not open", p.run_id))
                }
                other => Error::Sqlite(other),
            })?;
        if let Some(next) = &p.next {
            if next.card_id != card_id {
                return Err(Error::InvalidState(
                    "next run belongs to another card".into(),
                ));
            }
            let board_matches: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM cards c JOIN columns col ON col.id=?1
                               WHERE c.id=?2 AND c.board_id=col.board_id)",
                params![next.column_id, card_id],
                |row| row.get(0),
            )?;
            if !board_matches {
                return Err(Error::InvalidState(
                    "next run column belongs to another board".into(),
                ));
            }
        }
        tx.execute(
            "UPDATE runs SET ended_at=datetime('now'),outcome=?1,result_summary=?2 WHERE id=?3",
            params![p.outcome.as_str(), p.summary, p.run_id],
        )?;
        self.lifecycle_fault(LifecycleFaultPoint::FinalizeAfterRunUpdate)?;
        if let Some((author, body)) = p.comment {
            tx.execute(
                "INSERT INTO comments(card_id,author,body) VALUES(?1,?2,?3)",
                params![card_id, author, body],
            )?;
        }
        if let Some(column_id) = p.target_column_id {
            let board_matches: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM cards c JOIN columns col ON col.id=?1
                               WHERE c.id=?2 AND c.board_id=col.board_id)",
                params![column_id, card_id],
                |r| r.get(0),
            )?;
            if !board_matches {
                return Err(Error::InvalidState(
                    "target column belongs to another board".into(),
                ));
            }
            let position: i64 = tx.query_row(
                "SELECT COALESCE(MAX(position)+1,0) FROM cards WHERE column_id=?1",
                params![column_id],
                |r| r.get(0),
            )?;
            tx.execute(
                "UPDATE cards SET column_id=?1,position=?2 WHERE id=?3",
                params![column_id, position, card_id],
            )?;
        }
        tx.execute(
            "UPDATE cards SET status=?1,awaiting_reason=?2,updated_at=datetime('now') WHERE id=?3",
            params![
                p.final_status.as_str(),
                p.final_awaiting_reason.as_ref().map(AwaitingReason::as_str),
                card_id
            ],
        )?;
        let next_id = if let Some(next) = &p.next {
            tx.execute(
                "INSERT INTO runs(card_id,column_id,harness,argv_json,prompt_snapshot,
                 system_prompt_snapshot,session_id,session) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    next.card_id,
                    next.column_id,
                    next.harness,
                    next.argv_json,
                    next.prompt_snapshot,
                    next.system_prompt_snapshot,
                    next.session_id,
                    next.session
                ],
            )?;
            tx.execute(
                "UPDATE cards SET status='queued',awaiting_reason=NULL WHERE id=?1",
                params![card_id],
            )?;
            Some(tx.last_insert_rowid())
        } else {
            None
        };
        tx.commit()?;
        Ok(FinalizeEffects {
            card: self.require_card(card_id)?,
            finished_run: self.get_run(p.run_id)?,
            next_run: next_id.map(|id| self.get_run(id)).transpose()?,
        })
    }

    /// Mark a run started, recording herdr ids.
    pub fn start_run(
        &self,
        run_id: i64,
        workspace_id: Option<&str>,
        pane_id: Option<&str>,
    ) -> Result<Run> {
        self.conn.execute(
            "UPDATE runs SET started_at=datetime('now'), herdr_workspace_id=?1, herdr_pane_id=?2
             WHERE id=?3",
            params![workspace_id, pane_id, run_id],
        )?;
        self.get_run(run_id)
    }

    /// Close a run with an outcome + summary.
    pub fn finish_run(
        &self,
        run_id: i64,
        outcome: RunOutcome,
        summary: Option<&str>,
    ) -> Result<Run> {
        self.conn.execute(
            "UPDATE runs SET ended_at=datetime('now'), outcome=?1, result_summary=?2 WHERE id=?3",
            params![outcome.as_str(), summary, run_id],
        )?;
        self.get_run(run_id)
    }

    pub fn get_run(&self, id: i64) -> Result<Run> {
        opt(self
            .conn
            .query_row("SELECT * FROM runs WHERE id=?1", params![id], row_to_run))?
        .ok_or_else(|| Error::NotFound(format!("run {id}")))
    }

    pub fn list_runs(&self, card_id: i64) -> Result<Vec<Run>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM runs WHERE card_id=?1 ORDER BY id")?;
        let rows = stmt
            .query_map(params![card_id], row_to_run)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The card's open (queued or started, not ended) run, if any.
    pub fn open_run_for_card(&self, card_id: i64) -> Result<Option<Run>> {
        opt(self.conn.query_row(
            "SELECT * FROM runs WHERE card_id=?1 AND ended_at IS NULL
             ORDER BY id DESC LIMIT 1",
            params![card_id],
            row_to_run,
        ))
    }

    /// Whether any card currently in `column_id` has an open run.
    pub fn column_has_open_run(&self, column_id: i64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM runs r JOIN cards c ON c.id=r.card_id
               WHERE c.column_id=?1 AND r.ended_at IS NULL
             )",
            params![column_id],
            |row| row.get(0),
        )?)
    }

    /// The card's active (started, not ended) run, if any.
    pub fn active_run_for_card(&self, card_id: i64) -> Result<Option<Run>> {
        opt(self.conn.query_row(
            "SELECT * FROM runs WHERE card_id=?1 AND started_at IS NOT NULL AND ended_at IS NULL
             ORDER BY id DESC LIMIT 1",
            params![card_id],
            row_to_run,
        ))
    }

    /// Most recent run for the card that still records a target pane.
    pub fn latest_run_with_pane(&self, card_id: i64) -> Result<Option<Run>> {
        self.require_card(card_id)?;
        opt(self.conn.query_row(
            "SELECT * FROM runs WHERE card_id=?1 AND herdr_pane_id IS NOT NULL
             ORDER BY id DESC LIMIT 1",
            params![card_id],
            row_to_run,
        ))
    }

    /// Queued runs for a space key `(space_kind, space_ref)`, FIFO by run id.
    pub fn queued_runs_by_space(
        &self,
        space_kind: SpaceKind,
        space_ref: Option<&str>,
    ) -> Result<Vec<Run>> {
        let base = "SELECT r.* FROM runs r JOIN cards c ON c.id=r.card_id
                    WHERE r.started_at IS NULL AND r.ended_at IS NULL AND c.space_kind=?1";
        let rows = match space_ref {
            Some(sr) => {
                let sql = format!("{base} AND c.space_ref=?2 ORDER BY r.id");
                self.conn
                    .prepare(&sql)?
                    .query_map(params![space_kind.as_str(), sr], row_to_run)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => {
                let sql = format!("{base} AND c.space_ref IS NULL ORDER BY r.id");
                self.conn
                    .prepare(&sql)?
                    .query_map(params![space_kind.as_str()], row_to_run)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        Ok(rows)
    }

    pub fn count_active_runs(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM runs WHERE started_at IS NOT NULL AND ended_at IS NULL",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn count_queued_runs(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM runs WHERE started_at IS NULL AND ended_at IS NULL",
            [],
            |r| r.get(0),
        )?)
    }
}

// -- row mappers -------------------------------------------------------------

fn opt<T>(r: rusqlite::Result<T>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(Error::Sqlite(e)),
    }
}

fn row_to_board(row: &Row) -> rusqlite::Result<Board> {
    Ok(Board {
        id: row.get("id")?,
        name: row.get("name")?,
        scope_path: row.get("scope_path")?,
    })
}

fn row_to_column(row: &Row) -> rusqlite::Result<Column> {
    let trigger_s: String = row.get("trigger")?;
    let fresh: i64 = row.get("fresh_session")?;
    Ok(Column {
        id: row.get("id")?,
        board_id: row.get("board_id")?,
        name: row.get("name")?,
        position: row.get("position")?,
        system_prompt: row.get("system_prompt")?,
        trigger: Trigger::parse_str(&trigger_s).ok_or_else(|| conv_err("trigger"))?,
        on_success_column_id: row.get("on_success_column_id")?,
        on_fail_column_id: row.get("on_fail_column_id")?,
        fresh_session: fresh != 0,
        harness_override: row.get("harness_override")?,
        model_override: row.get("model_override")?,
        effort_override: row.get("effort_override")?,
        permission_override: row.get("permission_override")?,
        timeout_minutes: row.get("timeout_minutes")?,
    })
}

fn row_to_card(row: &Row) -> rusqlite::Result<Card> {
    let effort_s: Option<String> = row.get("effort")?;
    let effort = match effort_s {
        Some(s) => Some(Effort::parse_str(&s).ok_or_else(|| conv_err("effort"))?),
        None => None,
    };
    let space_s: String = row.get("space_kind")?;
    let status_s: String = row.get("status")?;
    let reason_s: Option<String> = row.get("awaiting_reason")?;
    let awaiting_reason = match reason_s {
        Some(s) => Some(AwaitingReason::parse_str(&s).ok_or_else(|| conv_err("awaiting_reason"))?),
        None => None,
    };
    Ok(Card {
        id: row.get("id")?,
        board_id: row.get("board_id")?,
        column_id: row.get("column_id")?,
        position: row.get("position")?,
        title: row.get("title")?,
        description: row.get("description")?,
        harness: row.get("harness")?,
        model: row.get("model")?,
        effort,
        permission_mode: row.get("permission_mode")?,
        session: row.get("session")?,
        space_kind: SpaceKind::parse_str(&space_s).ok_or_else(|| conv_err("space_kind"))?,
        space_ref: row.get("space_ref")?,
        space_cwd: row.get("space_cwd")?,
        status: CardStatus::parse_str(&status_s).ok_or_else(|| conv_err("status"))?,
        awaiting_reason,
        session_id: row.get("session_id")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        archived_at: row.get("archived_at")?,
    })
}

fn row_to_comment(row: &Row) -> rusqlite::Result<Comment> {
    Ok(Comment {
        id: row.get("id")?,
        card_id: row.get("card_id")?,
        author: row.get("author")?,
        body: row.get("body")?,
        created_at: row.get("created_at")?,
    })
}

fn row_to_run(row: &Row) -> rusqlite::Result<Run> {
    let outcome_s: Option<String> = row.get("outcome")?;
    let outcome = match outcome_s {
        Some(s) => Some(RunOutcome::parse_str(&s).ok_or_else(|| conv_err("outcome"))?),
        None => None,
    };
    Ok(Run {
        id: row.get("id")?,
        card_id: row.get("card_id")?,
        column_id: row.get("column_id")?,
        harness: row.get("harness")?,
        argv_json: row.get("argv_json")?,
        prompt_snapshot: row.get("prompt_snapshot")?,
        system_prompt_snapshot: row.get("system_prompt_snapshot")?,
        herdr_workspace_id: row.get("herdr_workspace_id")?,
        herdr_pane_id: row.get("herdr_pane_id")?,
        session_id: row.get("session_id")?,
        session: row.get("session")?,
        started_at: row.get("started_at")?,
        ended_at: row.get("ended_at")?,
        outcome,
        result_summary: row.get("result_summary")?,
        log_path: row.get("log_path")?,
    })
}
