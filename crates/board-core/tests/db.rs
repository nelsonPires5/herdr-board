//! Db migrations, seed, CRUD, position management, and atomic units of work.

#[path = "db/atomic.rs"]
mod atomic;
#[path = "db/crud.rs"]
mod crud;
#[path = "db/migrations.rs"]
mod migrations;
#[path = "db/runs.rs"]
mod runs;

use std::path::{Path, PathBuf};

use board_core::db::{Db, EnqueueRun};
use board_core::model::{Card, Comment, Run};
use board_core::protocol::CardCreateParams;
use rusqlite::Connection;

fn mem() -> Db {
    Db::open_in_memory().unwrap()
}

/// A queued-run unit of work with placeholder payloads.
fn enqueue<'a>(card_id: i64, column_id: i64) -> EnqueueRun<'a> {
    EnqueueRun {
        card_id,
        column_id,
        harness: "pi",
        argv_json: "[]",
        prompt_snapshot: "p",
        system_prompt_snapshot: Some("s"),
        launch_spec_json: None,
        session_id: None,
        session: None,
    }
}

/// A file-backed db holding a single card. The `TempDir` must stay bound for
/// the lifetime of the test — dropping it removes the database file.
fn create_file_db(title: &str) -> (tempfile::TempDir, PathBuf, Card) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let db = Db::open(&path).unwrap();
    let card = db
        .create_card(&CardCreateParams {
            title: title.into(),
            ..Default::default()
        })
        .unwrap();
    (dir, path, card)
}

/// Reopen the database at `path` and read back everything a unit of work can
/// touch for `card_id`, so a prior snapshot can be compared byte for byte.
fn reopened_state(path: &Path, card_id: i64) -> (Card, Vec<Run>, Vec<Comment>) {
    let db = Db::open(path).unwrap();
    (
        db.get_card(card_id).unwrap().unwrap(),
        db.list_runs(card_id).unwrap(),
        db.list_comments(card_id).unwrap(),
    )
}

/// Arm a SQLite trigger on the database at `path` so a specific write aborts.
/// A second connection installs it, leaving the connection under test alone.
fn arm_fault(path: &Path, trigger_sql: &str) {
    Connection::open(path)
        .unwrap()
        .execute_batch(trigger_sql)
        .unwrap();
}
