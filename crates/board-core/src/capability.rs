//! Harness capability catalog + run-pane naming.
//!
//! Built-in harness catalogs are intentionally small and static. Pi models stay
//! free-form because they depend on provider/auth/user configuration; Claude's
//! aliases are field-verified against Claude CLI 2.1.209. Config-defined
//! harnesses declare capabilities in `[harness.NAME]`.

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::harness::is_builtin_harness;
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
    /// Efforts available when the model is omitted or entered free-form.
    /// Missing on older serialized capability payloads, so default to empty.
    #[serde(default)]
    pub default_efforts: Vec<Effort>,
    pub permission_modes: Vec<String>,
}

/// Claude reasoning efforts, ascending.
const CLAUDE_EFFORTS: [Effort; 5] = [
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
            efforts: CLAUDE_EFFORTS.to_vec(),
        })
        .collect();
    HarnessCapabilities {
        harness: "claude".to_string(),
        models,
        model_freeform: true,
        default_efforts: CLAUDE_EFFORTS.to_vec(),
        permission_modes: CLAUDE_PERMISSION_MODES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

/// Built-in Pi capabilities. Models are user/provider-defined and therefore
/// free-form; thinking is valid for omitted and explicit model ids. Pi has no
/// board-level tool permission mode.
pub fn pi_capabilities() -> HarnessCapabilities {
    HarnessCapabilities {
        harness: "pi".to_string(),
        models: Vec::new(),
        model_freeform: true,
        default_efforts: vec![
            Effort::Off,
            Effort::Minimal,
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max,
        ],
        permission_modes: Vec::new(),
    }
}

/// Resolve capabilities for a built-in or config-defined harness. Custom
/// harnesses are always `model_freeform`. Unknown harness → `None`.
pub fn capabilities_for(harness: &str, config: &Config) -> Option<HarnessCapabilities> {
    if is_builtin_harness(harness) {
        return match harness {
            "pi" => Some(pi_capabilities()),
            "claude" => Some(claude_capabilities()),
            _ => None,
        };
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
        default_efforts: efforts,
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
