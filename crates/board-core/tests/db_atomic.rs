use std::fs;
use std::process::Command;

use board_core::db::{Db, EnqueueRun, FinalizeRun, LifecycleFaultPoint};
use board_core::model::{Card, Comment, Run};
use board_core::protocol::{AwaitingReason, CardCreateParams, CardStatus, RunOutcome};
use rusqlite::{types::Value, Connection};

const INDEX_SQL: &str =
    "CREATE UNIQUE INDEX idx_runs_one_open_per_card ON runs(card_id) WHERE ended_at IS NULL";
const QUEUED_INDEX_SQL: &str =
    "CREATE INDEX idx_runs_queued_fifo ON runs(id) WHERE started_at IS NULL AND ended_at IS NULL";
const ACTIVE_INDEX_SQL: &str =
    "CREATE INDEX idx_runs_active_open ON runs(id) WHERE started_at IS NOT NULL AND ended_at IS NULL";
const CRASH_CHILD_ENV: &str = "HERDR_BOARD_DB_ATOMIC_CRASH_CHILD";

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

#[test]
fn fresh_v11_has_exact_partial_scheduler_indexes_and_query_plans() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let db = Db::open(&path).unwrap();
    assert_eq!(db.user_version().unwrap(), 11);
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
fn v9_file_fixture_upgrades_through_v11_without_changing_existing_bytes() {
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
        // Historical v9→v11 migration fixture: enqueue_run_uow writes a v11
        // row; after manual downgrade to user_version=9 the migration path
        // still re-adds indexes and must preserve every byte.
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
    assert_eq!(db.user_version().unwrap(), 11);
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
            11
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
        .promote_run_uow(run.id, Some("workspace"), Some("pane"), None)
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
            comments: &[("system", "done")],
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
            comments: &[("agent", "result")],
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
            comments: &[("system", "durable")],
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
        Ok(())
    })
    .unwrap();
    let effects = db
        .finalize_run_uow(&FinalizeRun {
            run_id,
            outcome: RunOutcome::Ok,
            summary: Some("must roll back"),
            comments: &[("system", "must roll back")],
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
    assert_eq!(db.user_version().unwrap(), 11);
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
        assert_eq!(db.user_version().unwrap(), 11);
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
    db.finalize_run_uow(&FinalizeRun {
        run_id: first.id,
        outcome: RunOutcome::Ok,
        summary: None,
        comments: &[],
        target_column_id: None,
        final_status: CardStatus::Done,
        final_awaiting_reason: None,
        next: None,
    })
    .unwrap();
    db.enqueue_run_uow(&enqueue(card.id, card.column_id))
        .unwrap();
    drop(db);
    assert_eq!(reopened_state(&path, card.id).1.len(), 2);
}

#[test]
fn timeout_pause_rolls_back_card_when_run_write_fails() {
    let (_dir, path, card) = create_file_db("pause rollback");
    let db = Db::open(&path).unwrap();
    let run = db
        .enqueue_run_uow(&enqueue(card.id, card.column_id))
        .unwrap();
    db.promote_run_uow(run.id, None, None, Some(1_000)).unwrap();
    drop(db);
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_timeout_pause BEFORE UPDATE OF timeout_paused_at_ms ON runs
         BEGIN SELECT RAISE(ABORT, 'reject pause'); END;",
        )
        .unwrap();
    let db = Db::open(&path).unwrap();
    assert!(db
        .pause_run_timeout_uow(card.id, AwaitingReason::AgentDone, 100)
        .is_err());
    assert_eq!(
        db.get_card(card.id).unwrap().unwrap().status,
        CardStatus::Running
    );
    assert_eq!(db.get_run(run.id).unwrap().timeout_paused_at_ms, None);
}

#[test]
fn timeout_resume_rolls_back_run_when_card_write_fails() {
    let (_dir, path, card) = create_file_db("resume rollback");
    let db = Db::open(&path).unwrap();
    let run = db
        .enqueue_run_uow(&enqueue(card.id, card.column_id))
        .unwrap();
    db.promote_run_uow(run.id, None, None, Some(1_000)).unwrap();
    db.pause_run_timeout_uow(card.id, AwaitingReason::AgentDone, 100)
        .unwrap();
    drop(db);
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_timeout_resume BEFORE UPDATE OF status ON cards
         WHEN OLD.status='awaiting' AND NEW.status='running'
         BEGIN SELECT RAISE(ABORT, 'reject resume'); END;",
        )
        .unwrap();
    let db = Db::open(&path).unwrap();
    assert!(db
        .resume_run_timeout_uow(card.id, CardStatus::Running, 500)
        .is_err());
    assert_eq!(
        db.get_card(card.id).unwrap().unwrap().status,
        CardStatus::Awaiting
    );
    let persisted = db.get_run(run.id).unwrap();
    assert_eq!(persisted.timeout_deadline_at_ms, Some(1_000));
    assert_eq!(persisted.timeout_paused_at_ms, Some(100));
}
