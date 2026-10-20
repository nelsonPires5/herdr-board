//! Auto-start and stop for boardd, the CLI side of the daemon lifecycle.
//!
//! The decisions here are portable; what differs per platform lives behind
//! `platform`:
//!
//! - **unix** — the endpoint is a socket file. Its `(dev, inode, type)` is the
//!   identity a stale-socket cleanup must match before unlinking, and the
//!   auto-started daemon leads its own process group.
//! - **Windows** — the endpoint is a named pipe with nothing on disk; boardd
//!   writes a `<pid>:<nanos>` marker file at the socket path instead. The
//!   marker text is the identity a stale cleanup must match, and a live pipe
//!   whose server process is not the marker's pid is a replacement.
//!
//! On both, only an *absent* endpoint (`board_core::client::is_daemon_absent`)
//! means "boardd is not running". A busy or slow endpoint is a live daemon:
//! it is never answered with a second daemon or a cleanup.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use board_core::client::{is_daemon_absent, BoardClient, LocalClient};
use board_core::paths;
use serde_json::json;

use crate::render::emit_line;

#[cfg(unix)]
#[path = "daemon/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "daemon/windows.rs"]
mod platform;

/// What one connect attempt says about the daemon at an endpoint.
#[derive(Debug)]
enum Probe<C> {
    /// Connected.
    Live(C),
    /// Nothing listens there: the only outcome that may start or clean up.
    Absent,
    /// Something is there but did not accept this connection (a busy pipe, a
    /// timeout, a permission error): treated as a live daemon.
    Unreachable(anyhow::Error),
}

fn classify<C>(result: Result<C>) -> Probe<C> {
    match result {
        Ok(client) => Probe::Live(client),
        Err(error) if is_daemon_absent(&error) => Probe::Absent,
        Err(error) => Probe::Unreachable(error),
    }
}

/// Connect to the daemon socket, auto-starting the daemon if absent.
pub(crate) fn connect_or_start() -> Result<LocalClient> {
    connect_or_start_with(
        &paths::socket_path(),
        LocalClient::connect,
        || spawn_daemon().context("auto-starting boardd"),
        Duration::from_secs(3),
    )
}

/// [`connect_or_start`] with its effects injected. A daemon is spawned only
/// when the first probe finds the endpoint absent; an unreachable one is
/// waited on like a starting daemon instead, so a busy boardd never gets a
/// competing second start.
fn connect_or_start_with<C>(
    path: &Path,
    mut connect: impl FnMut(&Path) -> Result<C>,
    spawn: impl FnOnce() -> Result<()>,
    wait: Duration,
) -> Result<C> {
    let mut last_error = match classify(connect(path)) {
        Probe::Live(client) => return Ok(client),
        Probe::Absent => {
            spawn()?;
            None
        }
        Probe::Unreachable(error) => Some(error),
    };

    let deadline = Instant::now() + wait;
    let mut delay = Duration::from_millis(50);
    loop {
        std::thread::sleep(delay);
        match classify(connect(path)) {
            Probe::Live(client) => return Ok(client),
            Probe::Absent => {}
            Probe::Unreachable(error) => last_error = Some(error),
        }
        if Instant::now() >= deadline {
            let message = format!("could not connect to boardd at {}", path.display());
            return Err(match last_error {
                // Context goes on top: the boardd message stays the headline
                // and the OS cause follows as "Caused by".
                Some(cause) => cause.context(message),
                None => anyhow::anyhow!(message),
            });
        }
        delay = (delay * 2).min(Duration::from_millis(500));
    }
}

fn spawn_daemon() -> Result<()> {
    let exe = std::env::current_exe()?;
    let bootstrap_path = paths::bootstrap_log_path();
    if let Some(parent) = bootstrap_path.parent() {
        prepare_log_dir(parent)?;
    }
    platform::spawn_detached(daemon_command(&exe, &bootstrap_path)?)?;
    Ok(())
}

/// Create (or accept) the private diagnostic log directory. `symlink_metadata`
/// does not follow links, so a symlink — or, on Windows, a junction — in its
/// place is refused rather than written through.
fn prepare_log_dir(dir: &Path) -> Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            anyhow::bail!("diagnostic log path is not a directory");
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir)?;
        }
        Err(error) => return Err(error.into()),
    }
    platform::restrict_log_dir(dir)
}

/// Build the detached daemon child: stdin/stdout null, stderr into the
/// bootstrap log (truncated per start, never through a link), detached from
/// the CLI as the platform allows (see `platform::detach`). Lifecycle control
/// still goes through `daemon.stop`; the detachment is not a cleanup
/// authority.
pub(crate) fn daemon_command(exe: &Path, bootstrap_path: &Path) -> Result<Command> {
    // Truncate on each auto-start: this file contains only failures emitted
    // before the structured subscriber is available and cannot grow forever.
    let err = platform::open_bootstrap_log(bootstrap_path)?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(err);
    platform::detach(&mut cmd);
    Ok(cmd)
}

/// The result of re-checking an endpoint whose connect failed.
#[derive(Debug, PartialEq, Eq)]
enum ListenerCheck {
    Live,
    Gone,
    Replaced,
}

/// Confirm that a failed fresh connect means this exact endpoint is stale.
/// A live (or merely unreachable) endpoint is left alone; an absent one has
/// its leftover on-disk trace removed only while it still matches
/// `original`, so a replacement fails closed.
fn check_listener_after_connect_failure(
    path: &Path,
    original: &platform::Identity,
) -> ListenerCheck {
    if !matches!(classify(platform::probe(path)), Probe::Absent) {
        return ListenerCheck::Live;
    }
    platform::remove_stale(path, original)
}

/// `board daemon stop` (and the retained `board daemon --stop`): request a
/// graceful shutdown over the socket, then wait for its listener to vanish.
/// Cleanup is deliberately fail-closed: RPC errors, live listeners, and path
/// replacements are never unlinked. Like every other command, the report goes
/// through the shared output path and honors `--json`.
pub(crate) fn stop_daemon(json: bool) -> Result<()> {
    let path = paths::socket_path();
    let original = platform::identity(&path);

    let mut client = match classify(LocalClient::connect(&path)) {
        Probe::Live(client) => client,
        Probe::Absent => match check_listener_after_connect_failure(&path, &original) {
            ListenerCheck::Gone => {
                return emit_line(
                    &json!({ "stopped": true, "was_running": false }),
                    json,
                    "boardd not running",
                );
            }
            ListenerCheck::Live | ListenerCheck::Replaced => {
                bail!(
                    "could not connect to boardd at {}; socket preserved",
                    path.display()
                );
            }
        },
        Probe::Unreachable(error) => {
            return Err(error).context(format!(
                "could not connect to boardd at {}; socket preserved",
                path.display()
            ));
        }
    };

    let stop_result = client.daemon_stop();
    drop(client);
    let stop_result = stop_result.context("could not stop boardd gracefully; socket preserved")?;
    if !stop_result.stopping {
        bail!("boardd did not acknowledge stopping; socket preserved");
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut delay = Duration::from_millis(25);
    while Instant::now() < deadline {
        match classify(platform::probe(&path)) {
            Probe::Absent => match check_listener_after_connect_failure(&path, &original) {
                ListenerCheck::Gone => {
                    return emit_line(
                        &json!({ "stopped": true, "was_running": true }),
                        json,
                        "boardd stopped",
                    );
                }
                ListenerCheck::Replaced => {
                    bail!("boardd socket identity changed; socket preserved");
                }
                ListenerCheck::Live => {}
            },
            Probe::Live(stream) => {
                if !platform::serves_identity(&stream, &original) {
                    bail!("boardd socket identity changed; socket preserved");
                }
            }
            Probe::Unreachable(_) => {}
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_millis(200));
    }

    bail!("boardd acknowledged stop but is still listening; socket preserved")
}

#[cfg(test)]
#[path = "daemon/tests.rs"]
mod tests;
