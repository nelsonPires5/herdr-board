//! Live Antigravity model catalog: populate real agy models + per-model
//! efforts from the CLI itself.
//!
//! Antigravity has no on-disk catalog file; the verified source of truth is
//! the CLI (agy 1.1.13 — `agy --help`, `agy --output-format json models`):
//!
//! ```text
//! $ agy --output-format json models
//! {"conversation_id":"","status":"SUCCESS","response":"gemini-3.7-flash-high\tGemini 3.7 Flash (High)\n...",
//!  "command":{"name":"models","data":{"models":[
//!    {"id":"gemini-3.7-flash-high","label":"Gemini 3.7 Flash (High)"},
//!    {"id":"gemini-3.7-flash-medium","label":"Gemini 3.7 Flash (Medium)"},
//!    {"id":"gemini-3.7-flash-low","label":"Gemini 3.7 Flash (Low)"},
//!    {"id":"claude-sonnet-4-6","label":"Claude Sonnet 4.6 (Thinking)"},
//!    {"id":"claude-opus-4-6-thinking","label":"Claude Opus 4.6 (Thinking)"},
//!    {"id":"gpt-oss-120b-medium","label":"GPT-OSS 120B (Medium)"}]
//!  }}}
//! ```
//!
//! Field-verified facts pinned here:
//! - **the argv is `agy --output-format json models`** — `--output-format` is
//!   a ROOT flag and must come before the `models` subcommand; the subcommand
//!   itself rejects the flag (`agy models --output-format json` fails with
//!   "flags provided but not defined"). A refactor must not drift to another
//!   spelling;
//! - the model list lives in `command.data.models[].id` (the top-level
//!   `response` string is a tab-separated table and is NOT parsed); an
//!   envelope whose `status` is not `"SUCCESS"` yields no catalog;
//! - catalog ids embed the reasoning-effort variant as a trailing `-low` /
//!   `-medium` / `-high` suffix, which the board normalizes into one base
//!   model carrying the merged effort ladder: variants
//!   `gemini-3.7-flash-high`, `-medium` and `-low` collapse into model
//!   `gemini-3.7-flash` with efforts `low`, `medium`, `high` (canonical
//!   ascending order), and the base id runs with
//!   `--model <base> --effort <effort>`;
//! - models whose id carries no effort suffix (e.g. `claude-sonnet-4-6`,
//!   `claude-opus-4-6-thinking`) have a **fixed effort**: they are listed
//!   with empty efforts, so the UI offers no effort selector and argv never
//!   carries `--effort` for them;
//! - the board's `Effort` ladder has 7 levels but agy only knows
//!   `low|medium|high` (verified against `agy --help`: "--effort Reasoning
//!   effort for the current CLI session (low|medium|high)"). Suffixes map
//!   only onto those three; the rest of the enum is never produced here.
//!
//! There is deliberately **no static fallback catalog**: the card contract is
//! "query only the live catalog". When the CLI is missing, unauthenticated,
//! fails, times out, or yields nothing usable, [`live_models`] returns `None`
//! and the antigravity harness degrades to free-form (stored models still
//! run; only new selection is constrained, because the UIs have nothing to
//! offer).
//!
//! Parsing is safely bounded, mirroring `opencode_catalog`: a hard cap on
//! entries and on the raw stdout bytes of one CLI run, plus a wall-clock
//! budget — a pathological CLI output or a hung CLI cannot exhaust memory or
//! block the daemon forever. Everything here is pure subprocess reading;
//! nothing mutates state.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::capability::ModelInfo;
use crate::protocol::Effort;

/// Hard cap on catalog entries parsed from one CLI run.
pub const MAX_MODELS: usize = 256;
/// Hard cap on the raw stdout bytes retained from one CLI run: the largest
/// catalog the parser can consume is comfortably below this, so anything
/// beyond it is unparsable garbage and the run is abandoned.
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// Wall-clock budget for one `agy --output-format json models` run: the
/// discovery sits on the daemon's request/validation path, so a hung CLI
/// must not block forever.
pub const CLI_TIMEOUT: Duration = Duration::from_secs(30);

/// The exact argv the daemon runs to discover agy models (field-verified
/// against agy 1.1.13: `--output-format` is a root flag; `agy models
/// --output-format json` fails with "flags provided but not defined").
pub fn models_argv(agy_bin: &str) -> Vec<String> {
    vec![
        agy_bin.to_string(),
        "--output-format".to_string(),
        "json".to_string(),
        "models".to_string(),
    ]
}

/// Resolve the agy binary: `$AGY_BIN` else the `agy` on PATH. Mirrors the
/// daemon's other discovery overrides. Returns `None` when the env var is set
/// but empty.
pub fn default_agy_bin() -> Option<String> {
    match std::env::var("AGY_BIN") {
        Ok(bin) if !bin.trim().is_empty() => Some(bin),
        _ => Some("agy".to_string()),
    }
}

/// The effort levels agy can express (verified against `agy --help`:
/// `--effort (low|medium|high)`), in canonical ascending board order.
const ANTIGRAVITY_EFFORTS: [Effort; 3] = [Effort::Low, Effort::Medium, Effort::High];

/// Parse a trailing effort-variant suffix off a catalog id.
///
/// The catalog lists one id per variant with the effort embedded as a
/// trailing `-low` / `-medium` / `-high` suffix (`gemini-3.7-flash-high`).
/// Returns the base id and the effort; `None` means the id carries no
/// expressible variant (e.g. `claude-sonnet-4-6`, whose trailing `-4-6` is
/// not an effort spelling, or `claude-opus-4-6-thinking`) — the whole id is
/// the base model and its effort is fixed.
fn split_effort_suffix(id: &str) -> Option<(&str, Effort)> {
    let (base, suffix) = id.rsplit_once('-')?;
    if base.is_empty() {
        return None;
    }
    let effort = match suffix {
        "low" => Effort::Low,
        "medium" => Effort::Medium,
        "high" => Effort::High,
        _ => return None,
    };
    Some((base, effort))
}

/// Parse the raw stdout of `agy --output-format json models` into a model
/// catalog, normalizing effort variants onto base models.
///
/// The envelope is `{status, response, command: {name, data: {models:
/// [{id, label}]}}}`. Only `command.data.models[].id` is read (the
/// top-level `response` table is display text). Each id either normalizes
/// to a base + effort (`gemini-3.7-flash-high` → `gemini-3.7-flash` +
/// `high`) or is a fixed-effort model (`claude-sonnet-4-6` → itself with no
/// efforts). Variants of one base merge into a single model whose efforts
/// are the canonical ascending ladder (`low`, `medium`, `high`); a base
/// with no effort suffix at all keeps empty efforts — the model stays
/// selectable with no effort selector. Output is sorted by id and
/// de-duplicated.
///
/// Returns `None` when the envelope is missing, malformed, carries a
/// non-`SUCCESS` status, or yields no usable models — the caller treats
/// that as "catalog unavailable".
pub fn parse_models_json(stdout: &str) -> Option<Vec<ModelInfo>> {
    let root: Value = serde_json::from_str(stdout).ok()?;
    if root.get("status").and_then(Value::as_str) != Some("SUCCESS") {
        return None;
    }
    let models = root.get("command")?.get("data")?.get("models")?;
    let entries = models.as_array()?;

    // base id → efforts (BTreeSet keeps canonical ascending order for free).
    let mut by_base: BTreeMap<String, Vec<Effort>> = BTreeMap::new();
    for entry in entries.iter().take(MAX_MODELS) {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        match split_effort_suffix(id) {
            Some((base, effort)) => {
                let efforts = by_base.entry(base.to_string()).or_default();
                if !efforts.contains(&effort) {
                    efforts.push(effort);
                }
            }
            None => {
                by_base.entry(id.to_string()).or_default();
            }
        }
    }
    if by_base.is_empty() {
        return None;
    }
    let mut out: Vec<ModelInfo> = by_base
        .into_iter()
        .map(|(id, mut efforts)| {
            // Canonical ascending ladder (low → medium → high); the map
            // preserves catalog encounter order, so sort explicitly.
            efforts.retain(|e| ANTIGRAVITY_EFFORTS.contains(e));
            efforts.sort_by_key(|e| {
                ANTIGRAVITY_EFFORTS
                    .iter()
                    .position(|k| k == e)
                    .unwrap_or(usize::MAX)
            });
            ModelInfo { id, efforts }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Some(out)
}

/// Load the live agy model catalog by shelling out to the CLI and parsing
/// its JSON model list. Returns `None` when the binary is missing, exits
/// non-zero, times out, or yields no usable models.
pub fn load_from_cli(agy_bin: &str) -> Option<Vec<ModelInfo>> {
    load_from_cli_bounded(agy_bin, CLI_TIMEOUT)
}

/// The bounded core of [`load_from_cli`] with a caller-chosen wall-clock
/// budget (the tests use a short controlled timeout; production uses
/// [`CLI_TIMEOUT`]).
///
/// The run is bounded in both dimensions a pathological CLI could exhaust:
/// - **wall clock**: the child is killed (and reaped) when the deadline
///   passes, so a hung `agy` cannot block the daemon forever;
/// - **stdout**: a reader thread drains stdout into a buffer capped at
///   [`MAX_OUTPUT_BYTES`] (stdin/stderr are null, so no other pipe can fill
///   and deadlock the child), so an unparsable firehose cannot exhaust
///   memory.
///
/// Returns `None` on spawn failure, timeout, oversized output, a failed
/// read, a non-zero exit, or a lost reader thread.
pub fn load_from_cli_bounded(agy_bin: &str, timeout: Duration) -> Option<Vec<ModelInfo>> {
    let argv = models_argv(agy_bin);
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
    parse_models_json(&String::from_utf8_lossy(&stdout))
}

/// Poll interval while reaping a child whose stdout already reached EOF.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// The live agy model catalog: CLI discovery, or `None` when the discovery
/// cannot run or yields nothing usable.
///
/// Live discovery is **disabled** when `agy_bin` is `None` — the daemon
/// only sets it at startup; tests leave it unset and get the free-form
/// catalog (`None` = catalog unavailable). There is deliberately no static
/// fallback: an unavailable catalog degrades the harness to free-form
/// rather than pinning stale model names.
pub fn live_models(agy_bin: Option<&str>) -> Option<Vec<ModelInfo>> {
    load_from_cli(agy_bin?)
}
