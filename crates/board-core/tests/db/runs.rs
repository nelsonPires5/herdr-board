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

#[test]
fn run_for_card_requires_the_exact_owning_card() {
    let db = mem();
    let make_card = |title: &str| {
        db.create_card(&CardCreateParams {
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
    };
    let owner = make_card("owner");
    let other = make_card("other");
    let enqueue = |card: &board_core::model::Card| {
        db.enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "p",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: Some("conv-1"),
            session: Some("work"),
        })
        .unwrap()
    };
    let first = enqueue(&owner);
    db.promote_run_uow(first.id, Some("w1"), Some("w1:p1"), None)
        .unwrap();
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
    let second = enqueue(&owner);
    db.promote_run_uow(second.id, Some("w1"), Some("w1:p2"), None)
        .unwrap();
    let foreign = enqueue(&other);

    // Happy path: an *older* run is addressable, not only the latest.
    let got = db.run_for_card(owner.id, first.id).unwrap();
    assert_eq!(got.id, first.id);
    assert_eq!(got.card_id, owner.id);
    assert_eq!(got.herdr_pane_id.as_deref(), Some("w1:p1"));
    assert_eq!(got.session.as_deref(), Some("work"));
    assert_eq!(got.session_id.as_deref(), Some("conv-1"));
    assert_eq!(db.run_for_card(owner.id, second.id).unwrap().id, second.id);

    // A run that exists but belongs to another card is not found for this
    // card, and the message must not leak the owning card.
    let err = db.run_for_card(owner.id, foreign.id).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains(&foreign.id.to_string()), "message: {msg}");
    assert!(msg.contains(&owner.id.to_string()), "message: {msg}");
    assert!(!msg.contains(&format!("card {}", other.id)), "leak: {msg}");

    // Unknown run id.
    let err = db.run_for_card(owner.id, 99_999).unwrap_err();
    assert!(err.to_string().contains("99999"), "message: {err}");

    // Unknown card: `require_card` reports the card, not the run.
    let unknown = db.run_for_card(4242, first.id).unwrap_err();
    assert_eq!(unknown.to_string(), "not found: card 4242");
}

// ---------------------------------------------------------------------------
// Captured-session promotion (C4 core): a self-minting harness like codex
// reports its thread/conversation id only after launch, so the daemon persists
// it atomically on BOTH the run and the card — never half-promoted.
// ---------------------------------------------------------------------------

/// Enqueue a codex-shaped run: Mint persists `session_id: NULL` (the board
/// never invents a uuid for codex; the captured thread id replaces it).
fn enqueue_codex<'a>(card_id: i64, column_id: i64, session_id: Option<&'a str>) -> EnqueueRun<'a> {
    EnqueueRun {
        card_id,
        column_id,
        harness: "codex",
        argv_json: r#"["codex","--model","m"]"#,
        prompt_snapshot: "Card task:\nwork",
        system_prompt_snapshot: Some("s"),
        launch_spec_json: None,
        session_id,
        session: None,
    }
}

#[test]
fn captured_session_promotion_writes_run_and_card_in_one_transaction() {
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "codex mint".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&enqueue_codex(card.id, card.column_id, None))
        .unwrap();
    // Mint persisted NULL — never a board-invented uuid.
    assert_eq!(run.session_id, None);
    assert_eq!(db.get_card(card.id).unwrap().unwrap().session_id, None);

    // Capture while still queued (post-launch capture can race promotion).
    let promoted = db
        .promote_captured_session_uow(run.id, "thread-abc")
        .unwrap();
    assert_eq!(promoted.session_id.as_deref(), Some("thread-abc"));
    assert_eq!(
        db.get_card(card.id).unwrap().unwrap().session_id.as_deref(),
        Some("thread-abc")
    );

    // A later capture on the same open run replaces the id (fork: the NEW
    // thread id supersedes the source id recorded at enqueue).
    let replaced = db
        .promote_captured_session_uow(run.id, "thread-forked")
        .unwrap();
    assert_eq!(replaced.session_id.as_deref(), Some("thread-forked"));
    assert_eq!(
        db.get_card(card.id).unwrap().unwrap().session_id.as_deref(),
        Some("thread-forked")
    );
}

// ---------------------------------------------------------------------------
// Integrated promotion (C4 daemon path): the captured id is persisted in the
// SAME transaction as the run promotion + card running/session update.
// ---------------------------------------------------------------------------

#[test]
fn promotion_with_captured_session_commits_run_card_and_identity_together() {
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "codex capture promote".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&enqueue_codex(card.id, card.column_id, None))
        .unwrap();
    assert_eq!(run.session_id, None);

    // One UOW: run started + workspace/pane + session id on the run AND the
    // card's running state + session id — no intermediate state is visible.
    let promoted = db
        .promote_run_with_anchor_uow(
            run.id,
            Some("workspace"),
            Some("pane"),
            Some("anchor"),
            Some(1_000),
            Some("thread-captured"),
        )
        .unwrap();
    assert!(promoted.started_at.is_some());
    assert_eq!(promoted.herdr_workspace_id.as_deref(), Some("workspace"));
    assert_eq!(promoted.herdr_pane_id.as_deref(), Some("pane"));
    assert_eq!(promoted.herdr_anchor_pane_id.as_deref(), Some("anchor"));
    assert_eq!(
        promoted.session_id.as_deref(),
        Some("thread-captured"),
        "the captured id replaces the mint NULL on the run"
    );
    let card = db.get_card(card.id).unwrap().unwrap();
    assert_eq!(card.status, CardStatus::Running);
    assert_eq!(
        card.session_id.as_deref(),
        Some("thread-captured"),
        "run and card must never disagree about the conversation id"
    );
}

#[test]
fn promotion_without_capture_keeps_the_enqueue_time_session_id() {
    // A fork whose NEW thread id was never captured degrades to the recorded
    // source id: COALESCE keeps it instead of wiping identity.
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "codex fork no capture".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&enqueue_codex(card.id, card.column_id, Some("thread-1")))
        .unwrap();
    let promoted = db
        .promote_run_with_anchor_uow(run.id, None, None, None, None, None)
        .unwrap();
    assert_eq!(promoted.session_id.as_deref(), Some("thread-1"));
    assert_eq!(
        db.get_card(card.id).unwrap().unwrap().session_id.as_deref(),
        Some("thread-1")
    );
}

#[test]
fn promotion_with_captured_session_replaces_a_prior_enqueue_id_on_both_rows() {
    // Fork with a captured NEW thread id: the source id must survive nowhere.
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "codex fork captured".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&enqueue_codex(card.id, card.column_id, Some("thread-1")))
        .unwrap();
    let promoted = db
        .promote_run_with_anchor_uow(run.id, None, None, None, None, Some("thread-new"))
        .unwrap();
    assert_eq!(promoted.session_id.as_deref(), Some("thread-new"));
    assert_eq!(
        db.get_card(card.id).unwrap().unwrap().session_id.as_deref(),
        Some("thread-new")
    );
    assert!(!db
        .get_run(run.id)
        .unwrap()
        .session_id
        .as_deref()
        .is_some_and(|id| id == "thread-1"));
}

#[test]
fn captured_session_promotion_works_after_run_started() {
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "codex started".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&enqueue_codex(card.id, card.column_id, None))
        .unwrap();
    db.promote_run_uow(run.id, Some("workspace"), Some("pane"), None)
        .unwrap();
    let promoted = db
        .promote_captured_session_uow(run.id, "thread-late")
        .unwrap();
    assert_eq!(promoted.session_id.as_deref(), Some("thread-late"));
    assert_eq!(promoted.herdr_pane_id.as_deref(), Some("pane"));
    assert_eq!(
        db.get_card(card.id).unwrap().unwrap().session_id.as_deref(),
        Some("thread-late")
    );
}

#[test]
fn captured_session_promotion_rejects_ended_run_without_touching_identity() {
    // The cancel-during-spawn race: the run ended while the capture was in
    // flight, so persisting the captured id on a dead run must fail closed and
    // leave both rows untouched.
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "codex cancelled".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&enqueue_codex(card.id, card.column_id, None))
        .unwrap();
    db.finalize_run_uow(&FinalizeRun {
        run_id: run.id,
        outcome: RunOutcome::Cancelled,
        summary: None,
        comments: &[],
        target_column_id: None,
        final_status: CardStatus::Idle,
        final_awaiting_reason: None,
        next: None,
    })
    .unwrap();

    let err = db
        .promote_captured_session_uow(run.id, "thread-late")
        .unwrap_err();
    assert!(err.to_string().contains("not open"), "message: {err}");
    assert_eq!(db.get_run(run.id).unwrap().session_id, None);
    assert_eq!(db.get_card(card.id).unwrap().unwrap().session_id, None);
    assert!(db.get_run(run.id).unwrap().ended_at.is_some());
}

#[test]
fn captured_session_promotion_rejects_blank_session_id() {
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "codex blank".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&enqueue_codex(card.id, card.column_id, None))
        .unwrap();
    for blank in ["", "   "] {
        assert!(db.promote_captured_session_uow(run.id, blank).is_err());
    }
    assert_eq!(db.get_run(run.id).unwrap().session_id, None);
    assert_eq!(db.get_card(card.id).unwrap().unwrap().session_id, None);
}

#[test]
fn captured_session_promotion_replaces_a_prior_resume_id_on_both_rows() {
    // Fork enqueues the source id; the fork's NEW thread id replaces it on
    // run and card atomically once the integration reports it.
    let db = mem();
    let card = db
        .create_card(&CardCreateParams {
            title: "codex fork".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&enqueue_codex(card.id, card.column_id, Some("thread-1")))
        .unwrap();
    assert_eq!(run.session_id.as_deref(), Some("thread-1"));

    let promoted = db
        .promote_captured_session_uow(run.id, "thread-new")
        .unwrap();
    assert_eq!(promoted.session_id.as_deref(), Some("thread-new"));
    assert_eq!(
        db.get_card(card.id).unwrap().unwrap().session_id.as_deref(),
        Some("thread-new")
    );
    // The prior id survives nowhere.
    assert!(!db
        .get_run(run.id)
        .unwrap()
        .session_id
        .as_deref()
        .is_some_and(|id| id == "thread-1"));
}
