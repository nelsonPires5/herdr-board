//! The single gated way to open a Herdr request connection.
//!
//! The board-herdr client pins herdr-board to one supported Herdr release and
//! socket protocol and rejects every other one. The daemon opens a fresh
//! connection per operation,
//! so the gate has to live at the connect, not at a single startup check.

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

/// Canonicalize a caller-supplied Herdr socket path before it is connected to,
/// so two spellings of the same socket compare equal and a missing one is
/// reported as `herdr unavailable` rather than as an opaque connect failure.
/// `kind` names the socket in that message (e.g. `origin`, `target`).
pub(crate) fn normalize_socket(path: &Path, kind: &str) -> board_core::Result<PathBuf> {
    path.canonicalize().map_err(|e| {
        board_core::Error::HerdrUnavailable(format!(
            "{kind} Herdr socket '{}' is unavailable: {e}",
            path.display()
        ))
    })
}
