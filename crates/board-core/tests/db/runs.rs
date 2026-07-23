use super::mem;
use board_core::db::{Db, EnqueueRun, FinalizeRun, BOARD_ID};
use board_core::launch::{ExecutionSpec, RunLaunchSpec};
use board_core::protocol::{AwaitingReason, CardCreateParams, CardStatus, RunOutcome};
use rusqlite::Connection;

#[test]
fn run_system_prompt_snapshot_roundtrips_across_file_reopen() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let exact = "old instructions\\n\\nsecond line\\ntrailing spaces  ";
    let (card_id, run_id) = {
        let db = Db::open(&path).unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "snapshot".into(),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "pi",
                argv_json: r#"["pi","--model","x"]"#,
                prompt_snapshot: "Card task:\nwork",
                system_prompt_snapshot: Some(exact),
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        (card.id, run.id)
    };
    let db = Db::open(&path).unwrap();
    let run = db.get_run(run_id).unwrap();
    assert_eq!(run.card_id, card_id);
    assert_eq!(run.system_prompt_snapshot.as_deref(), Some(exact));
}

#[test]
fn launch_spec_json_roundtrips_exactly_across_file_reopen() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let spec = RunLaunchSpec::v1(ExecutionSpec {
        argv: vec!["agent".into(), "arg\n\0  ".into()],
        env: vec![("KEY".into(), "value\n\0  ".into())],
        agent_kind: None,
        initial_prompt: Some("prompt  ".into()),
        system_prompt: None,
    });
    let exact_json = serde_json::to_string(&spec).unwrap();
    let run_id = {
        let db = Db::open(&path).unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "spec".into(),
                ..Default::default()
            })
            .unwrap();
        db.enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "custom",
            argv_json: r#"["legacy"]"#,
            prompt_snapshot: "p",
            system_prompt_snapshot: Some("s"),
            launch_spec_json: Some(&exact_json),
            session_id: None,
            session: Some("enqueue-session"),
        })
        .unwrap()
        .id
    };
    for _ in 0..2 {
        let db = Db::open(&path).unwrap();
        assert_eq!(
            db.get_run(run_id).unwrap().launch_spec.as_ref(),
            Some(&spec)
        );
        drop(db);
        let conn = Connection::open(&path).unwrap();
        let stored: String = conn
            .query_row(
                "SELECT launch_spec_json FROM runs WHERE id=?1",
                [run_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored.as_bytes(), exact_json.as_bytes());
    }
}

#[test]
fn unsupported_persisted_launch_spec_is_rejected_on_read() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let run_id = {
        let db = Db::open(&path).unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "future".into(),
                ..Default::default()
            })
            .unwrap();
        db.enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "p",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap()
        .id
    };
    Connection::open(&path).unwrap().execute(
        "UPDATE runs SET launch_spec_json='{\"version\":99,\"execution\":{\"argv\":[],\"env\":[],\"agent_kind\":null,\"initial_prompt\":null,\"system_prompt\":null}}' WHERE id=?1",
        [run_id],
    ).unwrap();
    let error = Db::open(&path).unwrap().get_run(run_id).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported launch spec version 99"),
        "{error}"
    );
}

#[test]
fn comments_and_runs_roundtrip() {
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "X".into(),
            ..Default::default()
        })
        .unwrap();
    db.add_comment(card.id, "user", "hello").unwrap();
    db.add_comment(card.id, "agent:1", "did it").unwrap();
    assert_eq!(db.list_comments(card.id).unwrap().len(), 2);

    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "claude",
            argv_json: "[]",
            prompt_snapshot: "prompt",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: Some("sess"),
            session: None,
        })
        .unwrap();
    assert!(run.started_at.is_none());
    assert_eq!(db.count_queued_runs().unwrap(), 1);
    let queued = db.queued_runs_with_cards().unwrap();
    assert_eq!((queued[0].0.id, queued[0].1.id), (run.id, card.id));
    assert!(db.active_runs_with_cards().unwrap().is_empty());

    db.promote_run_uow(run.id, Some("w4"), Some("p9"), None)
        .unwrap();
    assert_eq!(db.count_active_runs().unwrap(), 1);
    assert!(db.queued_runs_with_cards().unwrap().is_empty());
    assert_eq!(db.active_runs_with_cards().unwrap()[0].0.id, run.id);
    let active = db.active_run_for_card(card.id).unwrap().unwrap();
    assert_eq!(active.herdr_pane_id.as_deref(), Some("p9"));

    db.finalize_run_uow(&FinalizeRun {
        run_id: run.id,
        outcome: RunOutcome::Ok,
        summary: Some("done"),
        comments: &[],
        target_column_id: None,
        final_status: CardStatus::Done,
        final_awaiting_reason: None,
        next: None,
    })
    .unwrap();
    assert_eq!(db.count_active_runs().unwrap(), 0);
    assert!(db.queued_runs_with_cards().unwrap().is_empty());
    assert!(db.active_runs_with_cards().unwrap().is_empty());
    let done = db.get_run(run.id).unwrap();
    assert_eq!(done.outcome, Some(RunOutcome::Ok));
    assert!(done.ended_at.is_some());
}

#[test]
fn direct_scheduler_queries_are_global_fifo_and_exclude_started_and_ended_rows() {
    let db = mem();
    let make = |title: &str| {
        db.create_card(&CardCreateParams {
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
    };
    let queued_one_card = make("queued one");
    let ended_card = make("ended");
    let queued_two_card = make("queued two");
    let active_card = make("active");

    let queued_one = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: queued_one_card.id,
            column_id: queued_one_card.column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "q1",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    let ended = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: ended_card.id,
            column_id: ended_card.column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "ended",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    db.finalize_run_uow(&FinalizeRun {
        run_id: ended.id,
        outcome: RunOutcome::Ok,
        summary: None,
        comments: &[],
        target_column_id: None,
        final_status: CardStatus::Done,
        final_awaiting_reason: None,
        next: None,
    })
    .unwrap();
    let queued_two = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: queued_two_card.id,
            column_id: queued_two_card.column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "q2",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    let active = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: active_card.id,
            column_id: active_card.column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "active",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    db.promote_run_uow(active.id, Some("workspace"), Some("pane"), None)
        .unwrap();

    let queued: Vec<_> = db
        .queued_runs_with_cards()
        .unwrap()
        .into_iter()
        .map(|(run, card)| (run.id, card.id))
        .collect();
    assert_eq!(
        queued,
        vec![
            (queued_one.id, queued_one_card.id),
            (queued_two.id, queued_two_card.id),
        ]
    );
    let active_rows: Vec<_> = db
        .active_runs_with_cards()
        .unwrap()
        .into_iter()
        .map(|(run, card)| (run.id, card.id))
        .collect();
    assert_eq!(active_rows, vec![(active.id, active_card.id)]);
    assert!(!queued
        .iter()
        .any(|(id, _)| *id == active.id || *id == ended.id));
    assert!(!active_rows.iter().any(|(id, _)| *id == ended.id));
}

#[test]
fn active_run_summaries_are_started_open_and_board_scoped() {
    let db = mem();
    let other = db.open_board("/tmp/other-board").unwrap();
    let make = |board_id: i64, title: &str| {
        db.create_card(&CardCreateParams {
            board_id: Some(board_id),
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
    };
    let active = make(BOARD_ID, "active");
    let queued = make(BOARD_ID, "queued");
    let ended = make(BOARD_ID, "ended");
    let other_active = make(other.id, "other active");

    let open = |card: &board_core::model::Card| {
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "prompt",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, Some("workspace"), Some("pane"), None)
            .unwrap();
        run
    };
    let _active_run = open(&active);
    let _queued_run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: queued.id,
            column_id: queued.column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "prompt",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    let ended_run = open(&ended);
    db.finalize_run_uow(&FinalizeRun {
        run_id: ended_run.id,
        outcome: RunOutcome::Ok,
        summary: None,
        comments: &[],
        target_column_id: None,
        final_status: CardStatus::Done,
        final_awaiting_reason: None,
        next: None,
    })
    .unwrap();
    let _other_run = open(&other_active);

    let summaries = db.active_run_summaries(BOARD_ID).unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].card_id, active.id);
    assert!(!summaries[0].started_at.is_empty());
    assert_eq!(db.active_run_summaries(other.id).unwrap().len(), 1);
}

#[test]
fn durable_timeout_pause_resume_is_atomic_idempotent_and_saturating() {
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "timed".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "p",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    db.promote_run_uow(run.id, None, None, Some(i64::MAX - 10))
        .unwrap();

    db.pause_run_timeout_uow(card.id, AwaitingReason::IdleExpired, 100)
        .unwrap();
    db.pause_run_timeout_uow(card.id, AwaitingReason::AgentDone, 200)
        .unwrap();
    let paused = db.get_run(run.id).unwrap();
    assert_eq!(paused.timeout_paused_at_ms, Some(100));
    assert_eq!(
        db.get_card(card.id).unwrap().unwrap().awaiting_reason,
        Some(AwaitingReason::AgentDone)
    );

    db.resume_run_timeout_uow(card.id, CardStatus::Running, 500)
        .unwrap();
    let resumed = db.get_run(run.id).unwrap();
    assert_eq!(resumed.timeout_deadline_at_ms, Some(i64::MAX));
    assert_eq!(resumed.timeout_paused_at_ms, None);
    db.resume_run_timeout_uow(card.id, CardStatus::Running, 900)
        .unwrap();
    assert_eq!(
        db.get_run(run.id).unwrap().timeout_deadline_at_ms,
        Some(i64::MAX)
    );
}
