//! Cross-cutting CLI surface: the command tree's refusals, `template apply`,
//! version/status separation, `skill`, and the JSON error envelope.

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use super::{json_error, json_output, TestDaemon};

#[test]
fn daemon_socket_is_owner_only() {
    let td = TestDaemon::start(&[]);
    let mode = std::fs::metadata(&td.socket)
        .expect("boardd socket metadata")
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(mode, 0o600, "boardd socket must be owner-only");
}

#[test]
fn top_level_status_is_rejected() {
    // Parsing fails before any socket work, so this needs no daemon at all.
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(super::BOARD_BIN)
        .arg("status")
        .current_dir(dir.path())
        .env("BOARD_SOCKET", dir.path().join("unused-boardd.sock"))
        .env("BOARD_DB", dir.path().join("board.db"))
        .env("HERDR_BOARD_CONFIG", dir.path().join("missing-config.toml"))
        .env("HOME", dir.path())
        .env_remove("BOARD_SCOPE_PATH")
        .env_remove("BOARD_RUN_ID")
        .stdin(Stdio::null())
        .output()
        .expect("run board status");

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

#[test]
fn template_apply_is_a_canonical_command() {
    let td = TestDaemon::start(&[]);
    let result = json_output(&td.board(&["template", "apply", "pipeline", "--json"]));
    let columns = result.as_array().expect("template result is columns");
    assert!(columns.iter().any(|column| column["name"] == "Todo"));
    assert!(columns.iter().any(|column| column["name"] == "Execute"));
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
    assert_eq!(
        *offline_daemon,
        serde_json::Value::Null,
        "with no daemon reachable the field is present and null"
    );
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

    // The comparison above is tautological by construction: it checks the
    // output against the very file the binary embeds, so it can only prove the
    // command prints *a* file. Pin the structure an agent reading `board skill`
    // depends on, which fails if the embedded file is ever replaced or gutted.
    let skill = std::str::from_utf8(&out.stdout).expect("skill output is valid UTF-8");
    assert!(!skill.trim().is_empty(), "skill output must not be empty");
    assert!(
        skill.starts_with("---\nname: herdr-board\n"),
        "skill opens with the frontmatter that names it; got: {:?}",
        skill.chars().take(40).collect::<String>()
    );
    for heading in [
        "# herdr-board",
        "## Inside a run",
        "## TUI",
        "## CLI taxonomy",
        "### Projects and boards",
        "### Cards",
        "### Comments",
        "### Runs",
        "### Columns and discovery",
    ] {
        assert!(
            skill.lines().any(|line| line == heading),
            "skill is missing the section heading {heading}"
        );
    }
    assert!(
        skill.contains("board done --outcome ok"),
        "skill documents how a dispatched agent closes its run"
    );
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
