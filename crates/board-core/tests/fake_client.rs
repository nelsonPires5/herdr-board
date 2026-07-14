//! FakeBoardClient in-memory state machine (feature `fake-client`).
#![cfg(feature = "fake-client")]

use board_core::client::{BoardClient, FakeBoardClient};
use board_core::protocol::{CardCreateParams, CardMoveParams, ColumnCreateParams, Trigger};

#[test]
fn fake_seeds_board_and_supports_crud() {
    let mut c = FakeBoardClient::new().unwrap();
    let snap = c.board_get().unwrap();
    assert_eq!(snap.board.name, "main");
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
