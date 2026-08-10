//! Live OpenCode model catalog: populate real opencode models + per-model
//! efforts from the CLI itself.
//!
//! OpenCode has no on-disk catalog file like Pi's models-store or Codex's
//! models_cache; the verified source of truth is the CLI:
//! `opencode models --verbose` prints, per model, a `provider/model` header
//! line followed by one JSON object whose `variants` map holds the model's
//! reasoning-effort variants (e.g. `{"low": …}`). Some models declare
//! `"variants": {}` — verified live for `opencode/nemotron-3-ultra-free` —
//! and are **still valid models**: they stay listed with empty efforts. So
//! the daemon's live opencode catalog is:
//!   1. run `opencode models --verbose` (the argv is pinned in
//!      [`models_argv`] — a refactor must not drift to another command);
//!   2. parse the repeated `provider/model` + JSON-object pairs — the model
//!      id is the **header line** (the JSON `id` alone is not
//!      provider-qualified), and each `variants` key maps onto the board
//!      `Effort` ladder in canonical ascending order: opencode's `none`
//!      becomes the board's `off`, and variant keys the board does not know
//!      (e.g. `thinking`) are filtered out rather than growing the protocol
//!      enum. A valid model whose variants map onto no board effort is
//!      **listed with empty efforts** — selecting it offers no effort while
//!      the model itself stays selectable;
//!   3. fall back to the static catalog [`fallback_models`] — which
//!      truthfully lists `opencode/nemotron-3-ultra-free` (empty efforts,
//!      matching the real `variants: {}`) plus the fixture model
//!      `opencode/deepseek-v4-flash-free` (low/high/max) — when the CLI is
//!      missing, fails, or yields nothing.
//!
//! Parsing is safely bounded: a hard cap on entries ([`MAX_MODELS`]), on the
//! bytes read per JSON object ([`MAX_OBJECT_BYTES`]), and on the raw stdout of
//! one CLI run ([`MAX_OUTPUT_BYTES`]), plus a wall-clock budget
//! ([`CLI_TIMEOUT`]) — so a pathological CLI output or a hung CLI cannot
//! exhaust memory or block the daemon forever. Everything here is pure
//! subprocess reading; nothing mutates state.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::capability::ModelInfo;
use crate::protocol::Effort;

/// Hard cap on catalog entries parsed from one CLI run.
pub const MAX_MODELS: usize = 256;
/// Hard cap on the bytes read for a single model's JSON object.
pub const MAX_OBJECT_BYTES: usize = 64 * 1024;
/// Hard cap on the raw stdout bytes retained from one CLI run: the largest
/// catalog the parser can consume is `MAX_MODELS × MAX_OBJECT_BYTES` (16 MiB),
/// so anything beyond that is unparsable garbage and the run is abandoned.
pub const MAX_OUTPUT_BYTES: usize = MAX_MODELS * MAX_OBJECT_BYTES;
/// Wall-clock budget for one `opencode models --verbose` run: the discovery
/// sits on the daemon's request path, so a hung CLI must not block forever.
pub const CLI_TIMEOUT: Duration = Duration::from_secs(30);

/// Resolve the opencode binary: `$OPENCODE_BIN` else the `opencode` on PATH.
/// Mirrors the daemon's other discovery overrides. Returns `None` when the
/// env var is set but empty.
pub fn default_opencode_bin() -> Option<String> {
    match std::env::var("OPENCODE_BIN") {
        Ok(bin) if !bin.trim().is_empty() => Some(bin),
        _ => Some("opencode".to_string()),
    }
}

/// The exact argv the daemon runs to discover opencode models (field-verified
/// against `opencode models --help`: `--verbose` prints "more verbose model
/// output (includes metadata like costs)").
pub fn models_argv(opencode_bin: &str) -> Vec<String> {
    vec![
        opencode_bin.to_string(),
        "models".to_string(),
        "--verbose".to_string(),
    ]
}

/// The board's canonical ascending effort order (also the order opencode's
/// variant keys are mapped into, minus `none` which maps onto [`Effort::Off`]).
const EFFORT_ORDER: [Effort; 7] = [
    Effort::Off,
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// Map one opencode variant key onto the board ladder. OpenCode spells the
/// lowest level `none` where the board says `off`; any key the board does not
/// know (e.g. `thinking`) maps to `None` and is filtered out — the `Effort`
/// enum is protocol and never grows from a CLI output.
fn effort_from_variant(variant: &str) -> Option<Effort> {
    if variant == "none" {
        return Some(Effort::Off);
    }
    Effort::parse_str(variant)
}

/// Collect a model's `variants` map keys into the canonical ascending board
/// ladder, with unknown variants filtered and duplicates removed. An empty
/// result means the model declares no board expressible effort (e.g. a real
/// `variants: {}`): the model stays listed with empty efforts.
fn efforts_of_variants(variants: Option<&Value>) -> Vec<Effort> {
    let Some(Value::Object(map)) = variants else {
        return Vec::new();
    };
    let mut supported: Vec<Effort> = Vec::new();
    for key in map.keys() {
        if let Some(effort) = effort_from_variant(key) {
            if !supported.contains(&effort) {
                supported.push(effort);
            }
        }
    }
    EFFORT_ORDER
        .iter()
        .copied()
        .filter(|effort| supported.contains(effort))
        .collect()
}

/// Whether a line looks like a `provider/model` header line (the standalone
/// prefix before a model's JSON object). JSON member lines never match: they
/// are indented and quoted.
fn is_model_header(line: &str) -> bool {
    let line = line.trim();
    if line.len() > 256 {
        return false;
    }
    let Some((provider, model)) = line.split_once('/') else {
        return false;
    };
    !provider.is_empty()
        && !model.is_empty()
        && provider
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Whether a line ends an open JSON object (brace balance zero). Handles
/// quotes and escapes well enough for the JSON the CLI emits; a line that
/// opens and closes more than it closes is handled by the running depth.
fn closes_object(line: &str, mut depth: i64) -> Option<i64> {
    let mut in_string = false;
    let mut escaped = false;
    for ch in line.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else {
            match ch {
                '"' => in_string = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth < 0 {
                        return None;
                    }
                }
                _ => {}
            }
        }
    }
    Some(depth)
}

/// Parse the raw stdout of `opencode models --verbose` into a model catalog.
///
/// The shape is a repeated `provider/model` header line followed by one
/// pretty-printed JSON object (the model metadata with its `variants` map).
/// The provider-qualified header line is the model id; a valid model is
/// **always listed** — its `efforts` are the mapped variant ladder, empty
/// when the model declares no variants the board knows (e.g. a real
/// `variants: {}`). Malformed objects and stray lines are skipped without
/// aborting the rest. Output is sorted by id and de-duplicated. Parsing stops
/// at [`MAX_MODELS`] entries and skips any JSON object larger than
/// [`MAX_OBJECT_BYTES`].
pub fn parse_verbose_output(stdout: &str) -> Vec<ModelInfo> {
    let mut out: Vec<ModelInfo> = Vec::new();
    let mut current_id: Option<String> = None;
    let mut object_lines: Vec<&str> = Vec::new();
    let mut object_bytes = 0_usize;
    let mut depth: i64 = 0;

    let flush = |out: &mut Vec<ModelInfo>, id: Option<&str>, lines: &[&str]| {
        let Some(id) = id else { return };
        if lines.is_empty() {
            return;
        }
        if lines.iter().map(|l| l.len()).sum::<usize>() > MAX_OBJECT_BYTES {
            // Oversized object: skip this entry, keep parsing the rest.
            return;
        }
        let raw = lines.join("\n");
        let Ok(root) = serde_json::from_str::<Value>(&raw) else {
            return;
        };
        // A valid model is listed even when no variant maps onto a board
        // effort: empty `efforts` means the board offers no effort selector
        // for it while the model stays selectable.
        let efforts = efforts_of_variants(root.get("variants"));
        out.push(ModelInfo {
            id: id.to_string(),
            efforts,
        });
    };

    for line in stdout.lines() {
        let trimmed = line.trim();
        // A header line wins over any in-flight object: JSON member lines can
        // never match the strict `provider/model` shape (they are quoted and
        // indented), so this only ever fires on a real header — including the
        // case where the previous object never closed (a broken entry is
        // flushed as a parse failure and skipped).
        if is_model_header(trimmed) {
            flush(&mut out, current_id.as_deref(), &object_lines);
            current_id = Some(trimmed.to_string());
            object_lines.clear();
            object_bytes = 0;
            depth = 0;
            continue;
        }
        if current_id.is_some() {
            if object_bytes + line.len() > MAX_OBJECT_BYTES {
                // Oversized object: drop the whole entry, keep parsing.
                current_id = None;
                object_lines.clear();
                object_bytes = 0;
                depth = 0;
                continue;
            }
            object_lines.push(line);
            object_bytes += line.len();
            match closes_object(line, depth) {
                Some(0) => {
                    flush(&mut out, current_id.as_deref(), &object_lines);
                    current_id = None;
                    object_lines.clear();
                    object_bytes = 0;
                    depth = 0;
                }
                Some(d) => depth = d,
                None => {
                    // Unbalanced brace in a single line: skip this entry.
                    current_id = None;
                    object_lines.clear();
                    object_bytes = 0;
                    depth = 0;
                }
            }
        }
        if out.len() >= MAX_MODELS {
            break;
        }
    }
    // A trailing entry whose object never closed is dropped with the rest.
    flush(&mut out, current_id.as_deref(), &object_lines);

    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// Load the live opencode model catalog by shelling out to the CLI and
/// parsing its verbose model list. Returns `None` when the binary is missing,
/// exits non-zero, or yields no usable models (caller falls back to
/// [`fallback_models`]).
pub fn load_from_cli(opencode_bin: &str) -> Option<Vec<ModelInfo>> {
    load_from_cli_bounded(opencode_bin, CLI_TIMEOUT)
}

/// The bounded core of [`load_from_cli`] with a caller-chosen wall-clock
/// budget (the tests use a short controlled timeout; production uses
/// [`CLI_TIMEOUT`]).
///
/// The run is bounded in both dimensions a pathological CLI could exhaust:
/// - **wall clock**: the child is killed (and reaped) when the deadline
///   passes, so a hung `opencode` cannot block the daemon forever;
/// - **stdout**: a reader thread drains stdout into a buffer capped at
///   [`MAX_OUTPUT_BYTES`] (stdin/stderr are null, so no other pipe can fill
///   and deadlock the child), so an unparsable firehose cannot exhaust memory.
///
/// Returns `None` on spawn failure, timeout, oversized output, a failed read,
/// or a lost reader thread — the caller keeps the static fallback either way.
pub fn load_from_cli_bounded(opencode_bin: &str, timeout: Duration) -> Option<Vec<ModelInfo>> {
    let argv = models_argv(opencode_bin);
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut buffer: Vec<u8> = Vec::new();
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buffer.len() + n > MAX_OUTPUT_BYTES {
                        let _ = tx.send(None);
                        return;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                }
                Err(_) => {
                    let _ = tx.send(None);
                    return;
                }
            }
        }
        let _ = tx.send(Some(buffer));
    });
    let deadline = Instant::now() + timeout;
    let stdout = match rx.recv_timeout(timeout) {
        Ok(Some(buffer)) => buffer,
        // Timeout, oversized output, or read failure: kill the child so its
        // pipe closes, reap it, and join the reader. The channel is dropped
        // first so a reader blocked on a final send cannot hang the join.
        Ok(None) | Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            drop(rx);
            let _ = reader.join();
            return None;
        }
    };
    // The reader saw EOF, but the process itself may still be exiting (a
    // grandchild could hold the pipe). Reap it within the same deadline;
    // a child that will not exit after EOF is killed like a timeout.
    let status = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => std::thread::sleep(remaining.min(POLL_INTERVAL)),
            Err(_) => break None,
        }
    };
    let status = match status {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    let _ = reader.join();
    if !status.success() {
        return None;
    }
    let models = parse_verbose_output(&String::from_utf8_lossy(&stdout));
    if models.is_empty() {
        return None;
    }
    Some(models)
}

/// Poll interval while reaping a child whose stdout already reached EOF.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// The static fallback opencode catalog: guarantees the `models` field is
/// defined even when no live CLI is reachable. Truthful, not aspirational:
/// the real `opencode/nemotron-3-ultra-free` declares `variants: {}`
/// (verified live), so it is listed with EMPTY efforts — selecting it offers
/// no board effort — and the fixture model
/// `opencode/deepseek-v4-flash-free` carries its verified `low`/`high`/`max`
/// variants so model/effort UX stays demonstrable without a live CLI. The
/// daemon prefers live discovered output whenever the CLI answers.
pub fn fallback_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "opencode/nemotron-3-ultra-free".to_string(),
            efforts: Vec::new(),
        },
        ModelInfo {
            id: "opencode/deepseek-v4-flash-free".to_string(),
            efforts: vec![Effort::Low, Effort::High, Effort::Max],
        },
    ]
}

/// The live opencode model catalog: CLI discovery first, else empty (the
/// caller keeps the static [`fallback_models`] catalog).
///
/// Live discovery is **disabled** when `opencode_bin` is `None` — the daemon
/// only sets it at startup; tests leave it unset and get the static catalog.
pub fn live_models(opencode_bin: Option<&str>) -> Vec<ModelInfo> {
    let Some(bin) = opencode_bin else {
        return Vec::new();
    };
    load_from_cli(bin).unwrap_or_default()
}
