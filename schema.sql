-- herdr-board SQLite schema (WAL mode; boardd is the only writer).
-- This file is the CURRENT (schema v14) shape: a fresh DB is created directly
-- from it and stamped `PRAGMA user_version = 14`. Existing databases are upgraded
-- by migrations in board-core/src/db/migrations.rs (kept in sync with this file).
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- A Project is a named collection of boards, identified by a canonical
-- filesystem path (a Git root or a plain existing directory). The special
-- Global project (id=1, scope_path NULL) preserves the pre-v14 flat boards.
CREATE TABLE projects (
  id         INTEGER PRIMARY KEY,
  scope_path TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Canonical path identity is unique for scoped projects while allowing exactly
-- one special Global row (NULL).
CREATE UNIQUE INDEX idx_projects_scope_path ON projects(scope_path) WHERE scope_path IS NOT NULL;

-- Boards belong to exactly one project. Names are unique case-insensitively
-- within the same project (COLLATE NOCASE on the UNIQUE index); the same name
-- on a different project is legal. Every project's first board is named 'main'.
CREATE TABLE boards (
  id         INTEGER PRIMARY KEY,
  project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name       TEXT NOT NULL COLLATE NOCASE,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  UNIQUE (project_id, name)
);

-- Persistent context: the selected project (singleton row; absent = no
-- selection yet, e.g. right after migration).
CREATE TABLE selection (
  id         INTEGER PRIMARY KEY CHECK (id = 1),
  project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE,
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- The selected board per project (a project always has a selected board once
-- it has been used; otherwise its first board 'main' is the context board).
CREATE TABLE board_selection (
  project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  board_id   INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Recency (most recent first, capped at 3 per project / 3 projects).
CREATE TABLE project_recents (
  project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  rank       INTEGER NOT NULL,
  used_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE board_recents (
  project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  board_id   INTEGER NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
  rank       INTEGER NOT NULL,
  used_at    TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (project_id, board_id)
);

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
                    CHECK (status IN ('idle','queued','running','blocked','failed','awaiting','done')),
  awaiting_reason TEXT,                        -- 'agent_done'|'idle_expired'; set in awaiting, NULL otherwise
  session_id      TEXT,                        -- harness conversation id for --resume
  created_at      TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
  archived_at     TEXT,                        -- NULL = active; timestamp = archived
  CHECK (
    (status = 'awaiting' AND awaiting_reason IS NOT NULL
      AND awaiting_reason IN ('agent_done','idle_expired'))
    OR
    (status <> 'awaiting' AND awaiting_reason IS NULL)
  )
);

CREATE TABLE comments (
  id         INTEGER PRIMARY KEY,
  card_id    INTEGER NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  author     TEXT NOT NULL,                    -- 'user' | 'agent:<run_id>' | 'system'
  body       TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  deleted_at TEXT                              -- NULL = current; timestamp = soft-deleted
);

-- Immutable snapshots of every user-visible comment state.  The current row
-- remains in `comments` so its identity and ownership are stable; history is
-- retained after a soft delete for audit and authorization decisions.
CREATE TABLE comment_history (
  id         INTEGER PRIMARY KEY,
  comment_id INTEGER NOT NULL REFERENCES comments(id) ON DELETE CASCADE,
  card_id    INTEGER NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  author     TEXT NOT NULL,
  body       TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  deleted_at TEXT
);

CREATE INDEX idx_comment_history_comment ON comment_history(comment_id, id);

-- System and daemon-generated comments use the same insertion path as user
-- comments (including run finalization), so every durable comment gets its
-- initial audit snapshot without relying on one caller remembering to do so.
CREATE TRIGGER comments_audit_insert
AFTER INSERT ON comments
BEGIN
  INSERT INTO comment_history(comment_id, card_id, author, body, created_at, deleted_at)
  VALUES(NEW.id, NEW.card_id, NEW.author, NEW.body, NEW.created_at, NEW.deleted_at);
END;

CREATE TABLE runs (
  id                 INTEGER PRIMARY KEY,
  card_id            INTEGER NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
  column_id          INTEGER NOT NULL REFERENCES columns(id),
  harness            TEXT NOT NULL,
  argv_json          TEXT NOT NULL,
  prompt_snapshot    TEXT NOT NULL,
  system_prompt_snapshot TEXT,                    -- NULL marks a legacy pre-v7 launch
  launch_spec_json TEXT,                          -- NULL marks a pre-v11 launch
  herdr_workspace_id TEXT,
  herdr_pane_id      TEXT,
  herdr_anchor_pane_id TEXT,                 -- exact board-owned card-tab shell anchor
  session_id         TEXT,                     -- harness conversation id (--resume)
  session            TEXT,                     -- herdr session name; NULL = default session
  started_at         TEXT,
  timeout_deadline_at_ms INTEGER,             -- durable wall-clock deadline; NULL = unlimited
  timeout_paused_at_ms INTEGER,               -- awaiting began; NULL while timeout is running
  ended_at           TEXT,
  outcome            TEXT CHECK (outcome IN (NULL,'ok','fail','cancelled','lost')),
  result_summary     TEXT,
  log_path           TEXT
);

CREATE INDEX idx_cards_column   ON cards(column_id, position);
CREATE INDEX idx_comments_card  ON comments(card_id, created_at);
CREATE INDEX idx_runs_card      ON runs(card_id, started_at);
-- A card has at most one queued/running/awaiting lifecycle at a time.
CREATE UNIQUE INDEX idx_runs_one_open_per_card ON runs(card_id) WHERE ended_at IS NULL;
CREATE INDEX idx_runs_queued_fifo ON runs(id) WHERE started_at IS NULL AND ended_at IS NULL;
CREATE INDEX idx_runs_active_open ON runs(id) WHERE started_at IS NOT NULL AND ended_at IS NULL;
