//! Live Antigravity model catalog discovery (`agy --output-format json
//! models`).
//!
//! Antigravity has no on-disk catalog file; the verified source of truth is
//! the CLI itself (agy 1.1.13): `agy --output-format json models` prints one
//! JSON envelope whose model list lives in `command.data.models[].id`.
//!
//! Contracts pinned here (the daemon overlays these onto the antigravity
//! capabilities exactly like `pi_catalog` / `codex_catalog` /
//! `opencode_catalog`):
//! - **the argv is `agy --output-format json models`** — `--output-format`
//!   is a ROOT flag and must precede the `models` subcommand (the
//!   subcommand rejects it: "flags provided but not defined");
//! - only an envelope with `status == "SUCCESS"` yields a catalog; anything
//!   else (error envelope, garbage, empty) is `None` = catalog unavailable;
//! - catalog ids embed the reasoning-effort variant as a trailing `-low` /
//!   `-medium` / `-high` suffix; variants normalize onto one base model
//!   carrying the merged effort ladder in canonical ascending order:
//!   `gemini-3.7-flash-high` + `-medium` + `-low` become model
//!   `gemini-3.7-flash` with efforts `[low, medium, high]`;
//! - ids with no effort suffix (`claude-sonnet-4-6`, `claude-opus-4-6-
//!   thinking`) are fixed-effort models: listed with EMPTY efforts, so the
//!   UI offers no effort selector and argv never sends `--effort` for them;
//! - a base id present only as a plain (suffix-less) catalog entry stays
//!   listed with empty efforts — it is selectable without an effort;
//! - parsing is safely bounded: a hard cap on entries and on the raw stdout
//!   bytes of one CLI run, plus a wall-clock budget with kill+reap;
//! - there is deliberately **no static fallback catalog**:
//!   [`live_models`](board_core::agy_catalog::live_models) returns `None`
//!   when the CLI is missing, fails, times out, or yields nothing usable —
//!   the antigravity harness then degrades to free-form (stored models keep
//!   running).

use std::fs;
use std::time::{Duration, Instant};

use board_core::agy_catalog::{
    live_models, load_from_cli, load_from_cli_bounded, models_argv, parse_models_json, MAX_MODELS,
    MAX_OUTPUT_BYTES,
};
use board_core::protocol::Effort;

/// A mirror of the real `agy --output-format json models` envelope shape
/// (verified against the installed CLI): the model list lives in
/// `command.data.models[].id`; the top-level `response` string is a
/// tab-separated table that is NOT parsed. `gemini-3.7-flash-*` carries its
/// three variants, `claude-sonnet-4-6` and `claude-opus-4-6-thinking` are
/// fixed-effort, `gpt-oss-120b-medium` has a single medium variant.
const JSON_FIXTURE: &str = r#"{
  "conversation_id": "",
  "status": "SUCCESS",
  "response": "id\tlabel\ngemini-3.7-flash-high\tGemini 3.7 Flash (High)\n",
  "command": {
    "name": "models",
    "data": {
      "models": [
        {"id": "gemini-3.7-flash-high", "label": "Gemini 3.7 Flash (High)"},
        {"id": "gemini-3.7-flash-medium", "label": "Gemini 3.7 Flash (Medium)"},
        {"id": "gemini-3.7-flash-low", "label": "Gemini 3.7 Flash (Low)"},
        {"id": "claude-sonnet-4-6", "label": "Claude Sonnet 4.6 (Thinking)"},
        {"id": "claude-opus-4-6-thinking", "label": "Claude Opus 4.6 (Thinking)"},
        {"id": "gpt-oss-120b-medium", "label": "GPT-OSS 120B (Medium)"}
      ]
    }
  }
}
"#;

#[test]
fn models_argv_pins_the_root_flag_before_the_subcommand() {
    // Field-verified: `--output-format` is a root flag. `agy models
    // --output-format json` fails with "flags provided but not defined";
    // `agy --output-format json models` is the working spelling.
    assert_eq!(
        models_argv("agy"),
        vec!["agy", "--output-format", "json", "models"]
    );
}

#[test]
fn parses_the_live_envelope_and_normalizes_variants_onto_base_models() {
    let models = parse_models_json(JSON_FIXTURE).expect("the SUCCESS fixture must parse");
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "claude-opus-4-6-thinking",
            "claude-sonnet-4-6",
            "gemini-3.7-flash",
            "gpt-oss-120b",
        ],
        "variant ids normalize onto base models, sorted"
    );

    let gemini = models.iter().find(|m| m.id == "gemini-3.7-flash").unwrap();
    assert_eq!(
        gemini.efforts,
        vec![Effort::Low, Effort::Medium, Effort::High],
        "the three catalog variants merge onto one base model with the canonical ascending ladder"
    );

    let gpt = models.iter().find(|m| m.id == "gpt-oss-120b").unwrap();
    assert_eq!(
        gpt.efforts,
        vec![Effort::Medium],
        "a single medium variant yields a one-level ladder"
    );

    // Fixed-effort models: the trailing `-4-6` / `-thinking` are not effort
    // suffixes, so the whole id is the base and there is no effort to offer.
    let sonnet = models.iter().find(|m| m.id == "claude-sonnet-4-6").unwrap();
    assert!(
        sonnet.efforts.is_empty(),
        "claude-sonnet-4-6 is fixed-effort: listed with NO board efforts"
    );
    let opus = models
        .iter()
        .find(|m| m.id == "claude-opus-4-6-thinking")
        .unwrap();
    assert!(
        opus.efforts.is_empty(),
        "claude-opus-4-6-thinking is fixed-effort: listed with NO board efforts"
    );
}

#[test]
fn non_success_envelope_yields_no_catalog() {
    // An error envelope (status != SUCCESS) is catalog-unavailable: None.
    assert!(parse_models_json(
        r#"{"conversation_id":"","status":"ERROR","response":"boom","command":{"name":"models","data":{"models":[]}}}"#
    )
    .is_none());
    // Garbage, empty output, and a missing model list are all None too.
    assert!(parse_models_json("").is_none());
    assert!(parse_models_json("not json at all").is_none());
    assert!(
        parse_models_json(r#"{"status":"SUCCESS","command":{"name":"models","data":{}}}"#)
            .is_none()
    );
    // A SUCCESS envelope whose model list is empty is unavailable too.
    assert!(parse_models_json(
        r#"{"status":"SUCCESS","command":{"name":"models","data":{"models":[]}}}"#
    )
    .is_none());
}

#[test]
fn suffix_efforts_are_the_only_three_agy_levels() {
    // agy only knows low|medium|high (`agy --help`: "--effort Reasoning
    // effort for the current CLI session (low|medium|high)"). Suffixes map
    // only onto those; `-max`-like or `-thinking`-like tails are not effort
    // suffixes and leave the model fixed-effort.
    let models = parse_models_json(
        r#"{"status":"SUCCESS","command":{"name":"models","data":{"models":[
            {"id":"model-a-low","label":"A"},
            {"id":"model-a-max","label":"A max"},
            {"id":"model-a-thinking","label":"A thinking"}
        ]}}}"#,
    )
    .unwrap();
    let a_low = models.iter().find(|m| m.id == "model-a").unwrap();
    assert_eq!(a_low.efforts, vec![Effort::Low]);
    let a_max = models.iter().find(|m| m.id == "model-a-max").unwrap();
    assert!(a_max.efforts.is_empty(), "model-a-max is fixed-effort");
    let a_thinking = models.iter().find(|m| m.id == "model-a-thinking").unwrap();
    assert!(
        a_thinking.efforts.is_empty(),
        "model-a-thinking is fixed-effort (the `-thinking` tail is not an effort suffix)"
    );
}

#[test]
fn duplicate_variant_ids_dedupe() {
    // A duplicated variant entry yields a single effort on the base model.
    let models = parse_models_json(
        r#"{"status":"SUCCESS","command":{"name":"models","data":{"models":[
            {"id":"dup-high","label":"h"},
            {"id":"dup-high","label":"h again"},
            {"id":"dup-low","label":"l"}
        ]}}}"#,
    )
    .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "dup");
    assert_eq!(models[0].efforts, vec![Effort::Low, Effort::High]);
}

#[test]
fn plain_base_ids_stay_selectable_without_efforts() {
    // A base id that appears only as a suffix-less catalog entry is a
    // fixed-effort model: listed, no efforts.
    let models = parse_models_json(
        r#"{"status":"SUCCESS","command":{"name":"models","data":{"models":[
            {"id":"plain","label":"plain"}
        ]}}}"#,
    )
    .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "plain");
    assert!(models[0].efforts.is_empty());
}

#[test]
fn entry_count_is_hard_bounded() {
    // A pathological catalog cannot exhaust memory: parsing stops at the cap.
    let mut entries = Vec::with_capacity(MAX_MODELS + 50);
    for i in 0..(MAX_MODELS + 50) {
        entries.push(format!(
            "{{\"id\": \"model-{i}-low\", \"label\": \"m{i}\"}}"
        ));
    }
    let out = format!(
        r#"{{"status":"SUCCESS","command":{{"name":"models","data":{{"models":[{}]}}}}}}"#,
        entries.join(",")
    );
    let models = parse_models_json(&out).expect("bounded parse still succeeds");
    assert_eq!(models.len(), MAX_MODELS, "entry cap applies");
}

/// A fake `agy` executable that prints the fixture to stdout.
#[cfg(unix)]
fn fixture_agy_bin(dir: &tempfile::TempDir, stdout: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let script = format!("#!/bin/sh\ncat <<'HBEOF'\n{stdout}\nHBEOF\n");
    let bin = dir.path().join("agy-fixture");
    fs::write(&bin, script).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    bin
}

#[cfg(unix)]
#[test]
fn load_from_cli_parses_the_live_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let bin = fixture_agy_bin(&dir, JSON_FIXTURE);
    let models = load_from_cli(bin.to_str().unwrap()).expect("fixture must parse");
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "claude-opus-4-6-thinking",
            "claude-sonnet-4-6",
            "gemini-3.7-flash",
            "gpt-oss-120b",
        ]
    );
}

#[cfg(unix)]
#[test]
fn load_from_cli_failing_binary_yields_none() {
    let dir = tempfile::tempdir().unwrap();
    // A script that exits non-zero (no model list) → None.
    let failing = dir.path().join("agy-failing");
    fs::write(&failing, "#!/bin/sh\nexit 3\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&failing, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(load_from_cli(failing.to_str().unwrap()).is_none());

    // A missing binary → None.
    assert!(load_from_cli("/nonexistent/agy-binary").is_none());
}

/// A fixture executable that records its own PID and then `exec`s the body, so
/// the recorded PID IS the killed process (no orphan child of a shell).
#[cfg(unix)]
fn pid_fixture_agy_bin(dir: &tempfile::TempDir, pid_file: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let script = format!("#!/bin/sh\necho \"$$\" > \"{pid_file}\"\nexec {body}\n");
    let bin = dir.path().join("agy-pid-fixture");
    fs::write(&bin, script).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    bin
}

/// Wait for the fixture's PID file (written at fixture start), so the kill
/// assertion always runs against a process that has actually started.
#[cfg(unix)]
fn wait_for_pid_file(pid_file: &std::path::Path) -> Option<u32> {
    for _ in 0..200 {
        if let Ok(pid) = fs::read_to_string(pid_file) {
            if let Ok(pid) = pid.trim().parse::<u32>() {
                return Some(pid);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// `kill -0 <pid>` fails once the process is gone (killed and reaped).
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(unix)]
#[test]
fn load_from_cli_bounded_times_out_and_kills_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("agy-fixture-timeout.pid");
    // A CLI that hangs far past the budget: the recorded PID must be gone once
    // the bounded run returns, proving kill+wait rather than abandonment.
    let bin = pid_fixture_agy_bin(&dir, pid_file.to_str().unwrap(), "sleep 60");
    let deadline = Duration::from_secs(2);
    let started = Instant::now();
    let models = load_from_cli_bounded(bin.to_str().unwrap(), deadline);
    let elapsed = started.elapsed();
    assert!(models.is_none(), "a hung CLI must yield None");
    assert!(
        elapsed < Duration::from_secs(30),
        "the child must be killed on timeout, not left to finish its sleep (took {elapsed:?})"
    );
    let pid = wait_for_pid_file(&pid_file).expect("the fixture wrote its PID before the kill");
    assert!(
        !process_alive(pid),
        "the hung child (pid {pid}) must be killed and reaped, not abandoned"
    );
}

#[cfg(unix)]
#[test]
fn load_from_cli_bounded_rejects_oversized_stdout_and_kills_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("agy-fixture-oversized.pid");
    // A CLI that prints more than the total stdout cap then hangs, so the kill
    // must come from the size bound, not the timeout.
    let bin = pid_fixture_agy_bin(
        &dir,
        pid_file.to_str().unwrap(),
        "head -c $((8 * 1024 * 1024 + 1)) /dev/zero | tr '\\0' x; exec sleep 30",
    );
    let started = Instant::now();
    let models = load_from_cli_bounded(bin.to_str().unwrap(), Duration::from_secs(30));
    let elapsed = started.elapsed();
    assert!(models.is_none(), "oversized stdout must yield None");
    assert!(
        elapsed < Duration::from_secs(2),
        "the oversized child must be killed promptly (took {elapsed:?})"
    );
    let pid = wait_for_pid_file(&pid_file).expect("the fixture wrote its PID before the kill");
    assert!(
        !process_alive(pid),
        "the oversized child (pid {pid}) must be killed and reaped, not abandoned"
    );
}

// The total stdout cap is the largest catalog the parser can consume; this
// invariant is compile-time asserted so it can never silently drift.
const _: () = assert!(MAX_OUTPUT_BYTES == 8 * 1024 * 1024);

#[test]
fn live_models_disabled_without_a_bin() {
    // `live_models(None)` is the daemon's no-configured-bin path (tests leave
    // it unset): catalog unavailable → None, so the harness is free-form.
    assert!(live_models(None).is_none());
}
