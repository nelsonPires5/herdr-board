//! Socket-path resolution, deadline configuration, and the thin herdr-specific
//! layer over [`board_ipc::LocalStream`].
//!
//! The platform transport (an AF_UNIX socket on unix, a named pipe on
//! Windows) lives in `board-ipc`, including the deadline connect and the
//! readiness poll. This module only maps its `io::Error`s onto
//! [`HerdrError`] and resolves the default endpoint path. Endpoint paths stay
//! on-disk paths everywhere (Herdr's `HERDR_SOCKET_PATH`); the Windows
//! `\\.\pipe\` translation happens inside `board-ipc` at connect.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use board_ipc::LocalStream;

use crate::error::{HerdrError, Result};

// -- socket-path resolution ---------------------------------------------------

/// Default socket path: `$HERDR_SOCKET_PATH` (herdr's canonical variable,
/// injected into panes/plugins so named sessions resolve to their own socket),
/// else `$HERDR_SOCKET` (this crate's override), else the default session's
/// socket — `~/.config/herdr/herdr.sock` on unix,
/// `%APPDATA%\herdr\herdr.sock` on Windows (an on-disk marker file; the
/// endpoint itself is the named pipe derived from that path).
pub fn default_socket_path() -> PathBuf {
    socket_path_from_env(|name| std::env::var_os(name))
}

/// [`default_socket_path`] over an injectable environment lookup.
fn socket_path_from_env(var: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    for name in ["HERDR_SOCKET_PATH", "HERDR_SOCKET"] {
        if let Some(path) = var(name).filter(|path| !path.is_empty()) {
            return PathBuf::from(path);
        }
    }
    default_session_socket(var)
}

#[cfg(unix)]
fn default_session_socket(var: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    // Unchanged from the unix-only resolver: an unset or non-UTF-8 `HOME`
    // falls back to `/root`.
    let home = var("HOME")
        .and_then(|home| home.into_string().ok())
        .unwrap_or_else(|| "/root".to_string());
    PathBuf::from(home).join(".config/herdr/herdr.sock")
}

#[cfg(windows)]
fn default_session_socket(var: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    // Herdr keeps per-user state under the roaming AppData folder. Without
    // `APPDATA`, derive it from the profile; never borrow a unix-style path.
    let appdata = var("APPDATA")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            var("USERPROFILE")
                .filter(|path| !path.is_empty())
                .map(|profile| PathBuf::from(profile).join("AppData").join("Roaming"))
        })
        .unwrap_or_default();
    appdata.join("herdr").join("herdr.sock")
}

// -- deadline configuration ---------------------------------------------------

/// Bounds for blocking socket operations. Long-running Herdr methods extend
/// `request` by their wire `timeout_ms` plus `method_grace`.
#[derive(Debug, Clone, Copy)]
pub struct SocketDeadlines {
    pub connect: Duration,
    pub read: Duration,
    pub write: Duration,
    pub handshake: Duration,
    pub request: Duration,
    pub method_grace: Duration,
}

impl Default for SocketDeadlines {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(2),
            read: Duration::from_secs(30),
            write: Duration::from_secs(5),
            handshake: Duration::from_secs(5),
            request: Duration::from_secs(30),
            method_grace: Duration::from_secs(5),
        }
    }
}

// -- connect with deadline ----------------------------------------------------

/// Open a blocking stream to the Herdr endpoint at `path`, bounded by
/// `timeout`.
///
/// The returned stream is in **blocking** mode with no read/write timeout so
/// callers can use std `read`/`write` with optional `set_read_timeout`. Only
/// expiry of `timeout` becomes [`HerdrError::Deadline`]; every other failure
/// (missing endpoint, refused, path too long) stays [`HerdrError::Io`].
pub(crate) fn connect_with_deadline(path: &Path, timeout: Duration) -> Result<LocalStream> {
    LocalStream::connect_with_deadline(path, timeout).map_err(|error| {
        if error.kind() == io::ErrorKind::TimedOut {
            HerdrError::Deadline {
                operation: "connect",
            }
        } else {
            HerdrError::Io(error)
        }
    })
}

// -- nonblocking toggle ------------------------------------------------------

/// Set or clear nonblocking reads on a stream (shared by every clone of it).
pub(crate) fn set_nonblocking(stream: &LocalStream, nonblocking: bool) -> Result<()> {
    stream
        .set_nonblocking(nonblocking)
        .map_err(HerdrError::from)
}

// -- small helpers ---------------------------------------------------------

/// Classify a std io error from a deadline-constrained operation.
pub(crate) fn deadline_io(error: io::Error, operation: &'static str) -> HerdrError {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        HerdrError::Deadline { operation }
    } else {
        HerdrError::Io(error)
    }
}

/// Block until `stream` is readable (data or peer close) or `deadline`
/// expires, without consuming data or touching the read timeout. Returns
/// `Ok(true)` when ready, `Ok(false)` on timeout, and `Err` on error.
pub(crate) fn poll_read_ready(stream: &LocalStream, deadline: Duration) -> Result<bool> {
    stream
        .poll_read_ready(Some(deadline))
        .map_err(HerdrError::from)
}

/// Block indefinitely until `stream` becomes readable or closes.
pub(crate) fn poll_read_ready_infinite(stream: &LocalStream) -> Result<bool> {
    stream.poll_read_ready(None).map_err(HerdrError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn explicit_socket_variables_win_in_order() {
        let both = [
            ("HERDR_SOCKET_PATH", "/a/herdr.sock"),
            ("HERDR_SOCKET", "/b/herdr.sock"),
            ("HOME", "/home/me"),
            ("APPDATA", r"C:\Users\me\AppData\Roaming"),
        ];
        assert_eq!(
            socket_path_from_env(env(&both)),
            PathBuf::from("/a/herdr.sock")
        );
        let override_only = [("HERDR_SOCKET_PATH", ""), ("HERDR_SOCKET", "/b/herdr.sock")];
        assert_eq!(
            socket_path_from_env(env(&override_only)),
            PathBuf::from("/b/herdr.sock")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_default_is_under_home_config() {
        assert_eq!(
            socket_path_from_env(env(&[("HOME", "/home/me")])),
            PathBuf::from("/home/me/.config/herdr/herdr.sock")
        );
        assert_eq!(
            socket_path_from_env(env(&[])),
            PathBuf::from("/root/.config/herdr/herdr.sock")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_default_is_under_roaming_appdata() {
        // Verified on Herdr 0.9.x for Windows: the default session's marker
        // file is `%APPDATA%\herdr\herdr.sock`.
        assert_eq!(
            socket_path_from_env(env(&[
                ("APPDATA", r"C:\Users\me\AppData\Roaming"),
                ("HOME", "/root"),
            ])),
            PathBuf::from(r"C:\Users\me\AppData\Roaming\herdr\herdr.sock")
        );
        assert_eq!(
            socket_path_from_env(env(&[("USERPROFILE", r"C:\Users\me")])),
            PathBuf::from(r"C:\Users\me\AppData\Roaming\herdr\herdr.sock")
        );
        let unresolved = socket_path_from_env(env(&[("HOME", "/root")]));
        assert_eq!(unresolved, PathBuf::from(r"herdr\herdr.sock"));
        assert!(!unresolved.starts_with("/root"));
    }
}
