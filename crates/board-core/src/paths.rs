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

/// Daemon log path: `<data>/daemon.log`.
pub fn log_path() -> PathBuf {
    data_dir().join("daemon.log")
}

/// Config file path: `$HERDR_BOARD_CONFIG` else `<config>/config.toml`.
pub fn config_path() -> PathBuf {
    match std::env::var_os("HERDR_BOARD_CONFIG") {
        Some(p) => PathBuf::from(p),
        None => config_dir().join("config.toml"),
    }
}
