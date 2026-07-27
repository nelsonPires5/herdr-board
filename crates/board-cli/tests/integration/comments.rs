//! Nested `card comment` CRUD/history, the structured JSON error envelope for
//! a rejected comment mutation, and agent-run ownership policy.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};

use board_core::client::BoardClient;
use board_core::protocol::{Request, Response};

use super::{json_error, json_output, old_card, TestDaemon};

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
        // One invocation dials boardd exactly once: the command context owns a
        // single lazily connected client for the whole dispatch.
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
    assert_eq!(
        out.status.code(),
        Some(70),
        "a protocol code outside 1..=5 is clamped for the exit status"
    );
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
