//! Canonical board and card verbs: list/show/open/rename, create/edit/move/
//! delete, nullable clears, visibility filtering, and delete confirmation.

use std::process::Output;

use board_core::client::BoardClient;
use board_core::protocol::{CardCreateParams, Effort};
use serde_json::Value;

use super::{json_output, old_card, TestDaemon};

fn json_card_ids(out: &Output) -> Vec<i64> {
    json_output(out)
        .as_array()
        .expect("card list JSON is an array")
        .iter()
        .map(|card| card["id"].as_i64().expect("card id"))
        .collect()
}

#[test]
fn board_commands_list_show_open_and_rename() {
    let td = TestDaemon::start(&[]);
    let project = td._dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project = project.canonicalize().unwrap();

    let mut client = td.client();
    let opened = client.board_open(project.to_str().unwrap()).unwrap().board;

    let listed = json_output(&td.board(&["board", "list", "--json"]));
    let boards = listed.as_array().expect("board list JSON is an array");
    assert!(boards.iter().any(|board| board["id"] == opened.id));

    let shown = json_output(&td.board(&["board", "show", &opened.id.to_string(), "--json"]));
    assert_eq!(shown["board"]["id"], opened.id);

    let reopened = json_output(&td.board(&["board", "open", project.to_str().unwrap(), "--json"]));
    assert_eq!(reopened["board"]["id"], opened.id);

    let renamed = json_output(&td.board(&[
        "board",
        "rename",
        &opened.id.to_string(),
        "Project board",
        "--json",
    ]));
    assert_eq!(renamed["name"], "Project board");
}

#[test]
fn global_board_selector_accepts_id_and_path() {
    let td = TestDaemon::start(&[]);
    let project = td._dir.path().join("selected-project");
    std::fs::create_dir_all(&project).unwrap();
    let project = project.canonicalize().unwrap();
    let mut client = td.client();
    let board = client.board_open(project.to_str().unwrap()).unwrap().board;
    let card = client
        .card_create(&CardCreateParams {
            board_id: Some(board.id),
            title: "selected card".into(),
            ..Default::default()
        })
        .unwrap();

    let by_id = td.board(&["--board", &board.id.to_string(), "card", "list", "--json"]);
    let by_id = json_output(&by_id);
    assert_eq!(by_id.as_array().unwrap().len(), 1);
    assert_eq!(by_id[0]["id"], card.id);

    let by_path = td.board_in(
        td._dir.path(),
        &[
            "--board",
            project.to_str().unwrap(),
            "card",
            "list",
            "--json",
        ],
    );
    let by_path = json_output(&by_path);
    assert_eq!(by_path.as_array().unwrap().len(), 1);
    assert_eq!(by_path[0]["id"], card.id);
}

#[test]
fn canonical_card_create_edit_move_and_delete() {
    let td = TestDaemon::start(&[]);

    let created = json_output(&td.board(&[
        "card",
        "create",
        "--title",
        "canonical card",
        "--description",
        "initial description",
        "--harness",
        "fake",
        "--json",
    ]));
    let id = created["id"].as_i64().expect("created card id");
    assert_eq!(created["title"], "canonical card");

    let edited = json_output(&td.board(&[
        "card",
        "edit",
        &id.to_string(),
        "--title",
        "edited card",
        "--description",
        "edited description",
        "--json",
    ]));
    assert_eq!(edited["title"], "edited card");
    assert_eq!(edited["description"], "edited description");

    // Move to a column the card is not already in, so the destination is
    // actually observable on the moved card and in the daemon's state.
    let destination = json_output(&td.board(&[
        "column",
        "create",
        "--name",
        "Canonical destination",
        "--trigger",
        "manual",
        "--json",
    ]));
    let destination_id = destination["id"].as_i64().expect("destination column id");
    assert_ne!(
        created["column_id"].as_i64(),
        Some(destination_id),
        "the card must start outside the destination column"
    );

    let moved = json_output(&td.board(&[
        "card",
        "move",
        &id.to_string(),
        "Canonical destination",
        "--json",
    ]));
    assert_eq!(moved["id"], id);
    assert_eq!(moved["column_id"], destination_id);
    assert_eq!(
        td.client().card_get(id).unwrap().card.column_id,
        destination_id,
        "the move must be persisted, not just echoed"
    );

    let deleted = json_output(&td.board(&["card", "delete", &id.to_string(), "--yes", "--json"]));
    assert_eq!(deleted["deleted"], true);
    assert!(td.client().card_get(id).is_err());
}

#[test]
fn card_edit_sets_and_explicitly_clears_nullable_patches() {
    let td = TestDaemon::start(&[]);
    let card = td
        .client()
        .card_create(&CardCreateParams {
            title: "patch card".into(),
            harness: Some("claude".into()),
            model: Some("sonnet".into()),
            effort: Some(Effort::Low),
            permission_mode: Some("acceptEdits".into()),
            session: Some("initial-session".into()),
            ..Default::default()
        })
        .unwrap();

    let set = json_output(&td.board(&[
        "card",
        "edit",
        &card.id.to_string(),
        "--title",
        "patched card",
        "--description",
        "patched description",
        "--model",
        "haiku",
        "--effort",
        "high",
        "--permission",
        "plan",
        "--session",
        "review-session",
        "--json",
    ]));
    assert_eq!(set["title"], "patched card");
    assert_eq!(set["description"], "patched description");
    assert_eq!(set["model"], "haiku");
    assert_eq!(set["effort"], "high");
    assert_eq!(set["permission_mode"], "plan");
    assert_eq!(set["session"], "review-session");

    let cleared = json_output(&td.board(&[
        "card",
        "edit",
        &card.id.to_string(),
        "--clear-model",
        "--clear-effort",
        "--clear-permission",
        "--clear-session",
        "--json",
    ]));
    assert_eq!(cleared["model"], Value::Null);
    assert_eq!(cleared["effort"], Value::Null);
    assert_eq!(cleared["permission_mode"], Value::Null);
    assert_eq!(cleared["session"], Value::Null);
    assert_eq!(cleared["title"], "patched card");
    assert_eq!(cleared["description"], "patched description");
}

#[test]
fn card_list_visibility_is_active_all_or_archived() {
    let td = TestDaemon::start(&[]);
    let active = old_card(&td, "active");
    let archived = old_card(&td, "archived");
    td.client().card_archive(archived, true).unwrap();

    let active_ids =
        json_card_ids(&td.board(&["card", "list", "--visibility", "active", "--json"]));
    assert!(active_ids.contains(&active));
    assert!(!active_ids.contains(&archived));

    let all_ids = json_card_ids(&td.board(&["card", "list", "--visibility", "all", "--json"]));
    assert!(all_ids.contains(&active));
    assert!(all_ids.contains(&archived));

    let archived_ids =
        json_card_ids(&td.board(&["card", "list", "--visibility", "archived", "--json"]));
    assert!(!archived_ids.contains(&active));
    assert_eq!(archived_ids, vec![archived]);
}

#[test]
fn card_delete_requires_yes_when_stdin_is_not_a_tty() {
    let td = TestDaemon::start(&[]);
    let id = old_card(&td, "must confirm");

    let refused = td.board(&["card", "delete", &id.to_string()]);
    assert!(
        !refused.status.success(),
        "non-TTY delete must require --yes"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("--yes")
            || String::from_utf8_lossy(&refused.stdout).contains("--yes")
    );
    assert!(
        td.client().card_get(id).is_ok(),
        "refused delete is non-mutating"
    );

    let deleted = json_output(&td.board(&["card", "delete", &id.to_string(), "--yes", "--json"]));
    assert_eq!(deleted["deleted"], true);
}
