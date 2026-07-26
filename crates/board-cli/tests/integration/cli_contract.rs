use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Output, Stdio};

use board_core::client::BoardClient;
use board_core::protocol::{
    CardCreateParams, CardStatus, ColumnCreateParams, Effort, Request, Response, Trigger,
};
use serde_json::Value;

use super::{col, poll, todo_id, TestDaemon};

fn json_output(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("command should emit JSON")
}

fn json_error(out: &Output) -> Value {
    assert!(!out.status.success(), "command unexpectedly succeeded");
    assert!(out.stdout.is_empty(), "JSON errors must leave stdout empty");
    serde_json::from_slice(&out.stderr).expect("JSON error should be emitted on stderr")
}

#[test]
fn top_level_status_is_rejected() {
    let td = TestDaemon::start(&[]);
    let output = td.board(&["status"]);

    assert!(
        !output.status.success(),
        "top-level status unexpectedly succeeded"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("status"),
        "parse error should identify the rejected command: {:?}",
        output.stderr
    );
}

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
    let card = json_output(&out);
    card["id"].as_i64().expect("created card id")
}

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

    let moved = json_output(&td.board(&["card", "move", &id.to_string(), "Todo", "--json"]));
    assert_eq!(moved["id"], id);

    let deleted = json_output(&td.board(&["card", "delete", &id.to_string(), "--yes", "--json"]));
    assert_eq!(deleted["deleted"], true);
    assert!(!td.client().card_get(id).is_ok());
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
fn canonical_card_comment_lifecycle_has_history() {
    let td = TestDaemon::start(&[]);
    let card_id = old_card(&td, "commented");

    let added = json_output(&td.board(&[
        "card",
        "comment",
        "add",
        &card_id.to_string(),
        "first body",
        "--json",
    ]));
    let comment_id = added["id"].as_i64().expect("comment id");
    assert_eq!(added["body"], "first body");

    let shown =
        json_output(&td.board(&["card", "comment", "show", &comment_id.to_string(), "--json"]));
    assert_eq!(shown["id"], comment_id);
    assert_eq!(shown["body"], "first body");

    let edited = json_output(&td.board(&[
        "card",
        "comment",
        "edit",
        &comment_id.to_string(),
        "edited body",
        "--json",
    ]));
    assert_eq!(edited["body"], "edited body");

    let history = json_output(&td.board(&[
        "card",
        "comment",
        "history",
        &comment_id.to_string(),
        "--json",
    ]));
    let history = history.as_array().expect("comment history is an array");
    assert!(history.iter().any(|entry| entry["body"] == "first body"));
    assert!(history.iter().any(|entry| entry["body"] == "edited body"));

    let deleted = json_output(&td.board(&[
        "card",
        "comment",
        "delete",
        &comment_id.to_string(),
        "--yes",
        "--json",
    ]));
    assert_eq!(deleted["deleted"], true);
}

#[test]
fn nested_comment_json_errors_preserve_structured_rpc_fields() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("comment-error.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        // `board card ...` opens its command client before dispatching the
        // nested operation, so accept and close that unused connection first.
        let (first, _) = listener.accept().unwrap();
        drop(first);

        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: Request = serde_json::from_str(line.trim_end()).unwrap();

        let response = Response::err_with_details(
            request.id,
            73,
            Some("comment_forbidden"),
            "comment belongs to another run",
            Some(serde_json::json!({"comment_id": 41, "owner_run_id": 9})),
        );
        let mut wire = serde_json::to_string(&response).unwrap();
        wire.push('\n');
        stream.write_all(wire.as_bytes()).unwrap();
        stream.flush().unwrap();
    });

    let out = Command::new(super::BOARD_BIN)
        .args(["card", "comment", "edit", "41", "replacement", "--json"])
        .current_dir(dir.path())
        .env("BOARD_SOCKET", &socket)
        .env("BOARD_DB", dir.path().join("board.db"))
        .env("HERDR_BOARD_CONFIG", dir.path().join("missing-config.toml"))
        .env("HOME", dir.path())
        .env_remove("BOARD_SCOPE_PATH")
        .env_remove("HERDR_PLUGIN_CONTEXT_JSON")
        .env_remove("BOARD_RUN_ID")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    server.join().unwrap();

    let error = json_error(&out);
    assert_eq!(error["error"]["code"], 73);
    assert_eq!(error["error"]["kind"], "comment_forbidden");
    assert_eq!(
        error["error"]["details"],
        serde_json::json!({"comment_id": 41, "owner_run_id": 9})
    );
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("comment operation"));
}

#[test]
fn comment_mutations_enforce_agent_run_ownership_and_reject_system_comments() {
    let td = TestDaemon::start(&[]);
    let card_id = old_card(&td, "comment authorization");

    let own = json_output(&td.board_with_env(
        &[
            "card",
            "comment",
            "add",
            &card_id.to_string(),
            "own comment",
            "--json",
        ],
        &[("BOARD_RUN_ID", "41")],
    ));
    assert_eq!(own["author"], "agent:41");
    let own_id = own["id"].as_i64().expect("own comment id");

    let other = json_output(&td.board_with_env(
        &[
            "card",
            "comment",
            "add",
            &card_id.to_string(),
            "other comment",
            "--json",
        ],
        &[("BOARD_RUN_ID", "42")],
    ));
    assert_eq!(other["author"], "agent:42");
    let other_id = other["id"].as_i64().expect("other comment id");

    let system = td
        .client()
        .comment_add(card_id, "system comment", Some("system"))
        .unwrap();

    let edited_own = json_output(&td.board_with_env(
        &[
            "card",
            "comment",
            "edit",
            &own_id.to_string(),
            "edited by owner",
            "--json",
        ],
        &[("BOARD_RUN_ID", "41")],
    ));
    assert_eq!(edited_own["body"], "edited by owner");
    assert_eq!(edited_own["author"], "agent:41");

    let denied_edit = td.board_with_env(
        &[
            "card",
            "comment",
            "edit",
            &other_id.to_string(),
            "illegally edited",
            "--json",
        ],
        &[("BOARD_RUN_ID", "41")],
    );
    let denied_edit = json_error(&denied_edit);
    assert!(denied_edit["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("comment"));

    let denied_delete = td.board_with_env(
        &[
            "card",
            "comment",
            "delete",
            &other_id.to_string(),
            "--yes",
            "--json",
        ],
        &[("BOARD_RUN_ID", "41")],
    );
    json_error(&denied_delete);

    let deleted_own = json_output(&td.board_with_env(
        &[
            "card",
            "comment",
            "delete",
            &own_id.to_string(),
            "--yes",
            "--json",
        ],
        &[("BOARD_RUN_ID", "41")],
    ));
    assert_eq!(deleted_own["deleted"], true);

    let human_edit = json_output(&td.board(&[
        "card",
        "comment",
        "edit",
        &other_id.to_string(),
        "edited by human",
        "--json",
    ]));
    assert_eq!(human_edit["body"], "edited by human");
    assert_eq!(human_edit["author"], "agent:42");

    let system_edit = td.board(&[
        "card",
        "comment",
        "edit",
        &system.id.to_string(),
        "cannot edit system",
        "--json",
    ]);
    json_error(&system_edit);

    let system_delete = td.board_with_env(
        &[
            "card",
            "comment",
            "delete",
            &system.id.to_string(),
            "--yes",
            "--json",
        ],
        &[("BOARD_RUN_ID", "41")],
    );
    json_error(&system_delete);

    let detail = td.client().card_get(card_id).unwrap();
    assert!(detail
        .comments
        .iter()
        .any(|comment| { comment.id == other_id && comment.body == "edited by human" }));
    assert!(!detail.comments.iter().any(|comment| comment.id == own_id));
    assert!(detail
        .comments
        .iter()
        .any(|comment| comment.id == system.id));
}

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

    let reordered = json_output(&td.board(&["column", "reorder", &id.to_string(), "0", "--json"]));
    assert!(reordered.is_array());

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

#[test]
fn template_apply_is_a_canonical_command() {
    let td = TestDaemon::start(&[]);
    let result = json_output(&td.board(&["template", "apply", "pipeline", "--json"]));
    let columns = result.as_array().expect("template result is columns");
    assert!(columns.iter().any(|column| column["name"] == "Todo"));
    assert!(columns.iter().any(|column| column["name"] == "Execute"));
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

#[test]
fn board_version_reports_cli_and_daemon_versions_without_forcing_autostart() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("offline-boardd.sock");
    let offline = Command::new(super::BOARD_BIN)
        .args(["version", "--json"])
        .env("BOARD_SOCKET", &socket)
        .env("BOARD_DB", dir.path().join("board.db"))
        .env("HERDR_BOARD_CONFIG", dir.path().join("missing-config.toml"))
        .env("HOME", dir.path())
        .env_remove("BOARD_SCOPE_PATH")
        .env_remove("BOARD_RUN_ID")
        .output()
        .expect("run offline board version");
    let offline = json_output(&offline);
    assert_eq!(offline["cli_version"], env!("CARGO_PKG_VERSION"));
    let offline_daemon = offline
        .get("daemon_version")
        .expect("offline version still reports daemon_version");
    assert!(offline_daemon.is_null() || offline_daemon == "unavailable");
    assert!(!socket.exists(), "board version must not autostart boardd");

    let td = TestDaemon::start(&[]);
    let online = json_output(&td.board(&["version", "--json"]));
    assert_eq!(online["cli_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(online["daemon_version"], env!("CARGO_PKG_VERSION"));

    // The daemon status command remains a separate operational probe.
    let status = json_output(&td.board(&["daemon", "status", "--json"]));
    assert_eq!(status["version"], env!("CARGO_PKG_VERSION"));
    assert!(status.get("active_runs").is_some());
}

#[test]
fn skill_prints_the_operational_skill_byte_for_byte() {
    let out = Command::new(super::BOARD_BIN)
        .arg("skill")
        .output()
        .expect("run board skill");
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    assert_eq!(out.stdout, include_bytes!("../../../../skill/SKILL.md"));
}

#[test]
fn json_errors_have_a_stable_code_and_message_shape() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&["card", "show", "999999", "--json"]);
    let error = json_error(&out);
    assert_eq!(error["error"]["code"], 2);
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("999999"));
}
