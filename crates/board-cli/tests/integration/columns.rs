//! Canonical `column` verbs: create/edit/reorder/delete, every setting and its
//! explicit clear, and the board-locality rule for transition targets.

use board_core::client::BoardClient;
use board_core::protocol::{CardCreateParams, ColumnCreateParams, Trigger};
use serde_json::Value;

use super::{col, json_error, json_output, TestDaemon};

#[test]
fn canonical_column_create_edit_reorder_and_delete_with_destination() {
    let td = TestDaemon::start(&[]);

    let created = json_output(&td.board(&[
        "column",
        "create",
        "--name",
        "Canonical review",
        "--trigger",
        "manual",
        "--json",
    ]));
    let id = created["id"].as_i64().expect("column id");

    let edited = json_output(&td.board(&[
        "column",
        "edit",
        &id.to_string(),
        "--name",
        "Edited review",
        "--json",
    ]));
    assert_eq!(edited["name"], "Edited review");

    // Reorder to the head: the board was seeded with `Todo` at position 0, so
    // the response must report the edited column first and `Todo` shifted down.
    let reordered = json_output(&td.board(&["column", "reorder", &id.to_string(), "0", "--json"]));
    let order: Vec<(i64, &str, i64)> = reordered
        .as_array()
        .expect("reorder returns the new column order")
        .iter()
        .map(|column| {
            (
                column["id"].as_i64().expect("column id"),
                column["name"].as_str().expect("column name"),
                column["position"].as_i64().expect("column position"),
            )
        })
        .collect();
    let names_and_positions: Vec<(&str, i64)> = order
        .iter()
        .map(|(_, name, position)| (*name, *position))
        .collect();
    assert_eq!(
        names_and_positions,
        vec![("Edited review", 0), ("Todo", 1)],
        "reorder must place the moved column at position 0 and compact the rest"
    );
    assert_eq!(
        order[0].0, id,
        "position 0 is the column that was reordered"
    );
    let todo_id = order[1].0;

    // The new order is persisted, not just reported by the reorder response.
    let listed = json_output(&td.board(&["column", "list", "--json"]));
    let persisted: Vec<(i64, i64)> = listed
        .as_array()
        .expect("column list is an array")
        .iter()
        .map(|column| {
            (
                column["id"].as_i64().expect("column id"),
                column["position"].as_i64().expect("column position"),
            )
        })
        .collect();
    assert_eq!(persisted, vec![(id, 0), (todo_id, 1)]);

    let mut client = td.client();
    let scope_path = td._dir.path().canonicalize().unwrap();
    let board_id = client
        .board_open(scope_path.to_str().unwrap())
        .unwrap()
        .board
        .id;
    let mut source_params = col("source", Trigger::Manual);
    source_params.board_id = Some(board_id);
    let source = client.column_create(&source_params).unwrap();
    let mut destination_params = col("destination", Trigger::Manual);
    destination_params.board_id = Some(board_id);
    let destination = client.column_create(&destination_params).unwrap();
    let card = client
        .card_create(&CardCreateParams {
            board_id: Some(board_id),
            title: "move before delete".into(),
            column_id: Some(source.id),
            ..Default::default()
        })
        .unwrap();
    let delete_output = td.board(&[
        "column",
        "delete",
        &source.id.to_string(),
        "--move-cards-to",
        &destination.id.to_string(),
        "--yes",
        "--json",
    ]);
    let deleted = json_output(&delete_output);
    assert_eq!(deleted["deleted"], true);
    assert_eq!(
        td.client().card_get(card.id).unwrap().card.column_id,
        destination.id
    );
}

#[test]
fn column_transition_targets_must_belong_to_the_current_board() {
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
    let alpha_source = client
        .column_create(&ColumnCreateParams {
            board_id: Some(alpha.id),
            name: "alpha source".into(),
            ..Default::default()
        })
        .unwrap();
    let beta_target = client
        .column_create(&ColumnCreateParams {
            board_id: Some(beta.id),
            name: "beta target".into(),
            ..Default::default()
        })
        .unwrap();
    let rejected_create = td.board_in(
        &alpha_path,
        &[
            "column",
            "create",
            "--name",
            "must not exist",
            "--on-success",
            &beta_target.id.to_string(),
            "--json",
        ],
    );
    assert!(!rejected_create.status.success());
    json_error(&rejected_create);

    let alpha_after_create = client.board_get_by_id(alpha.id).unwrap();
    assert!(!alpha_after_create
        .columns
        .iter()
        .any(|column| column.name == "must not exist"));
    let beta_after_create = client.board_get_by_id(beta.id).unwrap();
    assert!(!beta_after_create
        .columns
        .iter()
        .any(|column| column.name == "must not exist"));

    let rejected_edit = td.board_in(
        &alpha_path,
        &[
            "column",
            "edit",
            &alpha_source.id.to_string(),
            "--on-success",
            &beta_target.id.to_string(),
            "--json",
        ],
    );
    assert!(!rejected_edit.status.success());
    json_error(&rejected_edit);

    let alpha_after_edit = client.board_get_by_id(alpha.id).unwrap();
    let source_after = alpha_after_edit
        .columns
        .iter()
        .find(|column| column.id == alpha_source.id)
        .unwrap();
    assert_eq!(source_after.on_success_column_id, None);
}

#[test]
fn column_edit_covers_all_settings_and_explicit_clears() {
    let td = TestDaemon::start(&[]);
    let mut client = td.client();
    let scope_path = td._dir.path().canonicalize().unwrap();
    let board_id = client
        .board_open(scope_path.to_str().unwrap())
        .unwrap()
        .board
        .id;
    let mut success_params = col("success target", Trigger::Manual);
    success_params.board_id = Some(board_id);
    let success = client.column_create(&success_params).unwrap();
    let mut failure_params = col("failure target", Trigger::Manual);
    failure_params.board_id = Some(board_id);
    let failure = client.column_create(&failure_params).unwrap();

    let created = json_output(&td.board(&[
        "column",
        "create",
        "--name",
        "Configured stage",
        "--prompt",
        "run the configured stage",
        "--trigger",
        "auto",
        "--on-success",
        &success.id.to_string(),
        "--on-fail",
        &failure.id.to_string(),
        "--harness",
        "claude",
        "--model",
        "sonnet",
        "--effort",
        "low",
        "--permission",
        "acceptEdits",
        "--timeout",
        "12",
        "--fresh-session",
        "--json",
    ]));
    let id = created["id"].as_i64().expect("configured column id");
    assert_eq!(created["system_prompt"], "run the configured stage");
    assert_eq!(created["trigger"], "auto");
    assert_eq!(created["on_success_column_id"], success.id);
    assert_eq!(created["on_fail_column_id"], failure.id);
    assert_eq!(created["harness_override"], "claude");
    assert_eq!(created["model_override"], "sonnet");
    assert_eq!(created["effort_override"], "low");
    assert_eq!(created["permission_override"], "acceptEdits");
    assert_eq!(created["timeout_minutes"], 12);
    assert_eq!(created["fresh_session"], true);

    let cleared = json_output(&td.board(&[
        "column",
        "edit",
        &id.to_string(),
        "--trigger",
        "manual",
        "--reuse-session",
        "--clear-prompt",
        "--clear-on-success",
        "--clear-on-fail",
        "--clear-harness",
        "--clear-model",
        "--clear-effort",
        "--clear-permission",
        "--clear-timeout",
        "--json",
    ]));
    assert_eq!(cleared["trigger"], "manual");
    assert_eq!(cleared["fresh_session"], false);
    assert_eq!(cleared["system_prompt"], Value::Null);
    assert_eq!(cleared["on_success_column_id"], Value::Null);
    assert_eq!(cleared["on_fail_column_id"], Value::Null);
    assert_eq!(cleared["harness_override"], Value::Null);
    assert_eq!(cleared["model_override"], Value::Null);
    assert_eq!(cleared["effort_override"], Value::Null);
    assert_eq!(cleared["permission_override"], Value::Null);
    assert_eq!(cleared["timeout_minutes"], Value::Null);
}
