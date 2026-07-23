use std::process::Command;

use board_core::client::BoardClient;
use board_core::protocol::{CardCreateParams, ColumnCreateParams};

use super::TestDaemon;

#[test]
fn cli_scopes_plain_cwds_and_preserves_global() {
    let td = TestDaemon::start(&[]);
    let one = td._dir.path().join("plain-one");
    let two = td._dir.path().join("plain-two");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();

    let created_one = td.board_in(&one, &["card", "new", "--title", "one", "--json"]);
    assert!(created_one.status.success(), "{:?}", created_one.stderr);
    let created_two = td.board_in(&two, &["card", "new", "--title", "two", "--json"]);
    assert!(created_two.status.success(), "{:?}", created_two.stderr);

    let listed_one = td.board_in(&one, &["card", "list", "--json"]);
    let cards_one: serde_json::Value = serde_json::from_slice(&listed_one.stdout).unwrap();
    assert_eq!(cards_one.as_array().unwrap().len(), 1);
    assert_eq!(cards_one[0]["title"], "one");
    let listed_two = td.board_in(&two, &["card", "list", "--json"]);
    let cards_two: serde_json::Value = serde_json::from_slice(&listed_two.stdout).unwrap();
    assert_eq!(cards_two.as_array().unwrap().len(), 1);
    assert_eq!(cards_two[0]["title"], "two");

    let mut client = td.client();
    assert!(client.board_get().unwrap().cards.is_empty());
    assert_eq!(client.board_list().unwrap().boards.len(), 3);
}

#[test]
fn cli_git_root_and_subdirectory_share_board() {
    let td = TestDaemon::start(&[]);
    let repo = td._dir.path().join("repo");
    let sub = repo.join("nested");
    std::fs::create_dir_all(&sub).unwrap();
    assert!(Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());

    let created = td.board_in(&repo, &["card", "new", "--title", "shared", "--json"]);
    assert!(created.status.success(), "{:?}", created.stderr);
    let listed = td.board_in(&sub, &["card", "list", "--json"]);
    assert!(listed.status.success(), "{:?}", listed.stderr);
    let cards: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(cards.as_array().unwrap().len(), 1);
    assert_eq!(cards[0]["title"], "shared");
    assert_eq!(td.client().board_list().unwrap().boards.len(), 2);
}

#[test]
fn move_resolves_column_in_cards_board_not_current_cwd() {
    let td = TestDaemon::start(&[]);
    let alpha_path = td._dir.path().join("alpha");
    let beta_path = td._dir.path().join("beta");
    std::fs::create_dir_all(&alpha_path).unwrap();
    std::fs::create_dir_all(&beta_path).unwrap();
    let alpha_path = alpha_path.canonicalize().unwrap();
    let beta_path = beta_path.canonicalize().unwrap();

    let mut client = td.client();
    let alpha = client
        .board_open(alpha_path.to_str().unwrap())
        .unwrap()
        .board;
    let beta = client
        .board_open(beta_path.to_str().unwrap())
        .unwrap()
        .board;
    let alpha_done = client
        .column_create(&ColumnCreateParams {
            board_id: Some(alpha.id),
            name: "Done".into(),
            ..Default::default()
        })
        .unwrap();
    let beta_done = client
        .column_create(&ColumnCreateParams {
            board_id: Some(beta.id),
            name: "Done".into(),
            ..Default::default()
        })
        .unwrap();
    let card = client
        .card_create(&CardCreateParams {
            board_id: Some(alpha.id),
            title: "move me".into(),
            ..Default::default()
        })
        .unwrap();

    let moved = td.board_in(
        &beta_path,
        &["move", &card.id.to_string(), "Done", "--json"],
    );
    assert!(moved.status.success(), "{:?}", moved.stderr);
    let moved: serde_json::Value = serde_json::from_slice(&moved.stdout).unwrap();
    assert_eq!(moved["column_id"], alpha_done.id);
    assert_ne!(moved["column_id"], beta_done.id);
}
