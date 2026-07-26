use board_core::client::BoardClient;
use board_core::protocol::{CardCreateParams, CardStatus, ColumnCreateParams, Trigger};

use super::{poll, todo_id, TestDaemon};

fn old_card(td: &TestDaemon, title: &str) -> i64 {
    let out = td.board(&[
        "card",
        "new",
        "--title",
        title,
        "--harness",
        "fake",
        "--json",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["id"]
        .as_i64()
        .unwrap()
}

fn assert_json_success(out: std::process::Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("legacy alias should emit JSON")
}

#[test]
fn top_level_comment_and_move_aliases_remain_supported() {
    let td = TestDaemon::start(&[]);
    let card_id = old_card(&td, "legacy aliases");

    let comment = assert_json_success(td.board(&[
        "comment",
        &card_id.to_string(),
        "legacy comment",
        "--json",
    ]));
    assert_eq!(comment["card_id"], card_id);
    assert_eq!(comment["body"], "legacy comment");

    let moved = assert_json_success(td.board(&["move", &card_id.to_string(), "Todo", "--json"]));
    assert_eq!(moved["id"], card_id);
}

#[test]
fn top_level_done_cancel_and_retry_aliases_remain_supported() {
    let td = TestDaemon::start(&[("FAKE_AGENT_SLEEP", "10")]);
    let mut client = td.client();
    let todo = todo_id(&mut client);
    let work = client
        .column_create(&ColumnCreateParams {
            name: "legacy-work".into(),
            trigger: Some(Trigger::Auto),
            ..Default::default()
        })
        .unwrap();
    let card = client
        .card_create(&CardCreateParams {
            title: "legacy run aliases".into(),
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

    let comment =
        assert_json_success(td.board(&["comment", &card.id.to_string(), "before done", "--json"]));
    assert_eq!(comment["card_id"], card.id);

    let done =
        assert_json_success(td.board(&["done", &card.id.to_string(), "--outcome", "ok", "--json"]));
    assert_eq!(done["card"]["id"], card.id);

    let retry = assert_json_success(td.board(&["retry", &card.id.to_string(), "--json"]));
    assert_eq!(retry["card"]["id"], card.id);
    assert!(poll(&mut client, 10, |c| {
        c.card_get(card.id).unwrap().runs.len() >= 2
    }));

    let cancel = assert_json_success(td.board(&["cancel", &card.id.to_string(), "--json"]));
    assert_eq!(cancel["card"]["id"], card.id);
}
