//! Canonical `card run` verbs against a live run: done, retry, and cancel.

use board_core::client::BoardClient;
use board_core::protocol::{CardCreateParams, CardStatus, ColumnCreateParams, Trigger};

use super::{json_output, poll, todo_id, TestDaemon};

#[test]
fn canonical_card_run_done_cancel_and_retry() {
    let td = TestDaemon::start(&[("FAKE_AGENT_SLEEP", "10")]);
    let mut client = td.client();
    let todo = todo_id(&mut client);
    let work = client
        .column_create(&ColumnCreateParams {
            name: "run-work".into(),
            trigger: Some(Trigger::Auto),
            ..Default::default()
        })
        .unwrap();
    let card = client
        .card_create(&CardCreateParams {
            title: "run card".into(),
            harness: Some("fake".into()),
            column_id: Some(todo),
            ..Default::default()
        })
        .unwrap();
    client
        .card_move(&board_core::protocol::CardMoveParams {
            id: card.id,
            column_id: work.id,
            board_id: None,
            position: None,
        })
        .unwrap();
    assert!(poll(&mut client, 10, |c| {
        c.card_get(card.id).unwrap().card.status == CardStatus::Running
    }));

    let done = json_output(&td.board(&[
        "card",
        "run",
        "done",
        &card.id.to_string(),
        "--outcome",
        "ok",
        "--json",
    ]));
    assert_eq!(done["card"]["id"], card.id);

    let retried = json_output(&td.board(&["card", "run", "retry", &card.id.to_string(), "--json"]));
    assert_eq!(retried["card"]["id"], card.id);

    assert!(poll(&mut client, 10, |c| {
        c.card_get(card.id).unwrap().runs.len() >= 2
    }));
    let cancelled =
        json_output(&td.board(&["card", "run", "cancel", &card.id.to_string(), "--json"]));
    assert_eq!(cancelled["card"]["id"], card.id);
}
