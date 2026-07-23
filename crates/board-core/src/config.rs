//! `~/.config/herdr-board/config.toml` loader (override via `HERDR_BOARD_CONFIG`).
//! A missing file yields defaults.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

fn default_max_concurrent() -> usize {
    3
}
fn default_idle_grace_seconds() -> u64 {
    90
}
fn default_timeout_unit_secs() -> u64 {
    60
}
fn default_local_poll_ms() -> u64 {
    2000
}
fn default_tick_ms() -> u64 {
    1000
}

/// The kind of process spawner used by the daemon.
///
/// This lives in `board-core` because it is part of the on-disk configuration,
/// rather than a daemon implementation detail. Keeping it typed means a bad
/// value in `config.toml` is reported instead of silently falling back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpawnerKind {
    /// Launch agents as herdr panes (the production default).
    #[default]
    Herdr,
    /// Launch agents as plain child processes.
    Local,
}

/// Settings in the root `[daemon]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default)]
    pub spawner: SpawnerKind,
    #[serde(default = "default_timeout_unit_secs")]
    pub timeout_unit_secs: u64,
    #[serde(default = "default_local_poll_ms")]
    pub local_poll_ms: u64,
    #[serde(default = "default_tick_ms")]
    pub tick_ms: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            spawner: SpawnerKind::default(),
            timeout_unit_secs: default_timeout_unit_secs(),
            local_poll_ms: default_local_poll_ms(),
            tick_ms: default_tick_ms(),
        }
    }
}

/// Board configuration (the top-level fields and `[harness.*]` tables).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Global cap on concurrent runs across all spaces.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    /// Seconds an agent may sit idle (no `board done`) before it is parked awaiting review.
    #[serde(default = "default_idle_grace_seconds")]
    pub idle_grace_seconds: u64,
    /// Config-defined harnesses keyed by name (`[harness.NAME]`).
    #[serde(default)]
    pub harness: HashMap<String, HarnessDef>,
    /// Pi agent dir to read the live model catalog from (`auth.json` +
    /// `models-store.json`). `None` disables live Pi model discovery → the
    /// `pi` harness reports the static free-form catalog (`models: []`). The
    /// daemon fills this in at startup; tests leave it `None` to stay hermetic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pi_agent_dir: Option<PathBuf>,
}

/// A config-defined harness: an argv template plus an optional capability
/// declaration (consumed by [`crate::capability::capabilities_for`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessDef {
    pub argv: Vec<String>,
    /// Known model aliases this harness accepts (advisory; model strings are
    /// treated as free-form regardless).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Reasoning-effort levels this harness accepts (parsed via
    /// [`Effort::parse_str`](crate::protocol::Effort::parse_str); applied to
    /// every listed model).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<String>,
    /// Permission modes this harness accepts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permission_modes: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            max_concurrent: default_max_concurrent(),
            idle_grace_seconds: default_idle_grace_seconds(),
            harness: HashMap::new(),
            pi_agent_dir: None,
        }
    }
}

impl Config {
    /// Parse a TOML document and return its board portion.
    ///
    /// The complete document is still validated, including `[daemon]`, in the
    /// same serde pass used by [`RootConfig::from_toml`]. This projection is
    /// retained for callers that only need board settings.
    pub fn from_toml(s: &str) -> Result<Config> {
        Ok(RootConfig::from_toml(s)?.board)
    }

    /// Load from `path`; a missing file returns defaults.
    pub fn load_from(path: &Path) -> Result<Config> {
        Ok(RootConfig::load_from(path)?.board)
    }

    /// Load from the resolved config path (env override then XDG).
    pub fn load() -> Result<Config> {
        Ok(RootConfig::load()?.board)
    }
}

/// The complete configuration document.
///
/// Board settings intentionally remain at the TOML document root for
/// backwards compatibility; daemon settings occupy `[daemon]`. The flatten is
/// important: it lets one serde pass validate board, harness, and daemon
/// values together.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootConfig {
    #[serde(flatten)]
    pub board: Config,
    #[serde(default)]
    pub daemon: DaemonConfig,
}

impl RootConfig {
    /// Parse the complete configuration document from TOML.
    pub fn from_toml(s: &str) -> Result<Self> {
        toml::from_str(s).map_err(|e| Error::Config(e.to_string()))
    }

    /// Load the complete document from `path`; a missing file returns all
    /// defaults. Existing files are never treated as optional: malformed TOML
    /// and type errors are returned as [`Error::Config`].
    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_toml(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// Load from the resolved config path (env override then XDG).
    pub fn load() -> Result<Self> {
        Self::load_from(&crate::paths::config_path())
    }
}

/// Backwards-compatible name for callers that refer to the board section as a
/// board config.
pub type BoardConfig = Config;
