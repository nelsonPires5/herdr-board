//! Windows half of the daemon lifecycle: a named pipe whose `<pid>:<nanos>`
//! marker file is its identity, a bootstrap log that refuses reparse points,
//! and a daemon detached from the CLI's console and job.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::Result;
use board_ipc::LocalStream;
use windows_sys::Win32::Foundation::{
    SetHandleInformation, ERROR_ACCESS_DENIED, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
};

use super::ListenerCheck;

/// A protected DACL naming only the current user (the `0700` analogue, as
/// the daemon applies to the same directory), so a `BOARD_LOG_DIR` outside
/// the private profile does not inherit a broader parent ACL. Links in its
/// place are refused by the caller.
pub(super) fn restrict_log_dir(dir: &Path) -> Result<()> {
    board_ipc::owner_only::restrict_dir(dir)?;
    Ok(())
}

/// Open the bootstrap log, truncated, never through a reparse point.
/// `FILE_FLAG_OPEN_REPARSE_POINT` opens a link itself rather than its target
/// (the `O_NOFOLLOW` analogue), and the handle is validated *before* the
/// truncation so nothing is ever cut through a link.
pub(super) fn open_bootstrap_log(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_file() {
        return Err(io::Error::other(
            "bootstrap log is a link or not a regular file",
        ));
    }
    file.set_len(0)?;
    Ok(file)
}

/// The detached daemon's creation flags.
///
/// - `CREATE_NEW_PROCESS_GROUP`: a Ctrl+C / Ctrl+Break in the CLI's console
///   never reaches the daemon (the unix process-group analogue).
/// - `CREATE_NO_WINDOW`: the daemon gets its own *hidden* console rather than
///   sharing the CLI's. Deliberately not `DETACHED_PROCESS`: a console-less
///   daemon would make every console child it starts (herdr, harness CLIs)
///   allocate a fresh, visible console window; a hidden console is inherited
///   instead.
/// - `CREATE_BREAKAWAY_FROM_JOB` (when `breakaway`): leave a kill-on-close job
///   the CLI may run in (a terminal or pane host), so closing it does not take
///   boardd down.
pub(super) fn creation_flags(breakaway: bool) -> u32 {
    let flags = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    if breakaway {
        flags | CREATE_BREAKAWAY_FROM_JOB
    } else {
        flags
    }
}

pub(super) fn detach(cmd: &mut Command) {
    cmd.creation_flags(creation_flags(false));
}

/// Spawn breaking away from the CLI's job; a job that forbids breakaway
/// refuses that with `ERROR_ACCESS_DENIED`, and the daemon then stays in it.
pub(super) fn spawn_detached(mut cmd: Command) -> io::Result<()> {
    stop_std_handle_inheritance();
    cmd.creation_flags(creation_flags(true));
    match cmd.spawn() {
        Ok(_) => Ok(()),
        Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
            cmd.creation_flags(creation_flags(false));
            cmd.spawn().map(drop)
        }
        Err(error) => Err(error),
    }
}

/// Keep the CLI's own stdio handles out of the daemon. std creates every
/// child with handle inheritance on, so each *inheritable* handle in this
/// process leaks into it — including the stdout/stderr pipes a caller such
/// as `$(board card list)` or `Command::output` is reading, which would then
/// never see EOF while boardd runs (on unix, `CLOEXEC` and the `dup2` of the
/// child's own stdio close them). The daemon's stdio is set explicitly and
/// std duplicates those handles itself, so the originals need not be
/// inheritable; std also re-duplicates them for any later `Stdio::inherit`
/// child.
fn stop_std_handle_inheritance() {
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: `GetStdHandle` has no preconditions. `SetHandleInformation`
        // only clears the inherit flag of a handle this process holds; a null
        // or invalid handle (no console, closed stdio) just fails, and that
        // failure leaves nothing to leak, so it is ignored.
        unsafe {
            let handle = GetStdHandle(id);
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// The marker text at the socket path when first observed (`None` when there
/// is none): what a stale cleanup must still match.
pub(super) type Identity = Option<String>;

pub(super) fn identity(path: &Path) -> Identity {
    std::fs::read_to_string(path).ok()
}

/// The daemon pid a boardd marker names: exactly `<pid>:<nanos>`, both
/// decimal. Anything else is not a boardd marker.
pub(super) fn marker_pid(text: &str) -> Option<u32> {
    let (pid, nanos) = text.split_once(':')?;
    let decimal = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !decimal(pid) || !decimal(nanos) {
        return None;
    }
    pid.parse().ok()
}

/// A fresh connection, used only to observe the listener.
pub(super) fn probe(path: &Path) -> Result<LocalStream> {
    Ok(LocalStream::connect(path)?)
}

/// Whether the live pipe is still served by the daemon the original marker
/// named. Without a marker pid, or when the server pid cannot be read, there
/// is nothing to contradict, so the listener counts as the same daemon.
pub(super) fn serves_identity(stream: &LocalStream, original: &Identity) -> bool {
    let Some(pid) = original.as_deref().and_then(marker_pid) else {
        return true;
    };
    stream
        .server_process_id()
        .map_or(true, |server| server == pid)
}

/// The pipe is absent: remove the marker only while it is still the exact
/// boardd marker first observed. A missing marker is already clean, while a
/// changed or foreign file (or a link) fails closed.
pub(super) fn remove_stale(path: &Path, original: &Identity) -> ListenerCheck {
    remove_stale_with(path, original, |path| std::fs::remove_file(path))
}

/// [`remove_stale`] with the removal injected. The exiting daemon removes
/// its own marker right after its pipe closes, so this races it: the marker
/// disappearing under the removal (`NotFound`) is a clean stop, and one that
/// is mid-deletion (`PermissionDenied`) is looked at again on the next poll
/// (`Live`) rather than reported as a replacement.
pub(super) fn remove_stale_with(
    path: &Path,
    original: &Identity,
    remove: impl FnOnce(&Path) -> io::Result<()>,
) -> ListenerCheck {
    let current = match read_marker(path) {
        Ok(None) => return ListenerCheck::Gone,
        Ok(Some(text)) => text,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return ListenerCheck::Live;
        }
        Err(_) => return ListenerCheck::Replaced,
    };
    if original.as_deref() != Some(current.as_str()) || marker_pid(&current).is_none() {
        return ListenerCheck::Replaced;
    }
    if !matches!(read_marker(path), Ok(Some(again)) if again == current) {
        return ListenerCheck::Replaced;
    }
    match remove(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return ListenerCheck::Live;
        }
        Err(_) => return ListenerCheck::Replaced,
    }
    match read_marker(path) {
        Ok(None) => ListenerCheck::Gone,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => ListenerCheck::Live,
        _ => ListenerCheck::Replaced,
    }
}

/// The marker text, `None` when nothing is at the path. A link or any other
/// non-regular file is an error, never read through.
fn read_marker(path: &Path) -> io::Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                || !metadata.is_file() =>
        {
            Err(io::Error::other(
                "boardd marker is a link or not a regular file",
            ))
        }
        Ok(_) => std::fs::read_to_string(path).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
