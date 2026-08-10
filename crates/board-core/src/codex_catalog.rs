//! Live Codex model catalog: populate real codex models + per-model efforts.
//!
//! The codex CLI ships a built-in capability catalog with `models: []`
//! (free-form), but the CLI's own catalog is discoverable on disk: the
//! integration writes `$CODEX_HOME/models_cache.json` (default
//! `~/.codex/models_cache.json`) with every model the CLI knows and the
//! reasoning levels each accepts. Unlike Pi there is no auth step — the cache
//! is the catalog, and a per-model `visibility` marks which slugs the CLI
//! actually offers the user.
//!
//! So the daemon's live codex catalog is:
//!   1. read `models_cache.json` from the codex home;
//!   2. keep the **visible** model slugs (`visibility` missing or `"list"`),
//!      mapping each `supported_reasoning_levels` into the board `Effort`
//!      ladder — codex's `none` becomes the board's `off`, and levels the
//!      board does not know (e.g. `ultra`) are filtered out rather than
//!      growing the protocol enum;
//!   3. fall back to the static free-form catalog (`models: []`) when the
//!      file is missing, malformed, empty, or from a newer CLI whose shape
//!      this parser does not recognize.
//!
//! The modern shape is `{"models": [{"slug", "visibility", ...,
//! "supported_reasoning_levels": [{"effort", "description"}]}]}`; the legacy
//! map shape (`{slug: {supported_reasoning_levels: [...]}}`) and plain-string
//! levels are also accepted. Everything here is pure file reading; nothing
//! mutates state. `auth.json` is deliberately never read.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::capability::ModelInfo;
use crate::protocol::Effort;

/// Resolve the codex home: `$CODEX_HOME` else `~/.codex`. Mirrors the codex
/// CLI's own default. Returns `None` when no home dir is known.
pub fn default_codex_home() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CODEX_HOME") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".codex"))
}

/// The board's canonical ascending effort order (also the order codex's
/// cache uses, minus `none` which maps onto [`Effort::Off`]).
const EFFORT_ORDER: [Effort; 7] = [
    Effort::Off,
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// Map one codex reasoning level onto the board ladder. Codex spells the
/// lowest level `none` where the board says `off`; any level the board does
/// not know (e.g. `ultra`) maps to `None` and is filtered out — the `Effort`
/// enum is protocol and never grows from a cache.
fn effort_from_level(level: &str) -> Option<Effort> {
    if level == "none" {
        return Some(Effort::Off);
    }
    Effort::parse_str(level)
}

/// Collect a model's `supported_reasoning_levels` (objects with an `effort`
/// field, or plain strings) into the canonical ascending board ladder, with
/// unknown levels filtered and duplicates removed. Empty result means the
/// model cannot express any board effort and must be dropped.
fn efforts_of(levels: Option<&Value>) -> Vec<Effort> {
    let mut supported: Vec<Effort> = Vec::new();
    if let Some(Value::Array(items)) = levels {
        for item in items {
            let level = match item {
                Value::String(s) => Some(s.as_str()),
                Value::Object(map) => map.get("effort").and_then(Value::as_str),
                _ => None,
            };
            if let Some(level) = level {
                if let Some(effort) = effort_from_level(level) {
                    if !supported.contains(&effort) {
                        supported.push(effort);
                    }
                }
            }
        }
    }
    EFFORT_ORDER
        .iter()
        .copied()
        .filter(|effort| supported.contains(effort))
        .collect()
}

/// Whether the cache marks this entry as offered to the user. `visibility`
/// missing (legacy entries) or `"list"` means visible; anything else
/// (`"hide"` — e.g. `codex-auto-review`) is dropped.
fn is_visible(entry: &Value) -> bool {
    matches!(
        entry.get("visibility").and_then(Value::as_str),
        None | Some("list")
    )
}

/// The model slug of one cache entry: its `slug`/`id` field, or the map key
/// the entry sits under.
fn slug_of(entry: &Value, map_key: Option<&str>) -> Option<String> {
    entry
        .get("slug")
        .or_else(|| entry.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| map_key.map(str::to_string))
}

/// Load the live codex model catalog from `codex_home`'s `models_cache.json`.
/// Returns `None` when the file is missing/unreadable/malformed or yields no
/// usable models (the caller falls back to the static free-form catalog).
pub fn load_from_files(codex_home: &Path) -> Option<Vec<ModelInfo>> {
    let raw = std::fs::read_to_string(codex_home.join("models_cache.json")).ok()?;
    let root: Value = serde_json::from_str(&raw).ok()?;

    // Candidate `(slug, entry)` pairs: the modern `models` array, a
    // `models`-as-map middle shape, or a legacy top-level slug map.
    let mut pairs: Vec<(String, Value)> = Vec::new();
    match root.get("models") {
        Some(Value::Array(entries)) => {
            for entry in entries {
                if let Some(slug) = slug_of(entry, None) {
                    pairs.push((slug, entry.clone()));
                }
            }
        }
        Some(Value::Object(map)) => {
            for (slug, entry) in map {
                pairs.push((slug.clone(), entry.clone()));
            }
        }
        _ => match &root {
            Value::Object(map) if !map.is_empty() => {
                for (slug, entry) in map {
                    pairs.push((slug.clone(), entry.clone()));
                }
            }
            _ => return None,
        },
    }

    let mut out: Vec<ModelInfo> = Vec::new();
    for (slug, entry) in pairs {
        // A bare string entry (slug only) carries no level information.
        let Value::Object(_) = &entry else { continue };
        if !is_visible(&entry) {
            continue;
        }
        let efforts = efforts_of(entry.get("supported_reasoning_levels"));
        if efforts.is_empty() {
            // Every level got filtered (or none were declared): the board
            // cannot express this model at any effort, so it stays out of the
            // catalog — the model is still accepted free-form with the
            // default ladder.
            continue;
        }
        out.push(ModelInfo { id: slug, efforts });
    }
    if out.is_empty() {
        return None;
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    Some(out)
}

/// The live codex model catalog: on-disk cache first, else empty (the caller
/// keeps the static free-form catalog).
///
/// Live discovery is **disabled** when `codex_home` is `None` — the daemon
/// only sets it at startup; tests leave it unset and get the static catalog.
pub fn live_models(codex_home: Option<&Path>) -> Vec<ModelInfo> {
    let Some(home) = codex_home else {
        return Vec::new();
    };
    load_from_files(home).unwrap_or_default()
}
