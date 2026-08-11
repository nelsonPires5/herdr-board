use std::process::Command;

use board_core::client::BoardClient;
use board_core::protocol::{CardCreateParams, ColumnCreateParams};

use super::{json_output, TestDaemon};

#[test]
fn cli_scopes_plain_cwds_and_preserves_global() {
    let td = TestDaemon::start(&[]);
    let one = td._dir.path().join("plain-one");
    let two = td._dir.path().join("plain-two");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();

    json_output(&td.board_in(&one, &["card", "new", "--title", "one", "--json"]));
    json_output(&td.board_in(&two, &["card", "new", "--title", "two", "--json"]));

    let cards_one = json_output(&td.board_in(&one, &["card", "list", "--json"]));
    assert_eq!(cards_one.as_array().unwrap().len(), 1);
    assert_eq!(cards_one[0]["title"], "one");
    let cards_two = json_output(&td.board_in(&two, &["card", "list", "--json"]));
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

    json_output(&td.board_in(&repo, &["card", "new", "--title", "shared", "--json"]));
    let cards = json_output(&td.board_in(&sub, &["card", "list", "--json"]));
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

    let out = td.board_in(
        &beta_path,
        &["move", &card.id.to_string(), "Done", "--json"],
    );
    let moved = json_output(&out);
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "a move with no board selector must not warn"
    );
    assert_eq!(moved["column_id"], alpha_done.id);
    assert_ne!(moved["column_id"], beta_done.id);
}

/// D3: `--destination-board` is the explicit cross-board spelling. The global
/// `--board` selector — which everywhere else means "which board to read" —
/// still works as a destination, but says so on stderr.
#[test]
fn cross_board_move_prefers_destination_board_and_deprecates_the_selector() {
    let td = TestDaemon::start(&[]);
    let alpha_path = td._dir.path().join("alpha-move");
    let beta_path = td._dir.path().join("beta-move");
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
    let beta_done = client
        .column_create(&ColumnCreateParams {
            board_id: Some(beta.id),
            name: "Done".into(),
            ..Default::default()
        })
        .unwrap();
    let explicit = client
        .card_create(&CardCreateParams {
            board_id: Some(alpha.id),
            title: "explicit destination".into(),
            ..Default::default()
        })
        .unwrap();
    let fallback = client
        .card_create(&CardCreateParams {
            board_id: Some(alpha.id),
            title: "selector fallback".into(),
            ..Default::default()
        })
        .unwrap();
    let same_board = client
        .card_create(&CardCreateParams {
            board_id: Some(beta.id),
            title: "same-board selector".into(),
            ..Default::default()
        })
        .unwrap();

    let out = td.board(&[
        "move",
        &explicit.id.to_string(),
        "Done",
        "--destination-board",
        &beta.id.to_string(),
        "--json",
    ]);
    let moved = json_output(&out);
    assert_eq!(moved["column_id"], beta_done.id);
    assert_eq!(moved["board_id"], beta.id);
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "the explicit spelling must not warn"
    );

    let out = td.board(&[
        "--board",
        &beta.id.to_string(),
        "move",
        &same_board.id.to_string(),
        "Done",
        "--json",
    ]);
    let moved = json_output(&out);
    assert_eq!(moved["column_id"], beta_done.id);
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "the global selector must stay quiet when the card is already on that board"
    );

    let out = td.board(&[
        "--board",
        &beta.id.to_string(),
        "move",
        &fallback.id.to_string(),
        "Done",
        "--json",
    ]);
    let moved = json_output(&out);
    assert_eq!(moved["column_id"], beta_done.id, "the fallback still moves");
    let warning = String::from_utf8_lossy(&out.stderr);
    assert!(
        warning.contains("deprecated") && warning.contains("--destination-board"),
        "the fallback must warn: {warning}"
    );
}
