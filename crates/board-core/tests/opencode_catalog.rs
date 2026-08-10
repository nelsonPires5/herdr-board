//! Live OpenCode model catalog discovery (`opencode models --verbose`).
//!
//! OpenCode has no on-disk catalog file like Pi's models-store or Codex's
//! models_cache; the verified source of truth is the CLI itself:
//! `opencode models --verbose` prints a repeated `provider/model` header line
//! followed by one JSON object per model, whose `variants` map holds the
//! model's reasoning-effort variants (e.g. `{"none": …}` / `{"high": …}`).
//!
//! Contracts pinned here (the daemon overlays these onto the opencode
//! capabilities exactly like `pi_catalog` / `codex_catalog`):
//! - the model id is the **`provider/model` header line** (the JSON `id`
//!   field alone is not provider-qualified);
//! - a model's `variants` keys map onto the board `Effort` ladder in
//!   canonical ascending order — opencode's `none` maps to the board's `off`,
//!   every other level keeps its canonical spelling, and keys the board does
//!   not know (e.g. `thinking`) are filtered out, never added to the `Effort`
//!   enum;
//! - a valid model is **listed even when no variant maps onto a board
//!   effort** (e.g. `variants: {}` — the real shape `opencode` reports for
//!   `opencode/nemotron-3-ultra-free`): its `efforts` are empty, meaning the
//!   board offers no effort selector for it while the model stays selectable;
//! - parsing is safely bounded: a hard cap on entries and on the bytes read
//!   per JSON object, so a pathological CLI output cannot exhaust memory;
//! - a missing/unparseable object is skipped; the remaining output still
//!   parses;
//! - a missing/failing CLI yields `None` → the caller keeps the static
//!   fallback catalog, which truthfully lists
//!   `opencode/nemotron-3-ultra-free` (empty efforts — verified live:
//!   `variants: {}`) plus the fixture model
//!   `opencode/deepseek-v4-flash-free` (low/high/max — verified live).

use std::fs;
use std::time::{Duration, Instant};

use board_core::opencode_catalog::{
    fallback_models, live_models, load_from_cli, load_from_cli_bounded, parse_verbose_output,
    MAX_MODELS, MAX_OBJECT_BYTES, MAX_OUTPUT_BYTES,
};
use board_core::protocol::Effort;

/// A mirror of the real `opencode models --verbose` shape (verified against
/// the installed CLI): `provider/model` header lines + one JSON object each.
/// `opencode/nemotron-3-ultra-free` declares `variants: {}` for real — the
/// model is valid but carries no board efforts.
const VERBOSE_FIXTURE: &str = r#"opencode/nemotron-3-ultra-free
{
  "id": "nemotron-3-ultra-free",
  "providerID": "opencode",
  "name": "Nemotron 3 Ultra Free",
  "variants": {}
}
opencode/deepseek-v4-flash-free
{
  "id": "deepseek-v4-flash-free",
  "providerID": "opencode",
  "variants": {
    "low": {"reasoningEffort": "low"},
    "high": {"reasoningEffort": "high"},
    "max": {"reasoningEffort": "max"}
  }
}
opencode-go/minimax-m3
{
  "id": "minimax-m3",
  "variants": {
    "none": {"reasoningEffort": "none"},
    "thinking": {"reasoningEffort": "thinking"}
  }
}
openai/gpt-5.6-sol
{
  "id": "gpt-5.6-sol",
  "variants": {
    "low": {"reasoningEffort": "low"},
    "xhigh": {"reasoningEffort": "xhigh"},
    "max": {"reasoningEffort": "max"}
  }
}
opencode/no-variants
{
  "id": "no-variants",
  "variants": {}
}
opencode/plain-no-variants-field
{
  "id": "plain-no-variants-field"
}
"#;

#[test]
fn parses_provider_prefixed_ids_and_maps_variants_onto_efforts() {
    let models = parse_verbose_output(VERBOSE_FIXTURE);
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "openai/gpt-5.6-sol",
            "opencode-go/minimax-m3",
            "opencode/deepseek-v4-flash-free",
            "opencode/nemotron-3-ultra-free",
            "opencode/no-variants",
            "opencode/plain-no-variants-field",
        ],
        "ids are the provider/model header lines, sorted; variant-less models stay listed"
    );

    let nemotron = models
        .iter()
        .find(|m| m.id == "opencode/nemotron-3-ultra-free")
        .unwrap();
    assert!(
        nemotron.efforts.is_empty(),
        "the real nemotron declares variants {{}} → listed with NO board efforts"
    );

    let deepseek = models
        .iter()
        .find(|m| m.id == "opencode/deepseek-v4-flash-free")
        .unwrap();
    assert_eq!(
        deepseek.efforts,
        vec![Effort::Low, Effort::High, Effort::Max],
        "variant keys map onto board efforts in canonical ascending order (verified live)"
    );

    let minimax = models
        .iter()
        .find(|m| m.id == "opencode-go/minimax-m3")
        .unwrap();
    assert_eq!(
        minimax.efforts,
        vec![Effort::Off],
        "opencode `none` maps to the board's lowest effort `off`; the unknown `thinking` variant is filtered"
    );

    let no_variants = models
        .iter()
        .find(|m| m.id == "opencode/no-variants")
        .unwrap();
    assert!(no_variants.efforts.is_empty());
    let plain = models
        .iter()
        .find(|m| m.id == "opencode/plain-no-variants-field")
        .unwrap();
    assert!(plain.efforts.is_empty());
}

#[test]
fn models_without_recognized_variants_are_listed_with_empty_efforts() {
    // `variants: {}`, a missing variants field, and an all-filtered variants
    // map (only `thinking`) all yield no expressible board effort — the model
    // is still a valid model, so it stays listed with empty efforts.
    let models = parse_verbose_output(
        r#"opencode/empty-variants
{
  "id": "empty-variants",
  "variants": {}
}
opencode/plain
{
  "id": "plain"
}
opencode/only-thinking
{
  "variants": {
    "thinking": {"reasoningEffort": "thinking"}
  }
}
"#,
    );
    assert_eq!(models.len(), 3, "got {models:?}");
    for model in &models {
        assert!(
            model.efforts.is_empty(),
            "{} must be listed with empty efforts",
            model.id
        );
    }
}

#[test]
fn malformed_objects_and_garbage_lines_are_skipped() {
    // A header whose object is not JSON is skipped, and so is a stray line
    // that is not a provider/model header; the rest of the output still
    // parses.
    let models = parse_verbose_output(
        r#"some random banner line
opencode/broken
{ this is not json
opencode/good
{
  "id": "good",
  "variants": {"low": {}}
}
trailing garbage
"#,
    );
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["opencode/good"]);
}

#[test]
fn header_id_wins_over_json_id_field() {
    // The JSON `id` is not provider-qualified; the header line is the id.
    let models = parse_verbose_output(
        r#"opencode/custom-alias
{
  "id": "some-internal-id",
  "variants": {"high": {}}
}
"#,
    );
    assert_eq!(models[0].id, "opencode/custom-alias");
    assert_eq!(models[0].efforts, vec![Effort::High]);
}

#[test]
fn empty_output_yields_no_models() {
    assert!(parse_verbose_output("").is_empty());
    assert!(parse_verbose_output("\n\n").is_empty());
}

#[test]
fn entry_count_is_hard_bounded() {
    // A pathological CLI output cannot exhaust memory: parsing stops at the
    // hard cap.
    let mut out = String::new();
    for i in 0..(MAX_MODELS + 50) {
        out.push_str(&format!(
            "prov/model-{i}\n{{\"id\": \"model-{i}\", \"variants\": {{\"low\": {{}}}}\n}}\n"
        ));
    }
    let models = parse_verbose_output(&out);
    assert_eq!(models.len(), MAX_MODELS, "entry cap applies");
}

#[test]
fn object_size_is_hard_bounded() {
    // A single JSON object larger than the per-object cap is skipped, so a
    // single bloated entry cannot exhaust memory either.
    let huge = format!(
        "opencode/huge\n{{\"id\": \"huge\", \"padding\": \"{}\", \"variants\": {{\"low\": {{}}}}}}\n",
        "x".repeat(MAX_OBJECT_BYTES * 2)
    );
    let models = parse_verbose_output(&huge);
    assert!(models.is_empty(), "oversized object is skipped: {models:?}");
}

#[test]
fn duplicate_provider_model_headers_dedupe() {
    // A duplicate header (never emitted by the real CLI, but bounded anyway)
    // yields a single model entry; the first occurrence wins.
    let models = parse_verbose_output(
        "opencode/dup\n{\"variants\": {\"low\": {}}}\nopencode/dup\n{\"variants\": {\"high\": {}}}\n",
    );
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].efforts, vec![Effort::Low]);
}

/// A fake `opencode` executable that prints the fixture to stdout.
#[cfg(unix)]
fn fixture_opencode_bin(dir: &tempfile::TempDir, stdout: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let script = format!("#!/bin/sh\ncat <<'HBEOF'\n{stdout}\nHBEOF\n");
    let bin = dir.path().join("opencode-fixture");
    fs::write(&bin, script).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    bin
}

#[cfg(unix)]
#[test]
fn load_from_cli_parses_the_verbose_output() {
    let dir = tempfile::tempdir().unwrap();
    let bin = fixture_opencode_bin(&dir, VERBOSE_FIXTURE);
    let models = load_from_cli(bin.to_str().unwrap()).unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "openai/gpt-5.6-sol",
            "opencode-go/minimax-m3",
            "opencode/deepseek-v4-flash-free",
            "opencode/nemotron-3-ultra-free",
            "opencode/no-variants",
            "opencode/plain-no-variants-field",
        ]
    );
}

#[cfg(unix)]
#[test]
fn load_from_cli_failing_binary_yields_none() {
    let dir = tempfile::tempdir().unwrap();
    // A script that exits non-zero (no model list) → None.
    let failing = dir.path().join("opencode-failing");
    fs::write(&failing, "#!/bin/sh\nexit 3\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&failing, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(load_from_cli(failing.to_str().unwrap()).is_none());

    // A missing binary → None.
    assert!(load_from_cli("/nonexistent/opencode-binary").is_none());
}

/// A fixture executable that records its own PID and then `exec`s the body, so
/// the recorded PID IS the killed process (no orphan child of a shell).
#[cfg(unix)]
fn pid_fixture_opencode_bin(
    dir: &tempfile::TempDir,
    pid_file: &str,
    body: &str,
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let script = format!("#!/bin/sh\necho \"$$\" > \"{pid_file}\"\nexec {body}\n");
    let bin = dir.path().join("opencode-pid-fixture");
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
    let pid_file = dir.path().join("fixture-timeout.pid");
    // A CLI that hangs far past the budget: the recorded PID must be gone once
    // the bounded run returns, proving kill+wait rather than abandonment.
    let bin = pid_fixture_opencode_bin(&dir, pid_file.to_str().unwrap(), "sleep 60");
    // The controlled budget is far below the hang, with generous margins so
    // the test never races a busy machine (a cold first spawn of a script can
    // take a few hundred ms, so a tiny deadline would be flaky).
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
    let pid_file = dir.path().join("fixture-oversized.pid");
    // A CLI that prints more than the total stdout cap (16 MiB + 1 byte) then
    // hangs, so the kill must come from the size bound, not the timeout.
    let bin = pid_fixture_opencode_bin(
        &dir,
        pid_file.to_str().unwrap(),
        "head -c $((16 * 1024 * 1024 + 1)) /dev/zero | tr '\\0' x; exec sleep 30",
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
const _: () = assert!(MAX_OUTPUT_BYTES == MAX_MODELS * MAX_OBJECT_BYTES);

#[test]
fn live_models_disabled_without_a_bin() {
    // `live_models(None)` is the daemon's no-configured-bin path: empty, so
    // the caller keeps the static fallback catalog.
    assert!(live_models(None).is_empty());
}

#[test]
fn fallback_models_truthfully_define_nemotron_and_the_fixture_model() {
    // The static fallback guarantees the fields exist and models are defined
    // even when no live CLI is reachable. It must be truthful: the real
    // `opencode/nemotron-3-ultra-free` declares `variants: {}` (verified
    // live), so it is listed with EMPTY efforts — selecting it offers no
    // board effort. The fixture model `opencode/deepseek-v4-flash-free`
    // carries its verified low/high/max variants so model/effort UX stays
    // demonstrable without a live CLI.
    let models = fallback_models();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "opencode/nemotron-3-ultra-free");
    assert!(models[0].efforts.is_empty());
    assert_eq!(models[1].id, "opencode/deepseek-v4-flash-free");
    assert_eq!(
        models[1].efforts,
        vec![Effort::Low, Effort::High, Effort::Max]
    );
}
