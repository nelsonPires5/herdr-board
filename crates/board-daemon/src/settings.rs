//! Runtime daemon settings derived from the typed root configuration.
//!
//! The TOML document is parsed by `board_core::config::RootConfig`. This
//! module only applies the environment overrides, after parsing, so malformed
//! config cannot be hidden by a second best-effort parse.

use std::path::Path;

pub use board_core::config::SpawnerKind;
use board_core::config::{DaemonConfig, RootConfig};
use board_core::{Error, Result};

/// An environment lookup supplied by the caller.
///
/// Keeping lookup injectable makes settings tests hermetic and avoids tests
/// racing over the process-global environment. The daemon uses [`ProcessEnv`]
/// at its process boundary.
pub trait EnvLookup {
    fn var(&self, key: &str) -> Option<String>;
}

impl<F> EnvLookup for F
where
    F: Fn(&str) -> Option<String>,
{
    fn var(&self, key: &str) -> Option<String> {
        self(key)
    }
}

/// Process environment adapter used by the real daemon.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvLookup for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Resolved daemon settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonSettings {
    pub spawner: SpawnerKind,
    /// Seconds per column `timeout_minutes` unit. Default 60 (real minutes);
    /// tests set `BOARD_TIMEOUT_UNIT_SECS=1` to make timeouts fire in seconds.
    pub timeout_unit_secs: u64,
    /// LocalSpawner liveness poll interval (ms). Default 2000.
    pub local_poll_ms: u64,
    /// Timeout/idle ticker interval (ms). Default 1000.
    pub tick_ms: u64,
}

impl Default for DaemonSettings {
    fn default() -> DaemonSettings {
        DaemonSettings {
            spawner: SpawnerKind::Herdr,
            timeout_unit_secs: 60,
            local_poll_ms: 2000,
            tick_ms: 1000,
        }
    }
}

impl DaemonSettings {
    /// Resolve runtime settings from typed daemon config and injected env.
    /// Environment values have precedence over TOML values.
    pub fn from_config(config: &DaemonConfig, env: &dyn EnvLookup) -> Result<Self> {
        let mut settings = Self {
            spawner: config.spawner,
            timeout_unit_secs: config.timeout_unit_secs.max(1),
            local_poll_ms: config.local_poll_ms.max(1),
            tick_ms: config.tick_ms.max(1),
        };

        if let Some(value) = env.var("BOARD_SPAWNER") {
            settings.spawner = parse_spawner(&value)?;
        }
        if let Some(value) = env.var("BOARD_TIMEOUT_UNIT_SECS") {
            settings.timeout_unit_secs = parse_u64("BOARD_TIMEOUT_UNIT_SECS", &value)?;
        }
        if let Some(value) = env.var("BOARD_LOCAL_POLL_MS") {
            settings.local_poll_ms = parse_u64("BOARD_LOCAL_POLL_MS", &value)?;
        }
        if let Some(value) = env.var("BOARD_TICK_MS") {
            settings.tick_ms = parse_u64("BOARD_TICK_MS", &value)?;
        }

        Ok(settings)
    }

    /// Resolve settings from a complete, already-parsed root config.
    pub fn from_root(root: &RootConfig, env: &dyn EnvLookup) -> Result<Self> {
        Self::from_config(&root.daemon, env)
    }

    /// Load and parse a root config once, then apply process environment
    /// overrides. Prefer [`Self::from_root`] when the board config is also
    /// needed by the caller.
    #[allow(dead_code)]
    pub fn load(config_path: &Path) -> Result<Self> {
        Self::load_with_env(config_path, &ProcessEnv)
    }

    /// Testable form of [`Self::load`] with an injected environment lookup.
    #[allow(dead_code)]
    pub fn load_with_env(config_path: &Path, env: &dyn EnvLookup) -> Result<Self> {
        let root = RootConfig::load_from(config_path)?;
        Self::from_root(&root, env)
    }
}

fn parse_spawner(value: &str) -> Result<SpawnerKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "local" => Ok(SpawnerKind::Local),
        "herdr" => Ok(SpawnerKind::Herdr),
        _ => Err(Error::Config(format!(
            "invalid BOARD_SPAWNER value {value:?}; expected `herdr` or `local`"
        ))),
    }
}

fn parse_u64(key: &str, value: &str) -> Result<u64> {
    value.parse::<u64>().map(|n| n.max(1)).map_err(|_| {
        Error::Config(format!(
            "invalid {key} value {value:?}; expected a positive integer"
        ))
    })
}

#[cfg(test)]
mod tests;
