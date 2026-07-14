//! Harness capability catalog + run-pane naming.
//!
//! The claude CLI exposes no runtime capability query, so the catalog is static
//! (field-verified against claude CLI 2.1.209). Config-defined harnesses declare
//! their own capabilities in `[harness.NAME]` (see [`crate::config::HarnessDef`]).

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::protocol::Effort;

// ---------------------------------------------------------------------------
// Capability catalog
// ---------------------------------------------------------------------------

/// A model known to a harness, with the reasoning efforts it accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub efforts: Vec<Effort>,
}

/// What a harness can be asked for: known model aliases, whether arbitrary
/// model strings are also accepted, and the permission modes it understands.
///
/// `models` is *not* exhaustive when `model_freeform` is true — it lists the
/// well-known aliases while any model string is still accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessCapabilities {
    pub harness: String,
    pub models: Vec<ModelInfo>,
    pub model_freeform: bool,
    pub permission_modes: Vec<String>,
}

/// Every reasoning effort, ascending.
const ALL_EFFORTS: [Effort; 5] = [
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// The claude CLI permission-mode enum (exact casing; there is no `default`
/// literal — omitting the flag is the default).
const CLAUDE_PERMISSION_MODES: [&str; 6] = [
    "acceptEdits",
    "auto",
    "bypassPermissions",
    "manual",
    "dontAsk",
    "plan",
];

/// Builtin capabilities for the `claude` harness (claude CLI 2.1.209).
///
/// `--model` is free-form (aliases fable/opus/sonnet/haiku plus full ids, no
/// client-side validation); `--effort` accepts all five levels for every model;
/// `--permission-mode` is the fixed enum above.
pub fn claude_capabilities() -> HarnessCapabilities {
    let models = ["fable", "opus", "sonnet", "haiku"]
        .into_iter()
        .map(|id| ModelInfo {
            id: id.to_string(),
            efforts: ALL_EFFORTS.to_vec(),
        })
        .collect();
    HarnessCapabilities {
        harness: "claude".to_string(),
        models,
        model_freeform: true,
        permission_modes: CLAUDE_PERMISSION_MODES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

/// Resolve capabilities for `harness`: the builtin `claude`, or a config-defined
/// harness (capabilities built from its `models`/`efforts`/`permission_modes`).
/// Custom harnesses are always `model_freeform`. Unknown harness → `None`.
pub fn capabilities_for(harness: &str, config: &Config) -> Option<HarnessCapabilities> {
    if harness == "claude" {
        return Some(claude_capabilities());
    }
    let def = config.harness.get(harness)?;
    let efforts: Vec<Effort> = def
        .efforts
        .iter()
        .filter_map(|e| Effort::parse_str(e))
        .collect();
    let models = def
        .models
        .iter()
        .map(|id| ModelInfo {
            id: id.clone(),
            efforts: efforts.clone(),
        })
        .collect();
    Some(HarnessCapabilities {
        harness: harness.to_string(),
        models,
        model_freeform: true,
        permission_modes: def.permission_modes.clone(),
    })
}

// ---------------------------------------------------------------------------
// Run-pane naming
// ---------------------------------------------------------------------------

/// Slug length cap for a run-pane name (keeps herdr agent names tidy).
const SLUG_MAX: usize = 24;

/// Turn a column name into a pane-name slug: lowercased, every run of
/// non-ascii-alphanumeric characters collapsed to a single `-`, trimmed of
/// leading/trailing `-`, truncated to [`SLUG_MAX`] chars without ending on `-`.
fn column_slug(column_name: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in column_name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let mut out: String = slug.trim_matches('-').chars().take(SLUG_MAX).collect();
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Stable run-pane name: `card-<id>-<column-slug>` (e.g. `card-14-execute`).
/// An empty slug yields just `card-<id>`.
pub fn run_pane_name(card_id: i64, column_name: &str) -> String {
    let slug = column_slug(column_name);
    if slug.is_empty() {
        format!("card-{card_id}")
    } else {
        format!("card-{card_id}-{slug}")
    }
}

/// Collision-fallback variant: [`run_pane_name`] plus a `-r<run_id>` suffix.
/// (herdr agent names are exclusive while a pane is open.)
pub fn run_pane_name_unique(card_id: i64, column_name: &str, run_id: i64) -> String {
    format!("{}-r{run_id}", run_pane_name(card_id, column_name))
}
