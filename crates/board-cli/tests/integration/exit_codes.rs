//! Process exit codes are part of the scripting contract: an agent must be able
//! to branch on `$?` instead of parsing stderr.
//!
//! `docs/protocol.md` defines RPC error codes `1` bad request / unknown method,
//! `2` not found, `3` invalid state, `4` herdr unavailable, `5` internal. Those
//! pass through to the exit status; anything else the daemon may report is
//! clamped to `70`, and errors raised by the CLI itself exit `64`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Output, Stdio};

use board_core::protocol::{Request, Response};
use serde_json::Value;

use super::TestDaemon;

const CLI_ERROR: i32 = 64;
const UNMAPPED_RPC: i32 = 70;

fn code(out: &Output) -> i32 {
    out.status.code().expect("board exits, never signals")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn old_card(td: &TestDaemon, title: &str) -> i64 {
    let out = td.board(&[
        "card",
        "create",
        "--title",
        title,
        "--harness",
        "fake",
        "--json",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    serde_json::from_slice::<Value>(&out.stdout).expect("card JSON")["id"]
        .as_i64()
        .expect("card id")
}

#[test]
fn a_successful_command_exits_zero() {
    let td = TestDaemon::start(&[]);
    assert_eq!(code(&td.board(&["card", "list", "--json"])), 0);
}

#[test]
fn rpc_not_found_exits_with_the_protocol_code() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&["card", "show", "999999", "--json"]);
    assert_eq!(code(&out), 2, "not found is protocol code 2");
    let error: Value = serde_json::from_slice(&out.stderr).expect("JSON error on stderr");
    assert_eq!(error["error"]["code"], 2);

    // The same code without --json: the exit status carries it either way.
    assert_eq!(code(&td.board(&["card", "show", "999999"])), 2);
}

#[test]
fn cli_local_errors_exit_outside_the_protocol_range() {
    let td = TestDaemon::start(&[]);
    let id = old_card(&td, "confirmation");

    // A refused confirmation is the CLI's own error, not a "not found".
    let refused = td.board(&["card", "delete", &id.to_string()]);
    assert_eq!(code(&refused), CLI_ERROR);

    let refused_json = td.board(&["card", "delete", &id.to_string(), "--json"]);
    assert_eq!(code(&refused_json), CLI_ERROR);
    let error: Value = serde_json::from_slice(&refused_json.stderr).expect("JSON error on stderr");
    assert_eq!(error["error"]["code"], CLI_ERROR);
    assert_eq!(error["error"]["kind"], "cli");

    // A usage error is equally the CLI's own.
    assert_eq!(code(&td.board(&["status"])), CLI_ERROR);
    assert_eq!(
        code(&td.board(&["card", "list", "--visibility", "bogus"])),
        CLI_ERROR
    );
}

/// A protocol code outside `1..=5` must not be handed to `exit()` raw: exit
/// statuses are taken modulo 256, so `256` would report success.
#[test]
fn out_of_range_rpc_codes_are_clamped_but_kept_in_the_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("odd-code.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: Request = serde_json::from_str(line.trim_end()).unwrap();
        let response = Response::err(request.id, 256, "a code that is 0 modulo 256");
        let mut wire = serde_json::to_string(&response).unwrap();
        wire.push('\n');
        stream.write_all(wire.as_bytes()).unwrap();
        stream.flush().unwrap();
    });

    let out = Command::new(super::BOARD_BIN)
        .args(["card", "show", "1", "--json"])
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

    assert_eq!(code(&out), UNMAPPED_RPC);
    let error: Value = serde_json::from_slice(&out.stderr).expect("JSON error on stderr");
    assert_eq!(
        error["error"]["code"], 256,
        "the envelope keeps the exact protocol code"
    );
}

/// A2: `--json` is decided by parsing, so a positional whose value happens to
/// be the literal `--json` must not switch the error renderer.
#[test]
fn a_json_shaped_argument_value_does_not_switch_error_rendering() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&["card", "comment", "add", "999999", "--", "--json"]);
    assert!(!out.status.success());
    let stderr = stderr(&out);
    assert!(
        stderr.starts_with("board: "),
        "expected a human error, got: {stderr}"
    );
    assert!(
        serde_json::from_str::<Value>(&stderr).is_err(),
        "a comment body of \"--json\" must not produce a JSON error: {stderr}"
    );
}

#[test]
fn a_real_json_flag_after_a_separated_value_still_renders_json() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&["--json", "card", "comment", "add", "999999", "--", "--json"]);
    assert!(!out.status.success());
    let error: Value = serde_json::from_slice(&out.stderr).expect("JSON error on stderr");
    assert_eq!(error["error"]["code"], 2);
}
