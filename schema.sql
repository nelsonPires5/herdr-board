-- herdr-board SQLite schema (WAL mode; boardd is the only writer)
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE boards (
  id         INTEGER PRIMARY KEY,
  name       TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
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
  harness         TEXT NOT NULL DEFAULT 'claude',
  model           TEXT,
  effort          TEXT CHECK (effort IN (NULL,'low','medium','high','xhigh','max')),
  permission_mode TEXT,                        -- e.g. acceptEdits, plan; bypass = explicit opt-in
  space_kind      TEXT NOT NULL DEFAULT 'workspace'
                    CHECK (space_kind IN ('workspace','cwd','worktree')),
  space_ref       TEXT,                        -- herdr workspace id | path
  worktree_base   TEXT,                        -- base ref when space_kind='worktree'
  status          TEXT NOT NULL DEFAULT 'idle'
                    CHECK (status IN ('idle','queued','running','blocked','failed')),
  session_id      TEXT,                        -- harness session for --resume
  created_at      TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
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
  session_id         TEXT,
  started_at         TEXT,
  ended_at           TEXT,
  outcome            TEXT CHECK (outcome IN (NULL,'ok','fail','cancelled','lost')),
  result_summary     TEXT,
  log_path           TEXT
);

CREATE INDEX idx_cards_column   ON cards(column_id, position);
CREATE INDEX idx_comments_card  ON comments(card_id, created_at);
CREATE INDEX idx_runs_card      ON runs(card_id, started_at);
