//! `~/.config/herdr-board/config.toml` loader (override via `HERDR_BOARD_CONFIG`).
//! A missing file yields defaults.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

fn default_max_concurrent() -> usize {
    3
}
fn default_idle_grace_seconds() -> u64 {
    90
}

/// Daemon configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Global cap on concurrent runs across all spaces.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    /// Seconds an agent may sit idle (no `board done`) before a run is marked `lost`.
    #[serde(default = "default_idle_grace_seconds")]
    pub idle_grace_seconds: u64,
    /// Config-defined harnesses keyed by name (`[harness.NAME]`).
    #[serde(default)]
    pub harness: HashMap<String, HarnessDef>,
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
        }
    }
}

impl Config {
    /// Parse config from a TOML string.
    pub fn from_toml(s: &str) -> Result<Config> {
        toml::from_str(s).map_err(|e| Error::Config(e.to_string()))
    }

    /// Load from `path`; a missing file returns defaults.
    pub fn load_from(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(s) => Config::from_toml(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// Load from the resolved config path (env override then XDG).
    pub fn load() -> Result<Config> {
        Config::load_from(&crate::paths::config_path())
    }
}
