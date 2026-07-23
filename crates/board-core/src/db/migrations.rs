use rusqlite::{params, OptionalExtension};

use super::{Db, BOARD_ID};
use crate::{Error, Result};

/// Embedded current schema (repo-root `schema.sql`, kept at the latest shape =
/// [`SCHEMA_VERSION`]). A fresh DB is created straight from this.
const SCHEMA_SQL: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../schema.sql"));

/// The latest schema version embedded in [`SCHEMA_SQL`].
const SCHEMA_VERSION: i64 = 11;

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

/// v8 → v9: persist timeout accounting. Legacy running runs derive their
/// deadline once from their original start; awaiting runs are paused at the
/// card's last durable transition time.
const V9_MIGRATION_SQL: &str = "
UPDATE runs
SET timeout_deadline_at_ms = CAST(unixepoch(started_at) * 1000 AS INTEGER)
    + (SELECT MAX(columns.timeout_minutes, 0) * 60000 FROM columns WHERE columns.id=runs.column_id)
WHERE started_at IS NOT NULL AND ended_at IS NULL
  AND (SELECT timeout_minutes FROM columns WHERE columns.id=runs.column_id) IS NOT NULL;
UPDATE runs
SET timeout_paused_at_ms = (SELECT CAST(unixepoch(cards.updated_at) * 1000 AS INTEGER)
                            FROM cards WHERE cards.id=runs.card_id)
WHERE ended_at IS NULL AND EXISTS
  (SELECT 1 FROM cards WHERE cards.id=runs.card_id AND cards.status='awaiting');
";

/// v9 → v10: cover the two scheduler queue scans without indexing history.
const V10_QUEUED_INDEX_SQL: &str =
    "CREATE INDEX idx_runs_queued_fifo ON runs(id) WHERE started_at IS NULL AND ended_at IS NULL";
const V10_ACTIVE_INDEX_SQL: &str =
    "CREATE INDEX idx_runs_active_open ON runs(id) WHERE started_at IS NOT NULL AND ended_at IS NULL";
const V11_MIGRATION_SQL: &str = "ALTER TABLE runs ADD COLUMN launch_spec_json TEXT";

impl Db {
    /// Apply migrations gated on `PRAGMA user_version`. Idempotent.
    ///
    /// - A fresh DB (`version 0`) is built straight from [`SCHEMA_SQL`] (the
    ///   current shape) plus the seed (`board id=1 'Global'`, column `Todo` manual
    ///   position 0) and stamped [`SCHEMA_VERSION`] — no per-version replay.
    /// - Existing DBs replay the required migrations in order.
    pub(super) fn migrate(&self) -> Result<()> {
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
            if version < 9 {
                let has_deadline: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('runs') WHERE name='timeout_deadline_at_ms')",
                    [], |r| r.get(0),
                )?;
                if !has_deadline {
                    tx.execute_batch("ALTER TABLE runs ADD COLUMN timeout_deadline_at_ms INTEGER")?;
                }
                let has_paused: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('runs') WHERE name='timeout_paused_at_ms')",
                    [], |r| r.get(0),
                )?;
                if !has_paused {
                    tx.execute_batch("ALTER TABLE runs ADD COLUMN timeout_paused_at_ms INTEGER")?;
                }
                tx.execute_batch(V9_MIGRATION_SQL)?;
            }
            if version < 10 {
                for (name, expected) in [
                    ("idx_runs_queued_fifo", V10_QUEUED_INDEX_SQL),
                    ("idx_runs_active_open", V10_ACTIVE_INDEX_SQL),
                ] {
                    let existing: Option<String> = tx
                        .query_row(
                            "SELECT sql FROM sqlite_master WHERE type='index' AND name=?1",
                            params![name],
                            |row| row.get(0),
                        )
                        .optional()?;
                    match existing {
                        None => tx.execute_batch(expected)?,
                        Some(sql) if sql == expected => {}
                        Some(sql) => {
                            return Err(Error::InvalidState(format!(
                                "schema v10 migration blocked: {name} has unexpected SQL: {sql}"
                            )));
                        }
                    }
                }
            }
            if version < 11 {
                let has_spec: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('runs') WHERE name='launch_spec_json')",
                    [], |r| r.get(0),
                )?;
                if !has_spec {
                    tx.execute_batch(V11_MIGRATION_SQL)?;
                }
            }
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        }
        Ok(())
    }
}
