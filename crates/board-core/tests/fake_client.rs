//! FakeBoardClient in-memory state machine (feature `fake-client`).
#![cfg(feature = "fake-client")]

use board_core::client::{BoardClient, FakeBoardClient};
use board_core::db::{EnqueueRun, FinalizeRun};
use board_core::protocol::{
    AwaitingReason, CardCreateParams, CardMoveParams, CardStatus, ColumnCreateParams, RunOutcome,
    Trigger,
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
fn fake_run_focus_uses_latest_recorded_pane() {
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

    let focused = c.run_focus(card.id, "/tmp/herdr.sock").unwrap();
    assert_eq!(focused.run_id, latest.id);
    assert_eq!(focused.pane_id, "p-new");

    let no_pane = c
        .card_create(&CardCreateParams {
            title: "none".into(),
            ..Default::default()
        })
        .unwrap();
    assert!(c.run_focus(no_pane.id, "/tmp/herdr.sock").is_err());
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
