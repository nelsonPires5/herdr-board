//! Live Pi model catalog: populate real Pi models + per-model efforts.
//!
//! Pi's built-in capability catalog is intentionally `models: []` (free-form)
//! because Pi's catalog is user/provider-specific. But it *is* discoverable on
//! disk. `pi --list-models` only shows models for providers the user is
//! authenticated with (verified empirically: with no `auth.json` it prints
//! "No models available"), and `~/.pi/agent/models-store.json` holds the same
//! catalog in JSON form — richer, actually, since it carries a per-model
//! `thinkingLevelMap`.
//!
//! So the daemon's live Pi catalog is:
//!   1. read `auth.json` → the providers the user has credentials for;
//!   2. read `models-store.json` → keep only those providers' models, mapping
//!      `thinkingLevelMap` keys to each model's effort levels;
//!   3. fall back to shelling out to `pi --list-models` when the files are
//!      missing/unreadable;
//!   4. fall back to the static free-form catalog (`models: []`) on failure.
//!
//! Everything here is pure file/subprocess reading; nothing mutates state.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::capability::ModelInfo;
use crate::protocol::Effort;

/// Resolve the pi agent dir: `$PI_CODING_AGENT_DIR` else `~/.pi/agent`.
/// Mirrors pi's own `getAgentDir()`. Returns `None` when no home dir.
pub fn default_agent_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("PI_CODING_AGENT_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".pi").join("agent"))
}

// -- auth.json (provider allow-list) ----------------------------------------

/// The providers the user has credentials for, parsed from `auth.json`.
/// Shape: `{ "<provider>": {"type": ..., ...}, ... }` — we only need the keys.
fn auth_providers(agent_dir: &Path) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(agent_dir.join("auth.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let obj = v.as_object()?;
    Some(obj.keys().cloned().collect())
}

// -- models-store.json (the catalog) ----------------------------------------

/// One model entry in `models-store.json` (only the fields we consume).
#[derive(Debug, Deserialize)]
struct StoreModel {
    id: String,
    #[serde(default)]
    reasoning: bool,
    #[serde(default, rename = "thinkingLevelMap")]
    thinking_level_map: Option<serde_json::Map<String, serde_json::Value>>,
}

/// One provider block in `models-store.json`.
#[derive(Debug, Deserialize)]
struct StoreProvider {
    #[serde(default)]
    models: Vec<StoreModel>,
}

/// Efforts a model accepts, from its `thinkingLevelMap` keys (canonical order);
/// when absent (e.g. plain reasoning models), the default Pi ladder applies.
fn efforts_for_model(m: &StoreModel) -> Vec<Effort> {
    match &m.thinking_level_map {
        Some(map) if !map.is_empty() => EFFORT_ORDER
            .iter()
            .copied()
            .filter(|e| map.contains_key(e.as_str()))
            .collect(),
        _ => default_pi_efforts(),
    }
}

/// The full Pi thinking ladder (off..max), used as the fallback effort set.
fn default_pi_efforts() -> Vec<Effort> {
    EFFORT_ORDER.to_vec()
}

/// Canonical ascending effort order for display.
const EFFORT_ORDER: [Effort; 7] = [
    Effort::Off,
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// Load the live Pi model catalog from `agent_dir`: models for authenticated
/// providers only. Returns `None` when either file is missing/unreadable or
/// yields no models (caller falls back to the CLI path / static catalog).
pub fn load_from_files(agent_dir: &Path) -> Option<Vec<ModelInfo>> {
    let providers = auth_providers(agent_dir)?;
    let raw = std::fs::read_to_string(agent_dir.join("models-store.json")).ok()?;
    let store: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&raw).ok()?;

    let mut out: Vec<ModelInfo> = Vec::new();
    for provider in providers {
        let Some(block) = store.get(&provider) else {
            continue;
        };
        let Ok(block) = serde_json::from_value::<StoreProvider>(block.clone()) else {
            continue;
        };
        for m in block.models {
            // reasoning=false models take no thinking flag — but Pi still
            // accepts `--thinking`; keep the model's mapped efforts (or the
            // ladder) regardless, mirroring `--list-models` which lists all.
            let _ = m.reasoning;
            out.push(ModelInfo {
                id: format!("{provider}/{}", m.id),
                efforts: efforts_for_model(&m),
            });
        }
    }
    if out.is_empty() {
        return None;
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Some(out)
}

// -- pi --list-models (fallback) --------------------------------------------

/// Load the Pi model catalog by shelling out to `pi --list-models` and parsing
/// the table's `provider model …` rows. Used only when the on-disk files are
/// unavailable. Fragile (human table, no JSON flag), hence a fallback.
pub fn load_from_cli(pi_bin: &str) -> Option<Vec<ModelInfo>> {
    let out = Command::new(pi_bin).arg("--list-models").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut models: Vec<ModelInfo> = Vec::new();
    for (i, line) in stdout.lines().enumerate() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        // Skip the header row and any blank/short line.
        if i == 0 || cols.len() < 2 || cols[0] == "provider" {
            continue;
        }
        models.push(ModelInfo {
            id: format!("{}/{}", cols[0], cols[1]),
            efforts: default_pi_efforts(),
        });
    }
    if models.is_empty() {
        return None;
    }
    models.sort_by(|a, b| a.id.cmp(&b.id));
    Some(models)
}

/// The live Pi model catalog: on-disk files first, then `pi --list-models`,
/// else empty (caller keeps the static free-form catalog).
///
/// Live discovery is **disabled** when `agent_dir` is `None` — the daemon only
/// sets it at startup; tests leave it unset and get the static catalog.
pub fn live_models(agent_dir: Option<&Path>, pi_bin: &str) -> Vec<ModelInfo> {
    let Some(dir) = agent_dir else {
        return Vec::new();
    };
    if let Some(models) = load_from_files(dir) {
        return models;
    }
    load_from_cli(pi_bin).unwrap_or_default()
}
