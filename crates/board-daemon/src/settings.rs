//! Daemon-side settings not covered by `board_core::config::Config`.
//!
//! Spawner selection and a few test-tuning knobs. Values come from environment
//! variables first, then a `[daemon]` table in the same `config.toml` that
//! `board_core::config` reads, then defaults.

use std::path::Path;

/// Which spawner the daemon uses to launch agent processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnerKind {
    /// Launch agents as herdr panes (production default).
    Herdr,
    /// Launch agents as plain child processes (tests / headless).
    Local,
}

/// Resolved daemon settings.
#[derive(Debug, Clone)]
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
    /// Load from env + the `[daemon]` table of the config file at `config_path`.
    pub fn load(config_path: &Path) -> DaemonSettings {
        let mut s = DaemonSettings::default();

        // [daemon] table from config.toml (best effort; ignore parse errors).
        if let Ok(text) = std::fs::read_to_string(config_path) {
            if let Ok(v) = text.parse::<toml::Value>() {
                if let Some(d) = v.get("daemon") {
                    if let Some(sp) = d.get("spawner").and_then(|x| x.as_str()) {
                        if let Some(k) = parse_spawner(sp) {
                            s.spawner = k;
                        }
                    }
                    if let Some(n) = d.get("timeout_unit_secs").and_then(|x| x.as_integer()) {
                        s.timeout_unit_secs = n.max(1) as u64;
                    }
                    if let Some(n) = d.get("local_poll_ms").and_then(|x| x.as_integer()) {
                        s.local_poll_ms = n.max(1) as u64;
                    }
                    if let Some(n) = d.get("tick_ms").and_then(|x| x.as_integer()) {
                        s.tick_ms = n.max(1) as u64;
                    }
                }
            }
        }

        // Environment overrides win.
        if let Ok(sp) = std::env::var("BOARD_SPAWNER") {
            if let Some(k) = parse_spawner(&sp) {
                s.spawner = k;
            }
        }
        if let Some(n) = env_u64("BOARD_TIMEOUT_UNIT_SECS") {
            s.timeout_unit_secs = n.max(1);
        }
        if let Some(n) = env_u64("BOARD_LOCAL_POLL_MS") {
            s.local_poll_ms = n.max(1);
        }
        if let Some(n) = env_u64("BOARD_TICK_MS") {
            s.tick_ms = n.max(1);
        }
        s
    }
}

fn parse_spawner(s: &str) -> Option<SpawnerKind> {
    match s.trim().to_ascii_lowercase().as_str() {
        "local" => Some(SpawnerKind::Local),
        "herdr" => Some(SpawnerKind::Herdr),
        _ => None,
    }
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|v| v.parse().ok())
}
