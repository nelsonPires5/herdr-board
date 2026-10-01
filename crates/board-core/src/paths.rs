//! Path resolution for the db, socket, log, and config, with env overrides.

use std::path::PathBuf;

use directories::BaseDirs;

/// `canonicalize`, minus Windows' `\\?\` verbatim prefix on drive paths, so a
/// canonical path stays the plain `C:\…` users type and read. UNC and other
/// verbatim forms are returned as `canonicalize` gave them.
pub fn canonical(path: &std::path::Path) -> std::io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    #[cfg(windows)]
    if let Some(plain) = canonical.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        if plain.as_bytes().get(1) == Some(&b':') {
            return Ok(PathBuf::from(plain));
        }
    }
    Ok(canonical)
}

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
/// A named session's socket lives at `…/sessions/<name>/herdr.sock` (`\`
/// separators on Windows); anything else (unset, or the plain default
/// `…/herdr.sock`) means the daemon's default
/// session, represented as `None`. This function is pure so production
/// composition can inject its result without test-time environment reads.
pub fn session_name_from_socket(path: Option<&str>) -> Option<String> {
    // Expect the tail `sessions/<name>/herdr.sock`, with `/` or `\` separators.
    let mut parts = path?.rsplit(['/', '\\']);
    (parts.next()? == "herdr.sock").then_some(())?;
    let name = parts.next()?;
    (parts.next()? == "sessions" && !name.is_empty()).then(|| name.to_string())
}

/// Open a diagnostic file owner-only without following a symlink at `path`.
/// Unix: `0600` + `O_NOFOLLOW`. Windows: the profile ACL is already owner-only;
/// open the reparse point itself and refuse it if it is a symlink.
pub fn open_private_file(
    options: &mut std::fs::OpenOptions,
    path: &std::path::Path,
) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let file = options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        Ok(file)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let file = options
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if file.metadata()?.file_type().is_symlink() {
            return Err(std::io::Error::other(
                "refusing to open a symlinked private file",
            ));
        }
        Ok(file)
    }
}

/// Config file path: `$HERDR_BOARD_CONFIG` else `<config>/config.toml`.
pub fn config_path() -> PathBuf {
    match std::env::var_os("HERDR_BOARD_CONFIG") {
        Some(p) => PathBuf::from(p),
        None => config_dir().join("config.toml"),
    }
}
