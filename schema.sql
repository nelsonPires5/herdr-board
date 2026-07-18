-- herdr-board SQLite schema (WAL mode; boardd is the only writer).
-- This file is the CURRENT (schema v5) shape: a fresh DB is created directly
-- from it and stamped `PRAGMA user_version = 5`. Existing databases are upgraded
-- by migrations in board-core/src/db.rs (kept in sync with this file).
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE boards (
  id         INTEGER PRIMARY KEY,
  name       TEXT NOT NULL UNIQUE,
  scope_path TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- NULL scope identifies the preserved Global board. Canonical path identity is
-- unique for scoped boards while allowing exactly one legacy/global NULL row.
CREATE UNIQUE INDEX idx_boards_scope_path ON boards(scope_path) WHERE scope_path IS NOT NULL;

-- A fresh board gets exactly one seeded column: 'Todo' (trigger=manual).
-- Everything else (names, count, order, config) is user-created.
CREATE TABLE columns (
  id                   INTEGER PRIMARY KEY,
  board_id             INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
  name                 TEXT NOT NULL,
  position             INTEGER NOT NULL,
  system_prompt        TEXT,                -- prepended via --append-system-prompt
  trigger              TEXT NOT NULL DEFAULT 'manual'
                         CHECK (trigger IN ('manual','auto')),
  on_success_column_id INTEGER REFERENCES columns(id) ON DELETE SET NULL,
  on_fail_column_id    INTEGER REFERENCES columns(id) ON DELETE SET NULL,
  fresh_session        INTEGER NOT NULL DEFAULT 0,  -- 1 = never --resume in this column
  harness_override     TEXT,
  model_override       TEXT,
  effort_override      TEXT,
  permission_override  TEXT,
  timeout_minutes      INTEGER,
  UNIQUE (board_id, name)
);

CREATE TABLE cards (
  id              INTEGER PRIMARY KEY,
  board_id        INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
  column_id       INTEGER NOT NULL REFERENCES columns(id),
  position        INTEGER NOT NULL,
  title           TEXT NOT NULL,
  description     TEXT NOT NULL DEFAULT '',   -- the base prompt
  harness         TEXT NOT NULL DEFAULT 'pi',
  model           TEXT,
  effort          TEXT CHECK (effort IN (NULL,'off','minimal','low','medium','high','xhigh','max')),
  permission_mode TEXT,                        -- e.g. acceptEdits, plan; bypass = explicit opt-in
  session         TEXT,                        -- herdr session name; NULL = daemon's default session
  space_kind      TEXT NOT NULL DEFAULT 'workspace'
                    CHECK (space_kind IN ('workspace','new_workspace')),
  space_ref       TEXT,                        -- workspace id (workspace) | new-workspace label (new_workspace)
  space_cwd       TEXT,                        -- working dir when space_kind='new_workspace'
  status          TEXT NOT NULL DEFAULT 'idle'
                    CHECK (status IN ('idle','queued','running','blocked','failed')),
  session_id      TEXT,                        -- harness conversation id for --resume
  created_at      TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
  archived_at     TEXT                         -- NULL = active; timestamp = archived
);

CREATE TABLE comments (
  id         INTEGER PRIMARY KEY,
  card_id    INTEGER NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  author     TEXT NOT NULL,                    -- 'user' | 'agent:<run_id>' | 'system'
  body       TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE runs (
  id                 INTEGER PRIMARY KEY,
  card_id            INTEGER NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  column_id          INTEGER NOT NULL REFERENCES columns(id),
  harness            TEXT NOT NULL,
  argv_json          TEXT NOT NULL,
  prompt_snapshot    TEXT NOT NULL,
  herdr_workspace_id TEXT,
  herdr_pane_id      TEXT,
  session_id         TEXT,                     -- harness conversation id (--resume)
  session            TEXT,                     -- herdr session name; NULL = default session
  started_at         TEXT,
  ended_at           TEXT,
  outcome            TEXT CHECK (outcome IN (NULL,'ok','fail','cancelled','lost')),
  result_summary     TEXT,
  log_path           TEXT
);

CREATE INDEX idx_cards_column   ON cards(column_id, position);
CREATE INDEX idx_comments_card  ON comments(card_id, created_at);
CREATE INDEX idx_runs_card      ON runs(card_id, started_at);
