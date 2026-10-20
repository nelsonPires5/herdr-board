//! unix half of the daemon lifecycle: a socket file whose inode is its
//! identity, a private bootstrap log, and an owned process group.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::Result;
use board_ipc::LocalStream;

use super::ListenerCheck;

/// The diagnostic log directory is owner-only.
pub(super) fn restrict_log_dir(dir: &Path) -> Result<()> {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Open the bootstrap log truncated, owner-only, never through a symlink.
pub(super) fn open_bootstrap_log(path: &Path) -> std::io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

/// Detach the daemon child without a double-fork or a session-wide `setsid`.
/// The child is its own process-group leader (`setpgid(0, 0)` via
/// [`CommandExt::process_group`]) and is therefore independent of the CLI's
/// process group while remaining addressable by its exact PID for diagnostics.
/// The process group is not used as a broad cleanup authority.
pub(super) fn detach(cmd: &mut Command) {
    // One child, one owned process group. Do not replace this with a
    // double-fork/setsid sequence: it would lose the exact child identity that
    // callers and the safe harness use for ownership checks.
    cmd.process_group(0);
}

pub(super) fn spawn_detached(mut cmd: Command) -> std::io::Result<()> {
    cmd.spawn()?;
    Ok(())
}

/// The socket file's identity: what a stale cleanup must still match.
pub(super) type Identity = Option<FileIdentity>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FileIdentity {
    device: u64,
    inode: u64,
    file_type: u32,
}

const SOCKET_FILE_TYPE: u32 = 0o140000;

pub(super) fn identity(path: &Path) -> Identity {
    std::fs::symlink_metadata(path)
        .ok()
        .map(|metadata| FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            file_type: metadata.mode() & 0o170000,
        })
}

/// A fresh connection, used only to observe the listener.
pub(super) fn probe(path: &Path) -> Result<LocalStream> {
    Ok(LocalStream::connect(path)?)
}

/// A live socket carries no process identity to compare; replacement is
/// detected once the listener is gone (see [`remove_stale`]).
pub(super) fn serves_identity(_stream: &LocalStream, _original: &Identity) -> bool {
    true
}

/// The endpoint is absent: unlink the socket file only while it is still the
/// exact socket inode first observed. A missing path is already clean, while
/// a replacement (including a non-socket) fails closed.
pub(super) fn remove_stale(path: &Path, original: &Identity) -> ListenerCheck {
    let Some(current) = identity(path) else {
        return ListenerCheck::Gone;
    };
    if *original != Some(current) || current.file_type != SOCKET_FILE_TYPE {
        return ListenerCheck::Replaced;
    }
    if identity(path) != Some(current) {
        return ListenerCheck::Replaced;
    }
    if std::fs::remove_file(path).is_ok() && identity(path).is_none() {
        ListenerCheck::Gone
    } else {
        ListenerCheck::Replaced
    }
}
