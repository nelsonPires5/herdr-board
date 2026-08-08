//! Path resolution for the db, socket, log, and config, with env overrides.

use std::path::PathBuf;

use directories::BaseDirs;

/// XDG data dir: `<data>/herdr-board` (e.g. `~/.local/share/herdr-board`).
pub fn data_dir() -> PathBuf {
    match BaseDirs::new() {
        Some(b) => b.data_dir().join("herdr-board"),
        None => PathBuf::from(".herdr-board"),
    }
}

/// XDG config dir: `<config>/herdr-board` (e.g. `~/.config/herdr-board`).
pub fn config_dir() -> PathBuf {
    match BaseDirs::new() {
        Some(b) => b.config_dir().join("herdr-board"),
        None => PathBuf::from(".herdr-board"),
    }
}

/// SQLite db path: `$BOARD_DB` else `<data>/board.db`.
pub fn db_path() -> PathBuf {
    match std::env::var_os("BOARD_DB") {
        Some(p) => PathBuf::from(p),
        None => data_dir().join("board.db"),
    }
}

/// Unix socket path: `$BOARD_SOCKET` else `<data>/boardd.sock`.
pub fn socket_path() -> PathBuf {
    match std::env::var_os("BOARD_SOCKET") {
        Some(p) => PathBuf::from(p),
        None => data_dir().join("boardd.sock"),
    }
}

/// Structured daemon log directory: `$BOARD_LOG_DIR` else `<data>/logs`.
pub fn log_dir() -> PathBuf {
    match std::env::var_os("BOARD_LOG_DIR") {
        Some(p) => PathBuf::from(p),
        None => data_dir().join("logs"),
    }
}

/// Bounded pre-subscriber diagnostics for an auto-started daemon.
pub fn bootstrap_log_path() -> PathBuf {
    log_dir().join("bootstrap.log")
}

/// Legacy append-only path from releases before daily structured diagnostics.
pub fn legacy_log_path() -> PathBuf {
    data_dir().join("daemon.log")
}

/// Parse a herdr session name from a `HERDR_SOCKET_PATH` value.
///
/// A named session's socket lives at `…/sessions/<name>/herdr.sock`; anything
/// else (unset, or the plain default `…/herdr.sock`) means the daemon's default
/// session, represented as `None`. This function is pure so production
/// composition can inject its result without test-time environment reads.
pub fn session_name_from_socket(path: Option<&str>) -> Option<String> {
    // Expect the tail `sessions/<name>/herdr.sock`.
    let rest = path?.strip_suffix("/herdr.sock")?;
    let (parent, name) = rest.rsplit_once('/')?;
    let last_seg = parent.rsplit('/').next().unwrap_or(parent);
    (last_seg == "sessions" && !name.is_empty()).then(|| name.to_string())
}

/// Config file path: `$HERDR_BOARD_CONFIG` else `<config>/config.toml`.
pub fn config_path() -> PathBuf {
    match std::env::var_os("HERDR_BOARD_CONFIG") {
        Some(p) => PathBuf::from(p),
        None => config_dir().join("config.toml"),
    }
}
