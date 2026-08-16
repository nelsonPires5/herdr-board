//! FakeBoardClient in-memory state machine (feature `fake-client`).
#![cfg(feature = "fake-client")]

use board_core::client::{BoardClient, FakeBoardClient};
use board_core::config::{Config, HarnessDef};
use board_core::db::{EnqueueRun, FinalizeRun};
use board_core::launch::{ExecutionSpec, RunLaunchSpec};
use board_core::protocol::{
    AwaitingReason, CardCreateParams, CardMoveParams, CardStatus, ColumnCreateParams,
    RunFocusAction, RunOutcome, Trigger,
};

#[test]
fn fake_seeds_board_and_supports_crud() {
    let mut c = FakeBoardClient::new().unwrap();
    let snap = c.board_get().unwrap();
    assert_eq!(snap.board.name, "Global");
    assert_eq!(snap.columns.len(), 1);
    assert_eq!(snap.columns[0].name, "Todo");
    assert!(snap.cards.is_empty());

    let plan = c
        .column_create(&ColumnCreateParams {
            name: "Plan".into(),
            trigger: Some(Trigger::Auto),
            ..Default::default()
        })
        .unwrap();
    let card = c
        .card_create(&CardCreateParams {
            title: "Fix bug".into(),
            ..Default::default()
        })
        .unwrap();

    // Move into the auto column: fake just moves (no dispatch), status stays idle.
    let moved = c
        .card_move(&CardMoveParams {
            id: card.id,
            column_id: plan.id,
            board_id: None,
            position: None,
        })
        .unwrap();
    assert_eq!(moved.column_id, plan.id);

    c.comment_add(card.id, "hello", Some("user")).unwrap();
    let detail = c.card_get(card.id).unwrap();
    assert_eq!(detail.comments.len(), 1);
    assert_eq!(detail.comments[0].body, "hello");
    assert!(detail.runs.is_empty());

    let snap = c.board_get().unwrap();
    assert_eq!(snap.columns.len(), 2);
    assert_eq!(snap.cards.len(), 1);
}

#[test]
fn fake_supports_scoped_board_open_list_and_get() {
    let mut c = FakeBoardClient::new().unwrap();
    let alpha = c.board_open("/alpha/project").unwrap();
    let same = c.board_open("/alpha/project").unwrap();
    let beta = c.board_open("/beta/project").unwrap();
    assert_eq!(alpha.board.id, same.board.id);
    assert_ne!(alpha.board.id, beta.board.id);

    c.card_create(&CardCreateParams {
        board_id: Some(alpha.board.id),
        title: "alpha".into(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(c.board_get_by_id(alpha.board.id).unwrap().cards.len(), 1);
    assert!(c.board_get_by_id(beta.board.id).unwrap().cards.is_empty());
    let boards = c.board_list().unwrap().boards;
    assert_eq!(boards[0].name, "Global");
    assert_eq!(boards.len(), 3);
}

#[test]
fn fake_run_focus_targets_the_exact_requested_run() {
    let mut c = FakeBoardClient::new().unwrap();
    let card = c
        .card_create(&CardCreateParams {
            title: "focus".into(),
            ..Default::default()
        })
        .unwrap();
    let older = c
        .db()
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
    c.db()
        .promote_run_uow(older.id, Some("w"), Some("p-old"), None)
        .unwrap();
    c.db()
        .finalize_run_uow(&FinalizeRun {
            run_id: older.id,
            outcome: RunOutcome::Ok,
            summary: None,
            comments: &[],
            target_column_id: None,
            final_status: CardStatus::Done,
            final_awaiting_reason: None,
            next: None,
        })
        .unwrap();
    let latest = c
        .db()
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
    c.db()
        .promote_run_uow(latest.id, Some("w"), Some("p-new"), None)
        .unwrap();

    // The requested run is focused — including the older, already-ended one.
    let focused = c.run_focus(card.id, latest.id, "/tmp/herdr.sock").unwrap();
    assert_eq!(focused.run_id, latest.id);
    assert_eq!(focused.pane_id, "p-new");
    assert_eq!(focused.card_id, card.id);
    assert_eq!(focused.column_id, card.column_id);
    assert_eq!(focused.harness, "pi");
    assert_eq!(focused.session, None);
    assert_eq!(focused.session_id, None);

    let historical = c.run_focus(card.id, older.id, "/tmp/herdr.sock").unwrap();
    assert_eq!(historical.run_id, older.id);
    assert_eq!(historical.pane_id, "p-old");

    let no_pane = c
        .card_create(&CardCreateParams {
            title: "none".into(),
            ..Default::default()
        })
        .unwrap();
    // Unknown run id for a card with no runs at all.
    assert!(c.run_focus(no_pane.id, 1234, "/tmp/herdr.sock").is_err());
    // A real run id that belongs to a *different* card is rejected.
    assert!(c
        .run_focus(no_pane.id, latest.id, "/tmp/herdr.sock")
        .is_err());
    // A run with no recorded pane and no conversation id has nothing to focus
    // and nothing to resume.
    let queued = c
        .db()
        .enqueue_run_uow(&EnqueueRun {
            card_id: no_pane.id,
            column_id: no_pane.column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "p",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    assert!(c
        .run_focus(no_pane.id, queued.id, "/tmp/herdr.sock")
        .is_err());
    // A focused recorded pane is reported as exactly that.
    assert_eq!(focused.action, RunFocusAction::FocusedRecordedPane);
    assert_eq!(focused.recorded_pane_id.as_deref(), Some("p-new"));
}

/// The fake has no Herdr, so it cannot create a pane. What it *can* model
/// honestly is the rescue **decision**, which depends only on the run row and
/// the harness config.
#[test]
fn fake_run_focus_models_the_rescue_decision_without_faking_herdr() {
    /// `workspace` / `spec` are separately controllable so the fake's refusals
    /// can be compared against the daemon's one precondition at a time.
    fn seed_full(
        c: &FakeBoardClient,
        harness: &str,
        session_id: Option<&str>,
        spec: bool,
        workspace: Option<&str>,
    ) -> (i64, i64) {
        let card = c
            .db()
            .create_card(&CardCreateParams {
                title: "rescue".into(),
                harness: Some(harness.into()),
                ..Default::default()
            })
            .unwrap();
        let launch_spec = spec.then(|| {
            serde_json::to_string(&RunLaunchSpec::v1(ExecutionSpec {
                argv: vec![harness.to_string(), "--model".into(), "m".into()],
                env: vec![],
                agent_kind: None,
                initial_prompt: Some("p".into()),
                system_prompt: Some("s".into()),
            }))
            .unwrap()
        });
        let run = c
            .db()
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness,
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: launch_spec.as_deref(),
                session_id,
                session: None,
            })
            .unwrap();
        // Promoted with a workspace but NO pane: the dead-pane shape.
        c.db()
            .promote_run_uow(run.id, workspace, None, None)
            .unwrap();
        (card.id, run.id)
    }

    fn seed(c: &FakeBoardClient, harness: &str, session_id: Option<&str>) -> (i64, i64) {
        seed_full(c, harness, session_id, true, Some("w"))
    }

    let mut c = FakeBoardClient::new().unwrap();
    // pi/claude can resume, so a run with a conversation id would be rescued.
    let (card_id, run_id) = seed(&c, "pi", Some("conv-1"));
    let result = c.run_focus(card_id, run_id, "/tmp/herdr.sock").unwrap();
    assert_eq!(result.action, RunFocusAction::Rescued);
    assert_eq!(result.recorded_pane_id, None);
    assert_eq!(result.session_id.as_deref(), Some("conv-1"));
    // No invented pane id: the fake never pretends to have talked to Herdr.
    assert_eq!(result.pane_id, "(would-rescue)");

    // No conversation id ⇒ refused, not rescued.
    let (card_id, run_id) = seed(&c, "pi", None);
    let err = c
        .run_focus(card_id, run_id, "/tmp/herdr.sock")
        .unwrap_err()
        .to_string();
    assert!(err.contains("conversation id"), "message: {err}");

    // A config-defined harness that did not opt in ⇒ refused, naming it.
    let mut config = Config::default();
    config
        .harness
        .insert("custom".into(), HarnessDef::default());
    let mut c = FakeBoardClient::new().unwrap().with_config(config.clone());
    let (card_id, run_id) = seed(&c, "custom", Some("conv-1"));
    let err = c
        .run_focus(card_id, run_id, "/tmp/herdr.sock")
        .unwrap_err()
        .to_string();
    assert!(err.contains("custom"), "message: {err}");
    assert!(err.contains("resum"), "message: {err}");

    // Mirrors the daemon exactly: a pre-v11 row has no execution to resume...
    let mut c = FakeBoardClient::new().unwrap();
    let (card_id, run_id) = seed_full(&c, "pi", Some("conv-1"), false, Some("w"));
    let err = c
        .run_focus(card_id, run_id, "/tmp/herdr.sock")
        .unwrap_err()
        .to_string();
    assert!(err.contains("durable launch specs"), "message: {err}");
    // ...and a run with no recorded workspace has nowhere to put a pane.
    let (card_id, run_id) = seed_full(&c, "pi", Some("conv-1"), true, None);
    let err = c
        .run_focus(card_id, run_id, "/tmp/herdr.sock")
        .unwrap_err()
        .to_string();
    assert!(err.contains("no Herdr workspace"), "message: {err}");

    // The same harness with `resume = true` is accepted.
    config.harness.insert(
        "custom".into(),
        HarnessDef {
            resume: true,
            ..Default::default()
        },
    );
    let mut c = FakeBoardClient::new().unwrap().with_config(config);
    let (card_id, run_id) = seed(&c, "custom", Some("conv-1"));
    let result = c.run_focus(card_id, run_id, "/tmp/herdr.sock").unwrap();
    assert_eq!(result.action, RunFocusAction::Rescued);
}

#[test]
fn fake_run_done_applies_the_real_transition_decision() {
    let mut c = FakeBoardClient::new().unwrap();
    let card = c
        .card_create(&CardCreateParams {
            title: "confirm".into(),
            ..Default::default()
        })
        .unwrap();
    let run = c
        .db()
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
    c.db()
        .promote_run_uow(run.id, Some("w"), Some("p"), None)
        .unwrap();
    c.db()
        .set_card_awaiting(card.id, AwaitingReason::AgentDone)
        .unwrap();

    let result = c.run_done(card.id, RunOutcome::Ok, None).unwrap();

    assert_eq!(result.run.id, run.id);
    assert_eq!(result.run.outcome, Some(RunOutcome::Ok));
    assert_eq!(result.card.status, CardStatus::Done);
    assert_eq!(result.card.awaiting_reason, None);
    assert!(c
        .card_get(card.id)
        .unwrap()
        .comments
        .iter()
        .any(|comment| comment.author == "system" && comment.body.contains("no target column")));
}

#[test]
fn fake_delete_column_with_active_card_refused() {
    let mut c = FakeBoardClient::new().unwrap();
    let col = c
        .column_create(&ColumnCreateParams {
            name: "WIP".into(),
            ..Default::default()
        })
        .unwrap();
    let card = c
        .card_create(&CardCreateParams {
            title: "T".into(),
            column_id: Some(col.id),
            ..Default::default()
        })
        .unwrap();

    // Empty column with no move target: still fine if it has no cards, but here it has one.
    let err = c.column_delete(col.id, None).unwrap_err();
    assert!(err.to_string().contains("cards"));

    // With a move target it succeeds.
    let todo = c.board_get().unwrap().columns[0].id;
    let _ = card;
    assert!(c.column_delete(col.id, Some(todo)).unwrap().deleted);
}

#[test]
fn fake_comment_actor_authorization_matches_daemon() {
    let mut c = FakeBoardClient::new().unwrap();
    let card = c
        .card_create(&CardCreateParams {
            title: "owned card".into(),
            ..Default::default()
        })
        .unwrap();
    let other_card = c
        .card_create(&CardCreateParams {
            title: "other card".into(),
            ..Default::default()
        })
        .unwrap();
    let enqueue = |c: &FakeBoardClient, card_id: i64| {
        let card = c.db().get_card(card_id).unwrap().unwrap();
        c.db()
            .enqueue_run_uow(&EnqueueRun {
                card_id,
                column_id: card.column_id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "prompt",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap()
            .id
    };
    let own_run = enqueue(&c, card.id);
    let other_run = enqueue(&c, other_card.id);

    let own_author = format!("agent:{own_run}");
    let own_comment = c.comment_add(card.id, "owned", Some(&own_author)).unwrap();
    assert_eq!(
        c.comment_update(own_comment.id, "edited by owner", Some(own_run))
            .unwrap()
            .body,
        "edited by owner"
    );
    assert!(c
        .comment_update(own_comment.id, "edited by other", Some(other_run))
        .is_err());
    assert!(c.comment_delete(own_comment.id, Some(other_run)).is_err());
    assert!(
        c.comment_delete(own_comment.id, Some(own_run))
            .unwrap()
            .deleted
    );

    // A durable run must also belong to the comment's card, not merely match
    // the author string.
    let other_author = format!("agent:{other_run}");
    let forged = c
        .comment_add(card.id, "forged", Some(&other_author))
        .unwrap();
    assert!(c
        .comment_update(forged.id, "must be rejected", Some(other_run))
        .is_err());
    assert!(c.comment_delete(forged.id, Some(other_run)).is_err());

    // Human callers may mutate non-system comments without an actor run.
    let human = c.comment_add(card.id, "human", Some("user")).unwrap();
    assert!(c.comment_update(human.id, "human edit", None).is_ok());
    assert!(c.comment_delete(human.id, None).unwrap().deleted);

    let system = c.comment_add(card.id, "system", Some("system")).unwrap();
    assert!(c
        .comment_update(system.id, "must be immutable", None)
        .is_err());
    assert!(c.comment_delete(system.id, None).is_err());
}

#[test]
fn fake_agent_comments_keep_no_run_harness_compatibility() {
    let mut c = FakeBoardClient::new().unwrap();
    let card = c
        .card_create(&CardCreateParams {
            title: "fake harness".into(),
            harness: Some("fake".into()),
            ..Default::default()
        })
        .unwrap();
    let comment = c
        .comment_add(card.id, "fake output", Some("agent:12345"))
        .unwrap();

    assert!(c
        .comment_update(comment.id, "revised fake output", Some(12345))
        .is_ok());
    assert!(c.comment_delete(comment.id, Some(12345)).is_ok());
}

#[test]
fn fake_card_move_transfers_across_boards() {
    let mut c = FakeBoardClient::new().unwrap();
    let alpha = c.board_open("/alpha").unwrap();
    let beta = c.board_open("/beta").unwrap();
    let beta_done = c
        .column_create(&ColumnCreateParams {
            board_id: Some(beta.board.id),
            name: "Done".into(),
            ..Default::default()
        })
        .unwrap();
    let card = c
        .card_create(&CardCreateParams {
            board_id: Some(alpha.board.id),
            title: "ship".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(card.board_id, alpha.board.id);

    let moved = c
        .card_transfer(card.id, beta.board.id, beta_done.id, None)
        .unwrap();
    assert_eq!(moved.board_id, beta.board.id);
    assert_eq!(moved.column_id, beta_done.id);

    // The typed wrapper serializes board_id; the card now belongs to beta.
    assert!(c.board_get_by_id(alpha.board.id).unwrap().cards.is_empty());
    assert_eq!(c.board_get_by_id(beta.board.id).unwrap().cards.len(), 1);

    // A cross-board move lying about the board (beta column, declared alpha)
    // is rejected by the fake's underlying transfer_card.
    let err = c
        .card_move(&CardMoveParams {
            id: moved.id,
            column_id: beta_done.id,
            board_id: Some(alpha.board.id),
            position: None,
        })
        .unwrap_err();
    assert!(err.to_string().contains("belongs to board"));
}
