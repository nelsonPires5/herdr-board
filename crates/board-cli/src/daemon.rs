use std::fs::OpenOptions;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use board_core::client::{BoardClient, UnixClient};
use board_core::paths;

/// Connect to the daemon socket, auto-starting the daemon if absent.
pub(crate) fn connect_or_start() -> Result<UnixClient> {
    let path = paths::socket_path();
    if let Ok(c) = UnixClient::connect(&path) {
        return Ok(c);
    }
    spawn_daemon().context("auto-starting boardd")?;

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut delay = Duration::from_millis(50);
    loop {
        std::thread::sleep(delay);
        if let Ok(c) = UnixClient::connect(&path) {
            return Ok(c);
        }
        if Instant::now() >= deadline {
            bail!("could not connect to boardd at {}", path.display());
        }
        delay = (delay * 2).min(Duration::from_millis(500));
    }
}

fn spawn_daemon() -> Result<()> {
    let exe = std::env::current_exe()?;
    let log_path = paths::log_path();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    daemon_command(&exe, &log_path)?.spawn()?;
    Ok(())
}

/// Build the detached daemon child without a double-fork or a session-wide
/// `setsid`. The child is its own process-group leader (`setpgid(0, 0)` via
/// [`CommandExt::process_group`]) and is therefore independent of the CLI's
/// process group while remaining addressable by its exact PID for diagnostics.
/// Lifecycle control still goes through `daemon.stop`; the process group is not
/// used as a broad cleanup authority.
pub(crate) fn daemon_command(exe: &Path, log_path: &Path) -> Result<Command> {
    let out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let err = out.try_clone()?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err);
    // One child, one owned process group. Do not replace this with a
    // double-fork/setsid sequence: it would lose the exact child identity that
    // callers and the safe harness use for ownership checks.
    cmd.process_group(0);
    Ok(cmd)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    file_type: u32,
}

const SOCKET_FILE_TYPE: u32 = 0o140000;

fn file_identity(path: &Path) -> Option<FileIdentity> {
    std::fs::symlink_metadata(path)
        .ok()
        .map(|metadata| FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            file_type: metadata.mode() & 0o170000,
        })
}

enum ListenerCheck {
    Live,
    Gone,
    Replaced,
}

/// Confirm that a failed fresh connect means this exact socket is stale. The
/// identity is checked again immediately before unlinking; a missing path is
/// already clean, while a replacement (including a non-socket) fails closed.
fn check_listener_after_connect_failure(
    path: &Path,
    original: Option<FileIdentity>,
) -> ListenerCheck {
    if UnixClient::connect(path).is_ok() {
        return ListenerCheck::Live;
    }

    let Some(current) = file_identity(path) else {
        return ListenerCheck::Gone;
    };
    if original != Some(current) || current.file_type != SOCKET_FILE_TYPE {
        return ListenerCheck::Replaced;
    }
    if file_identity(path) != Some(current) {
        return ListenerCheck::Replaced;
    }
    if std::fs::remove_file(path).is_ok() && file_identity(path).is_none() {
        ListenerCheck::Gone
    } else {
        ListenerCheck::Replaced
    }
}

/// `board daemon --stop`: request a graceful shutdown over the socket, then
/// wait for its listener to vanish. Cleanup is deliberately fail-closed: RPC
/// errors, live listeners, and path replacements are never unlinked.
pub(crate) fn stop_daemon() -> Result<()> {
    let path = paths::socket_path();
    let original = file_identity(&path);

    let mut client = match UnixClient::connect(&path) {
        Ok(c) => c,
        Err(_) => match check_listener_after_connect_failure(&path, original) {
            ListenerCheck::Gone => {
                println!("boardd not running");
                return Ok(());
            }
            ListenerCheck::Live | ListenerCheck::Replaced => {
                bail!(
                    "could not connect to boardd at {}; socket preserved",
                    path.display()
                );
            }
        },
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
        if UnixClient::connect(&path).is_err() {
            match check_listener_after_connect_failure(&path, original) {
                ListenerCheck::Gone => {
                    println!("boardd stopped");
                    return Ok(());
                }
                ListenerCheck::Replaced => {
                    bail!("boardd socket identity changed; socket preserved");
                }
                ListenerCheck::Live => {}
            }
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_millis(200));
    }

    bail!("boardd acknowledged stop but is still listening; socket preserved")
}

#[cfg(test)]
#[path = "daemon/tests.rs"]
mod tests;
