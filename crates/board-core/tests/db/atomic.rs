//! Rollback and atomicity of the durable units of work: an armed fault must
//! leave the database exactly as it was found.

use std::fs;
use std::process::Command;

use super::{arm_fault, create_file_db, enqueue, reopened_state};
use board_core::db::{Db, FinalizeRun, LifecycleFaultPoint};
use board_core::protocol::{AwaitingReason, CardStatus, RunOutcome};

const CRASH_CHILD_ENV: &str = "HERDR_BOARD_DB_ATOMIC_CRASH_CHILD";

#[test]
fn enqueue_rolls_back_when_card_queue_update_fails() {
    let (_dir, path, card) = create_file_db("enqueue atomic");
    let before = reopened_state(&path, card.id);
    let db = Db::open(&path).unwrap();
    arm_fault(
        &path,
        "CREATE TRIGGER abort_queue BEFORE UPDATE OF status ON cards
         WHEN NEW.status='queued' BEGIN SELECT RAISE(ABORT,'fault: queue'); END;",
    );

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
    arm_fault(
        &path,
        "CREATE TRIGGER abort_running BEFORE UPDATE OF status ON cards
         WHEN NEW.status='running' BEGIN SELECT RAISE(ABORT,'fault: running'); END;",
    );

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
    arm_fault(
        &path,
        "CREATE TRIGGER abort_comment BEFORE INSERT ON comments
         BEGIN SELECT RAISE(ABORT,'fault: comment'); END;",
    );

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
    arm_fault(
        &path,
        "CREATE TRIGGER abort_next BEFORE INSERT ON runs
         WHEN NEW.prompt_snapshot='next'
         BEGIN SELECT RAISE(ABORT,'fault: next enqueue'); END;",
    );
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
        .args(["--exact", "atomic::crash_fault_hook_child", "--nocapture"])
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
    arm_fault(
        &path,
        "CREATE TRIGGER reject_timeout_pause BEFORE UPDATE OF timeout_paused_at_ms ON runs
         BEGIN SELECT RAISE(ABORT, 'reject pause'); END;",
    );
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
    arm_fault(
        &path,
        "CREATE TRIGGER reject_timeout_resume BEFORE UPDATE OF status ON cards
         WHEN OLD.status='awaiting' AND NEW.status='running'
         BEGIN SELECT RAISE(ABORT, 'reject resume'); END;",
    );
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

#[test]
fn reorder_column_rolls_back_partial_position_compaction() {
    let (_dir, path, _card) = create_file_db("reorder atomic");
    let db = Db::open(&path).unwrap();
    let todo = db.default_column_id(board_core::db::BOARD_ID).unwrap();
    let first = db
        .create_column(&board_core::protocol::ColumnCreateParams {
            name: "First".into(),
            ..Default::default()
        })
        .unwrap();
    let second = db
        .create_column(&board_core::protocol::ColumnCreateParams {
            name: "Second".into(),
            ..Default::default()
        })
        .unwrap();
    let last = db
        .create_column(&board_core::protocol::ColumnCreateParams {
            name: "Last".into(),
            ..Default::default()
        })
        .unwrap();
    let before = db.list_columns(board_core::db::BOARD_ID).unwrap();

    arm_fault(
        &path,
        &format!(
            "CREATE TRIGGER abort_reorder BEFORE UPDATE OF position ON columns
             WHEN OLD.id={todo} AND NEW.position=1
             BEGIN SELECT RAISE(ABORT, 'fault: reorder'); END;"
        ),
    );

    let error = db.reorder_column(last.id, 0).unwrap_err();
    assert!(error.to_string().contains("fault: reorder"), "{error}");
    let after = db.list_columns(board_core::db::BOARD_ID).unwrap();
    assert_eq!(
        after, before,
        "column positions must remain unchanged after an intermediate reorder failure"
    );
    // `First` and `Second` are the columns the aborted compaction would have
    // shifted; spell their rolled-back positions out rather than relying on the
    // whole-list comparison above.
    let position = |id: i64| {
        after
            .iter()
            .find(|column| column.id == id)
            .unwrap_or_else(|| panic!("column {id} still exists"))
            .position
    };
    assert_eq!(
        (
            position(todo),
            position(first.id),
            position(second.id),
            position(last.id)
        ),
        (0, 1, 2, 3),
        "every column keeps its pre-reorder position"
    );
}

#[test]
fn move_card_rolls_back_column_and_position_compaction_failure() {
    let (_dir, path, _card) = create_file_db("move atomic");
    let db = Db::open(&path).unwrap();
    let source = db.default_column_id(board_core::db::BOARD_ID).unwrap();
    let target = db
        .create_column(&board_core::protocol::ColumnCreateParams {
            name: "Target".into(),
            ..Default::default()
        })
        .unwrap();
    let keep = db
        .create_card(&board_core::protocol::CardCreateParams {
            title: "keep".into(),
            ..Default::default()
        })
        .unwrap();
    let moved = db
        .create_card(&board_core::protocol::CardCreateParams {
            title: "moved".into(),
            ..Default::default()
        })
        .unwrap();
    let target_first = db
        .create_card(&board_core::protocol::CardCreateParams {
            title: "target first".into(),
            column_id: Some(target.id),
            ..Default::default()
        })
        .unwrap();
    db.create_card(&board_core::protocol::CardCreateParams {
        title: "target second".into(),
        column_id: Some(target.id),
        ..Default::default()
    })
    .unwrap();
    let before_source = db.list_cards_in_column(source).unwrap();
    let before_target = db.list_cards_in_column(target.id).unwrap();

    let target_first_id = target_first.id;
    arm_fault(
        &path,
        &format!(
            "CREATE TRIGGER abort_card_compaction BEFORE UPDATE OF position ON cards
             WHEN OLD.id={target_first_id} AND NEW.position=1
             BEGIN SELECT RAISE(ABORT, 'fault: card compaction'); END;"
        ),
    );

    let error = db.move_card(moved.id, target.id, Some(0)).unwrap_err();
    assert!(
        error.to_string().contains("fault: card compaction"),
        "{error}"
    );
    assert_eq!(db.list_cards_in_column(source).unwrap(), before_source);
    assert_eq!(db.list_cards_in_column(target.id).unwrap(), before_target);
    assert_eq!(db.get_card(moved.id).unwrap().unwrap().column_id, source);
    assert_eq!(
        db.get_card(keep.id).unwrap().unwrap().position,
        before_source
            .iter()
            .find(|card| card.id == keep.id)
            .unwrap()
            .position
    );
}

#[test]
fn delete_column_rolls_back_card_migration_and_both_position_compactions() {
    let (_dir, path, _card) = create_file_db("delete migration atomic");
    let db = Db::open(&path).unwrap();
    let target = db.default_column_id(board_core::db::BOARD_ID).unwrap();
    let source = db
        .create_column(&board_core::protocol::ColumnCreateParams {
            name: "Source".into(),
            ..Default::default()
        })
        .unwrap();
    db.create_card(&board_core::protocol::CardCreateParams {
        title: "existing target".into(),
        ..Default::default()
    })
    .unwrap();
    let first = db
        .create_card(&board_core::protocol::CardCreateParams {
            title: "first source".into(),
            column_id: Some(source.id),
            ..Default::default()
        })
        .unwrap();
    let second = db
        .create_card(&board_core::protocol::CardCreateParams {
            title: "second source".into(),
            column_id: Some(source.id),
            ..Default::default()
        })
        .unwrap();
    let before_columns = db.list_columns(board_core::db::BOARD_ID).unwrap();
    let before_target = db.list_cards_in_column(target).unwrap();
    let before_source = db.list_cards_in_column(source.id).unwrap();

    arm_fault(
        &path,
        &format!(
            "CREATE TRIGGER abort_second_card_migration BEFORE UPDATE OF column_id ON cards
             WHEN OLD.id={} AND OLD.column_id={}
             BEGIN SELECT RAISE(ABORT, 'fault: card migration'); END;",
            second.id, source.id
        ),
    );

    let error = db.delete_column(source.id, Some(target)).unwrap_err();
    assert!(
        error.to_string().contains("fault: card migration"),
        "{error}"
    );
    assert_eq!(
        db.list_columns(board_core::db::BOARD_ID).unwrap(),
        before_columns
    );
    assert_eq!(db.list_cards_in_column(target).unwrap(), before_target);
    assert_eq!(db.list_cards_in_column(source.id).unwrap(), before_source);
    assert_eq!(db.get_card(first.id).unwrap().unwrap().column_id, source.id);
    assert_eq!(
        db.get_card(second.id).unwrap().unwrap().column_id,
        source.id
    );
}

#[test]
fn captured_session_promotion_rolls_back_when_card_write_fails() {
    let (_dir, path, card) = create_file_db("capture atomic");
    let db = Db::open(&path).unwrap();
    let run = db
        .enqueue_run_uow(&enqueue(card.id, card.column_id))
        .unwrap();
    drop(db);
    let before = reopened_state(&path, card.id);
    let db = Db::open(&path).unwrap();
    arm_fault(
        &path,
        "CREATE TRIGGER abort_capture BEFORE UPDATE OF session_id ON cards
         BEGIN SELECT RAISE(ABORT,'fault: capture'); END;",
    );

    assert!(db
        .promote_captured_session_uow(run.id, "thread-abc")
        .is_err());
    drop(db);

    assert_eq!(
        reopened_state(&path, card.id),
        before,
        "a failed capture must leave the run AND the card exactly as found"
    );
}

#[test]
fn integrated_capture_promotion_rolls_back_when_card_write_fails() {
    // The daemon's launch path persists the captured id inside the promotion
    // UOW: a card-side failure must roll back the run promotion AND the
    // session id together — never a started run without its identity.
    let (_dir, path, card) = create_file_db("capture promotion atomic");
    let db = Db::open(&path).unwrap();
    let run = db
        .enqueue_run_uow(&enqueue(card.id, card.column_id))
        .unwrap();
    drop(db);
    let before = reopened_state(&path, card.id);
    let db = Db::open(&path).unwrap();
    arm_fault(
        &path,
        "CREATE TRIGGER abort_capture_promotion BEFORE UPDATE OF session_id ON cards
         BEGIN SELECT RAISE(ABORT,'fault: capture promotion'); END;",
    );

    assert!(db
        .promote_run_with_anchor_uow(
            run.id,
            Some("workspace"),
            Some("pane"),
            None,
            None,
            Some("thread-captured"),
        )
        .is_err());
    drop(db);

    assert_eq!(
        reopened_state(&path, card.id),
        before,
        "a failed capture promotion must leave the run queued, the card queued, and no session id anywhere"
    );
}

#[test]
fn capture_crash_fault_hook_child() {
    if std::env::var_os(CRASH_CHILD_ENV).is_none() {
        return;
    }
    let path = std::path::PathBuf::from(std::env::var_os("DB_PATH").unwrap());
    let run_id: i64 = std::env::var("RUN_ID").unwrap().parse().unwrap();
    let effect_path = std::path::PathBuf::from(std::env::var_os("EFFECT_PATH").unwrap());
    let event_path = std::path::PathBuf::from(std::env::var_os("EVENT_PATH").unwrap());
    let db = Db::open_with_lifecycle_fault_hook(&path, |point| {
        if point == LifecycleFaultPoint::CaptureAfterRunUpdate {
            std::process::exit(87);
        }
        Ok(())
    })
    .unwrap();
    let run = db
        .promote_captured_session_uow(run_id, "thread-crash")
        .unwrap();
    fs::write(effect_path, format!("{:?}", run)).unwrap();
    fs::write(event_path, "captured").unwrap();
}

#[test]
fn subprocess_crash_before_capture_commit_reopens_exact_prior_state() {
    if std::env::var_os(CRASH_CHILD_ENV).is_some() {
        return;
    }
    let (_dir, path, card) = create_file_db("capture crash atomic");
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
        .args([
            "--exact",
            "atomic::capture_crash_fault_hook_child",
            "--nocapture",
        ])
        .env(CRASH_CHILD_ENV, "1")
        .env("DB_PATH", &path)
        .env("RUN_ID", run.id.to_string())
        .env("EFFECT_PATH", &effect_path)
        .env("EVENT_PATH", &event_path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(87), "{output:?}");

    assert_eq!(reopened_state(&path, card.id), before);
    assert_eq!(fs::read(&effect_path).unwrap(), b"");
    assert_eq!(fs::read(&event_path).unwrap(), b"");
}
