//! The single gated way to open a Herdr request connection.
//!
//! The board-herdr client pins herdr-board to the supported Herdr socket
//! protocol (22) and rejects every other one. A Herdr version different from
//! the 0.9.0 reference is accepted with a warning when it speaks protocol 22.
//! The daemon opens a fresh connection per operation, so the gate has to live
//! at the connect, not at a single startup check.
//!
//! Socket identity lives here too ([`normalize_socket`], [`same_socket`]). On
//! unix the endpoint *is* the socket file, so canonicalizing it is exact. On
//! Windows the endpoint is the named pipe `\\.\pipe\` + the socket path string
//! (the on-disk `.sock` is only Herdr's `<pid>:<nanos>` marker file), so the
//! path is normalized lexically instead: resolving links or 8.3 names would
//! change the string and with it the pipe the connect reaches.

use std::path::{Path, PathBuf};

use board_herdr::HerdrClient;

/// Connect to `socket` and require the pinned Herdr protocol before any other
/// request can reach it.
///
/// The raw [`board_herdr::HerdrError`] is returned so each caller keeps the
/// mapping its own layer already uses (`board_core::Error` in `ops`,
/// `ProbeFailure` in the supervisor, `board_herdr::Result` in the watchers)
/// while the gate itself stays identical everywhere.
pub(crate) fn connect_checked(socket: &Path) -> board_herdr::Result<HerdrClient> {
    let mut client = HerdrClient::connect(socket)?;
    client.require_supported_protocol()?;
    Ok(client)
}

/// [`connect_checked`] for the `anyhow` edges (the spawner and the run-pane
/// rescue), keeping the two distinct contexts those sites already report:
/// an unreachable socket and an incompatible one are different operator
/// problems. `purpose` names what the connection was about to do.
pub(crate) fn connect_checked_for(socket: &Path, purpose: &str) -> anyhow::Result<HerdrClient> {
    let mut client = HerdrClient::connect(socket).map_err(|error| {
        let message = error.to_string();
        anyhow::Error::new(error).context(format!("herdr unavailable: {message}"))
    })?;
    client.require_supported_protocol().map_err(|error| {
        let message = error.to_string();
        anyhow::Error::new(error).context(format!(
            "checking Herdr protocol before {purpose}: {message}"
        ))
    })?;
    Ok(client)
}

/// Normalize a caller-supplied Herdr socket path before it is connected to,
/// so two spellings of the same socket compare equal (with [`same_socket`])
/// and a missing one is reported as `herdr unavailable` rather than as an
/// opaque connect failure. `kind` names the socket in that message (e.g.
/// `origin`, `target`).
///
/// unix: `canonicalize` (the socket file must exist). Windows: an absolute
/// path without a `\\?\` prefix whose marker file must exist; links are not
/// resolved because the pipe name is the path string itself.
pub(crate) fn normalize_socket(path: &Path, kind: &str) -> board_core::Result<PathBuf> {
    let unavailable = |e: std::io::Error| {
        board_core::Error::HerdrUnavailable(format!(
            "{kind} Herdr socket '{}' is unavailable: {e}",
            path.display()
        ))
    };
    #[cfg(unix)]
    {
        path.canonicalize().map_err(unavailable)
    }
    #[cfg(windows)]
    {
        let normalized = lexical(path).map_err(unavailable)?;
        std::fs::metadata(&normalized).map_err(unavailable)?;
        Ok(normalized)
    }
}

/// Whether two Herdr socket paths name the same endpoint, without touching
/// the filesystem.
///
/// unix: plain path equality, exactly as before (callers that need symlinks
/// resolved go through [`normalize_socket`] first). Windows: pipe names are
/// case-insensitive and Win32 normalizes separators, so both paths are made
/// absolute and compared case-insensitively — `C:\Users\…\herdr.sock` as
/// `herdr session list` reports it equals `c:/users/…/herdr.sock`.
pub(crate) fn same_socket(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        a == b
    }
    #[cfg(windows)]
    {
        endpoint_key(a) == endpoint_key(b)
    }
}

/// Absolute, separator-normalized form of `path`, minus any `\\?\` prefix.
#[cfg(windows)]
fn lexical(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    Ok(dunce::simplified(&absolute).to_path_buf())
}

/// The case-folded comparison key of a Windows socket path.
#[cfg(windows)]
fn endpoint_key(path: &Path) -> String {
    let normalized = lexical(path).unwrap_or_else(|_| path.to_path_buf());
    normalized
        .to_string_lossy()
        .replace('/', "\\")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_socket_is_reported_as_herdr_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("herdr.sock");

        let error = normalize_socket(&missing, "origin").unwrap_err();
        assert!(
            matches!(error, board_core::Error::HerdrUnavailable(_)),
            "{error:?}"
        );
        assert!(error.to_string().contains("origin Herdr socket"), "{error}");
    }

    #[test]
    fn identical_spellings_are_the_same_socket() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("herdr.sock");
        let b = dir.path().join("other.sock");
        assert!(same_socket(&a, &a.clone()));
        assert!(!same_socket(&a, &b));
    }

    #[cfg(unix)]
    #[test]
    fn unix_normalization_resolves_symlinks_and_compares_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("herdr.sock");
        std::fs::write(&real, "").unwrap();
        let link = dir.path().join("link.sock");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let via_link = normalize_socket(&link, "origin").unwrap();
        let direct = normalize_socket(&real, "target").unwrap();
        assert!(same_socket(&via_link, &direct));
        // unix paths are case-sensitive: no folding is applied.
        assert!(!same_socket(
            Path::new("/run/herdr.sock"),
            Path::new("/run/HERDR.sock")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_normalization_is_lexical_and_strips_the_verbatim_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("herdr.sock");
        std::fs::write(&marker, "1234:5678").unwrap();

        let forward = PathBuf::from(marker.to_string_lossy().replace('\\', "/"));
        let normalized = normalize_socket(&forward, "origin").unwrap();
        let text = normalized.to_string_lossy();
        assert!(!text.starts_with(r"\\?\"), "{text}");
        assert!(!text.contains('/'), "{text}");
        assert!(normalized.is_absolute());

        let verbatim = PathBuf::from(format!(r"\\?\{}", marker.display()));
        let from_verbatim = normalize_socket(&verbatim, "origin").unwrap();
        assert!(same_socket(&normalized, &from_verbatim));
    }

    #[cfg(windows)]
    #[test]
    fn windows_sockets_compare_case_and_separator_insensitively() {
        // `herdr session list --json` reports backslashed on-disk paths, while
        // HERDR_SOCKET_PATH may be spelled with forward slashes or other case.
        let listed = Path::new(r"C:\Users\jon\AppData\Roaming\herdr\herdr.sock");
        let configured = Path::new("c:/users/JON/appdata/roaming/herdr/HERDR.sock");
        assert!(same_socket(listed, configured));
        assert!(!same_socket(
            listed,
            Path::new(r"C:\Users\jon\AppData\Roaming\herdr\sessions\work\herdr.sock")
        ));
    }
}
