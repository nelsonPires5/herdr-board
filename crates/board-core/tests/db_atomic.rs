use std::fs;
use std::process::Command;

use board_core::db::{Db, EnqueueRun, FinalizeRun, LifecycleFaultPoint};
use board_core::model::{Card, Comment, Run};
use board_core::protocol::{AwaitingReason, CardCreateParams, CardStatus, RunOutcome};
use rusqlite::Connection;

const INDEX_SQL: &str =
    "CREATE UNIQUE INDEX idx_runs_one_open_per_card ON runs(card_id) WHERE ended_at IS NULL";
const CRASH_CHILD_ENV: &str = "HERDR_BOARD_DB_ATOMIC_CRASH_CHILD";

fn enqueue<'a>(card_id: i64, column_id: i64) -> EnqueueRun<'a> {
    EnqueueRun {
        card_id,
        column_id,
        harness: "pi",
        argv_json: "[]",
        prompt_snapshot: "p",
        system_prompt_snapshot: Some("s"),
        session_id: None,
        session: None,
    }
}

fn create_file_db(title: &str) -> (tempfile::TempDir, std::path::PathBuf, Card) {
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

fn reopened_state(path: &std::path::Path, card_id: i64) -> (Card, Vec<Run>, Vec<Comment>) {
    let db = Db::open(path).unwrap();
    (
        db.get_card(card_id).unwrap().unwrap(),
        db.list_runs(card_id).unwrap(),
        db.list_comments(card_id).unwrap(),
    )
}

#[test]
fn enqueue_rolls_back_when_card_queue_update_fails() {
    let (_dir, path, card) = create_file_db("enqueue atomic");
    let before = reopened_state(&path, card.id);
    let db = Db::open(&path).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER abort_queue BEFORE UPDATE OF status ON cards
             WHEN NEW.status='queued' BEGIN SELECT RAISE(ABORT,'fault: queue'); END;",
        )
        .unwrap();

    assert!(db
        .enqueue_run_uow(&enqueue(card.id, card.column_id))
        .is_err());
    drop(db);

    assert_eq!(reopened_state(&path, card.id), before);
}

#[test]
fn promotion_rolls_back_when_card_running_update_fails() {
    let (_dir, path, card) = create_file_db("promotion atomic");
    let db = Db::open(&path).unwrap();
    let run = db
        .enqueue_run_uow(&enqueue(card.id, card.column_id))
        .unwrap();
    drop(db);
    let before = reopened_state(&path, card.id);
    let db = Db::open(&path).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER abort_running BEFORE UPDATE OF status ON cards
             WHEN NEW.status='running' BEGIN SELECT RAISE(ABORT,'fault: running'); END;",
        )
        .unwrap();

    assert!(db
        .promote_run_uow(run.id, Some("workspace"), Some("pane"))
        .is_err());
    drop(db);

    assert_eq!(reopened_state(&path, card.id), before);
}

#[test]
fn finalization_rolls_back_when_comment_insert_fails() {
    let (_dir, path, card) = create_file_db("finalization atomic");
    let db = Db::open(&path).unwrap();
    let run = db
        .enqueue_run_uow(&enqueue(card.id, card.column_id))
        .unwrap();
    drop(db);
    let before = reopened_state(&path, card.id);
    let db = Db::open(&path).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER abort_comment BEFORE INSERT ON comments
             BEGIN SELECT RAISE(ABORT,'fault: comment'); END;",
        )
        .unwrap();

    assert!(db
        .finalize_run_uow(&FinalizeRun {
            run_id: run.id,
            outcome: RunOutcome::Ok,
            summary: Some("summary"),
            comment: Some(("system", "done")),
            target_column_id: None,
            final_status: CardStatus::Done,
            final_awaiting_reason: None,
            next: None,
        })
        .is_err());
    drop(db);

    assert_eq!(reopened_state(&path, card.id), before);
}

#[test]
fn auto_finalize_rolls_back_when_next_enqueue_fails() {
    let (_dir, path, card) = create_file_db("auto-finalize atomic");
    let db = Db::open(&path).unwrap();
    let run = db
        .enqueue_run_uow(&enqueue(card.id, card.column_id))
        .unwrap();
    drop(db);
    let before = reopened_state(&path, card.id);
    let db = Db::open(&path).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER abort_next BEFORE INSERT ON runs
             WHEN NEW.prompt_snapshot='next'
             BEGIN SELECT RAISE(ABORT,'fault: next enqueue'); END;",
        )
        .unwrap();
    let mut next = enqueue(card.id, card.column_id);
    next.prompt_snapshot = "next";

    assert!(db
        .finalize_run_uow(&FinalizeRun {
            run_id: run.id,
            outcome: RunOutcome::Ok,
            summary: Some("finished"),
            comment: Some(("agent", "result")),
            target_column_id: None,
            final_status: CardStatus::Done,
            final_awaiting_reason: None,
            next: Some(next),
        })
        .is_err());
    drop(db);

    assert_eq!(reopened_state(&path, card.id), before);
}

#[test]
fn successful_finalize_returns_only_durable_post_commit_dtos() {
    let (_dir, path, card) = create_file_db("post-commit dto");
    let db = Db::open(&path).unwrap();
    let run = db
        .enqueue_run_uow(&enqueue(card.id, card.column_id))
        .unwrap();
    let effects = db
        .finalize_run_uow(&FinalizeRun {
            run_id: run.id,
            outcome: RunOutcome::Ok,
            summary: Some("durable"),
            comment: Some(("system", "durable")),
            target_column_id: None,
            final_status: CardStatus::Awaiting,
            final_awaiting_reason: Some(AwaitingReason::AgentDone),
            next: None,
        })
        .unwrap();
    drop(db);

    let reopened = reopened_state(&path, card.id);
    assert_eq!(effects.card, reopened.0);
    assert_eq!(effects.finished_run, reopened.1[0]);
    assert_eq!(effects.next_run, None);
    assert_eq!(reopened.2.len(), 1);
}

#[test]
fn crash_fault_hook_child() {
    if std::env::var_os(CRASH_CHILD_ENV).is_none() {
        return;
    }
    let path = std::path::PathBuf::from(std::env::var_os("DB_PATH").unwrap());
    let run_id: i64 = std::env::var("RUN_ID").unwrap().parse().unwrap();
    let effect_path = std::path::PathBuf::from(std::env::var_os("EFFECT_PATH").unwrap());
    let event_path = std::path::PathBuf::from(std::env::var_os("EVENT_PATH").unwrap());
    let db = Db::open_with_lifecycle_fault_hook(&path, |point| {
        if point == LifecycleFaultPoint::FinalizeAfterRunUpdate {
            std::process::exit(86);
        }
    })
    .unwrap();
    let effects = db
        .finalize_run_uow(&FinalizeRun {
            run_id,
            outcome: RunOutcome::Ok,
            summary: Some("must roll back"),
            comment: Some(("system", "must roll back")),
            target_column_id: None,
            final_status: CardStatus::Done,
            final_awaiting_reason: None,
            next: None,
        })
        .unwrap();
    fs::write(effect_path, format!("{:?}", effects)).unwrap();
    fs::write(event_path, "run_ended").unwrap();
}

#[test]
fn subprocess_crash_before_commit_reopens_exact_prior_state_with_zero_event_or_effect() {
    if std::env::var_os(CRASH_CHILD_ENV).is_some() {
        return;
    }
    let (_dir, path, card) = create_file_db("crash atomic");
    let db = Db::open(&path).unwrap();
    let run = db
        .enqueue_run_uow(&enqueue(card.id, card.column_id))
        .unwrap();
    drop(db);
    let before = reopened_state(&path, card.id);
    let effect_path = path.with_extension("effects");
    let event_path = path.with_extension("events");
    fs::File::create(&effect_path).unwrap();
    fs::File::create(&event_path).unwrap();

    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_fault_hook_child", "--nocapture"])
        .env(CRASH_CHILD_ENV, "1")
        .env("DB_PATH", &path)
        .env("RUN_ID", run.id.to_string())
        .env("EFFECT_PATH", &effect_path)
        .env("EVENT_PATH", &event_path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(86), "{output:?}");

    assert_eq!(reopened_state(&path, card.id), before);
    assert_eq!(fs::read(&effect_path).unwrap(), b"");
    assert_eq!(fs::read(&event_path).unwrap(), b"");
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
    assert_eq!(db.user_version().unwrap(), 8);
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
        assert_eq!(db.user_version().unwrap(), 8);
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

#[test]
fn unique_open_run_index_rejects_second_open_run_and_allows_history() {
    let (_dir, path, card) = create_file_db("one open run");
    let db = Db::open(&path).unwrap();
    let first = db
        .enqueue_run_uow(&enqueue(card.id, card.column_id))
        .unwrap();
    assert!(db
        .enqueue_run_uow(&enqueue(card.id, card.column_id))
        .is_err());
    db.finish_run(first.id, RunOutcome::Ok, None).unwrap();
    db.enqueue_run_uow(&enqueue(card.id, card.column_id))
        .unwrap();
    drop(db);
    assert_eq!(reopened_state(&path, card.id).1.len(), 2);
}
