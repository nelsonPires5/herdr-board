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
/// session, represented as `None`. Both `/` and `\` separate segments, so a
/// Windows value (`C:\…\herdr\sessions\<name>\herdr.sock`, which Herdr
/// serves as a named pipe of the same name) parses like its unix counterpart.
/// This function is pure so production composition can inject its result
/// without test-time environment reads.
pub fn session_name_from_socket(path: Option<&str>) -> Option<String> {
    const SEPARATORS: [char; 2] = ['/', '\\'];
    // Expect the tail `sessions/<name>/herdr.sock`.
    let rest = path?.strip_suffix("herdr.sock")?;
    let rest = rest.strip_suffix(SEPARATORS)?;
    let (parent, name) = rest.rsplit_once(SEPARATORS)?;
    let last_seg = parent.rsplit(SEPARATORS).next().unwrap_or(parent);
    (last_seg == "sessions" && !name.is_empty()).then(|| name.to_string())
}

/// The user's home directory, as the harness CLIs themselves resolve it.
///
/// - **unix**: `$HOME` verbatim (unset or empty → `None`), so `HOME=…`
///   overrides keep pointing discovery at a fixture home.
/// - **Windows**: `$HOME` is not a native concept (Git Bash sets it, a plain
///   console does not); node's `os.homedir()` and Rust CLIs use the profile
///   folder, so this does too (`directories::BaseDirs::home_dir`).
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
    }
    #[cfg(not(unix))]
    {
        BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
    }
}

/// Config file path: `$HERDR_BOARD_CONFIG` else `<config>/config.toml`.
pub fn config_path() -> PathBuf {
    match std::env::var_os("HERDR_BOARD_CONFIG") {
        Some(p) => PathBuf::from(p),
        None => config_dir().join("config.toml"),
    }
}
