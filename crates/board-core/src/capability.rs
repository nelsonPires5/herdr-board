//! Harness capability catalog + run-pane naming.
//!
//! Built-in harness catalogs are intentionally small and static. Pi models stay
//! free-form because they depend on provider/auth/user configuration; Claude's
//! aliases are field-verified against Claude CLI 2.1.209. Config-defined
//! harnesses declare capabilities in `[harness.NAME]`.

use serde::{Deserialize, Serialize};

use crate::config::{Config, HarnessDef};
use crate::harness::BUILTIN_HARNESSES;
use crate::protocol::Effort;

// ---------------------------------------------------------------------------
// HarnessMeta trait — the uniform adapter interface
// ---------------------------------------------------------------------------

/// The uniform capability interface every harness adapter must expose.
///
/// Built-ins (`pi`, `claude`) and config-defined harnesses all implement this;
/// the daemon turns a `dyn HarnessMeta` into the wire [`HarnessCapabilities`]
/// snapshot served by `harness.capabilities`. The TUI/CLI never see the trait —
/// they consume the snapshot — so the trait is purely the daemon-side adapter
/// contract that guarantees a single source of truth for models/efforts/
/// permissions.
///
/// `efforts(None)` is the default/free-form effort set (used when the model is
/// omitted or entered free-form); `efforts(Some(id))` is authoritative for a
/// known model alias and otherwise falls back to the default set.
pub trait HarnessMeta {
    /// Harness id (e.g. `pi`, `claude`, or a config-defined name).
    fn id(&self) -> &str;
    /// Known model aliases with the efforts each accepts. *Not* exhaustive when
    /// [`HarnessMeta::model_freeform`] is true.
    fn models(&self) -> Vec<ModelInfo>;
    /// Efforts available for `model` (`None`/unknown = the default set).
    fn efforts(&self, model: Option<&str>) -> Vec<Effort>;
    /// Permission modes the harness understands (empty = none, e.g. Pi).
    fn permissions(&self) -> Vec<String>;
    /// Whether arbitrary model strings are accepted beyond [`models`].
    fn model_freeform(&self) -> bool;
}

// ---------------------------------------------------------------------------
// Capability catalog (wire DTO built from a HarnessMeta)
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
/// well-known aliases while any model string is still accepted. This is the
/// serializable snapshot of a [`HarnessMeta`] served over the wire.
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

impl HarnessCapabilities {
    /// Build the wire snapshot from any `HarnessMeta` adapter.
    pub fn from_meta(m: &dyn HarnessMeta) -> HarnessCapabilities {
        HarnessCapabilities {
            harness: m.id().to_string(),
            models: m.models(),
            model_freeform: m.model_freeform(),
            default_efforts: m.efforts(None),
            permission_modes: m.permissions(),
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in adapter impls
// ---------------------------------------------------------------------------

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

/// Built-in `pi` harness adapter (zero-sized). Models are user/provider-defined
/// and therefore free-form; thinking is valid for omitted and explicit model
/// ids; Pi has no board-level tool permission mode.
pub struct Pi;

impl HarnessMeta for Pi {
    fn id(&self) -> &str {
        "pi"
    }
    fn models(&self) -> Vec<ModelInfo> {
        Vec::new()
    }
    fn efforts(&self, _model: Option<&str>) -> Vec<Effort> {
        [
            Effort::Off,
            Effort::Minimal,
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max,
        ]
        .to_vec()
    }
    fn permissions(&self) -> Vec<String> {
        Vec::new()
    }
    fn model_freeform(&self) -> bool {
        true
    }
}

/// Built-in `claude` harness adapter (zero-sized, claude CLI 2.1.209).
///
/// `--model` is free-form (aliases fable/opus/sonnet/haiku plus full ids, no
/// client-side validation); `--effort` accepts all five levels for every model;
/// `--permission-mode` is the fixed enum above.
pub struct Claude;

impl HarnessMeta for Claude {
    fn id(&self) -> &str {
        "claude"
    }
    fn models(&self) -> Vec<ModelInfo> {
        ["fable", "opus", "sonnet", "haiku"]
            .into_iter()
            .map(|id| ModelInfo {
                id: id.to_string(),
                efforts: CLAUDE_EFFORTS.to_vec(),
            })
            .collect()
    }
    fn efforts(&self, model: Option<&str>) -> Vec<Effort> {
        // Every known claude model accepts the full ascending ladder; an
        // unknown/free-form model gets the same default set.
        let _ = model;
        CLAUDE_EFFORTS.to_vec()
    }
    fn permissions(&self) -> Vec<String> {
        CLAUDE_PERMISSION_MODES
            .iter()
            .map(|s| s.to_string())
            .collect()
    }
    fn model_freeform(&self) -> bool {
        true
    }
}

/// Owning adapter for a config-defined harness (`[harness.NAME]`).
pub struct ConfigHarness {
    name: String,
    def: HarnessDef,
}

impl HarnessMeta for ConfigHarness {
    fn id(&self) -> &str {
        &self.name
    }
    fn models(&self) -> Vec<ModelInfo> {
        let efforts = self.parsed_efforts();
        self.def
            .models
            .iter()
            .map(|id| ModelInfo {
                id: id.clone(),
                efforts: efforts.clone(),
            })
            .collect()
    }
    fn efforts(&self, model: Option<&str>) -> Vec<Effort> {
        // A declared model carries its own efforts; otherwise the declared
        // default set (unparseable entries dropped).
        if let Some(id) = model {
            if let Some(m) = self.models().into_iter().find(|m| m.id == id) {
                return m.efforts;
            }
        }
        self.parsed_efforts()
    }
    fn permissions(&self) -> Vec<String> {
        self.def.permission_modes.clone()
    }
    fn model_freeform(&self) -> bool {
        // Config-defined harnesses always accept arbitrary model strings.
        true
    }
}

impl ConfigHarness {
    fn parsed_efforts(&self) -> Vec<Effort> {
        self.def
            .efforts
            .iter()
            .filter_map(|e| Effort::parse_str(e))
            .collect()
    }
}

/// Resolve the [`HarnessMeta`] adapter for a built-in or config-defined harness.
/// Unknown harness → `None`.
pub fn meta_for(harness: &str, config: &Config) -> Option<Box<dyn HarnessMeta>> {
    match harness {
        "pi" => Some(Box::new(Pi)),
        "claude" => Some(Box::new(Claude)),
        _ => config.harness.get(harness).map(|def| {
            Box::new(ConfigHarness {
                name: harness.to_string(),
                def: def.clone(),
            }) as Box<dyn HarnessMeta>
        }),
    }
}

/// Every harness the daemon knows about: built-ins (`pi`, `claude`) plus every
/// config-defined `[harness.NAME]`, sorted and de-duplicated. Drives the
/// `harness.list` RPC and the harness/harness-override selects in the TUI.
pub fn available_harnesses(config: &Config) -> Vec<String> {
    let mut out: Vec<String> = BUILTIN_HARNESSES.iter().map(|s| s.to_string()).collect();
    out.extend(config.harness.keys().cloned());
    out.sort();
    out.dedup();
    out
}

// ---------------------------------------------------------------------------
// Wire-snapshot constructors (kept for back-comat / tests)
// ---------------------------------------------------------------------------

/// Builtin capabilities for the `claude` harness (claude CLI 2.1.209).
pub fn claude_capabilities() -> HarnessCapabilities {
    HarnessCapabilities::from_meta(&Claude)
}

/// Built-in Pi capabilities.
pub fn pi_capabilities() -> HarnessCapabilities {
    HarnessCapabilities::from_meta(&Pi)
}

/// Resolve capabilities for a built-in or config-defined harness via its
/// [`HarnessMeta`] adapter. Unknown harness → `None`.
pub fn capabilities_for(harness: &str, config: &Config) -> Option<HarnessCapabilities> {
    meta_for(harness, config).map(|m| HarnessCapabilities::from_meta(m.as_ref()))
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
