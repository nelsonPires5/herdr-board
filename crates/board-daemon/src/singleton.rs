//! Single-instance guard: an exclusive `flock` on `<db>.lock`.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// Path of the lock file for a given db path (`board.db` → `board.db.lock`).
pub fn lock_path(db_path: &Path) -> PathBuf {
    let mut s = db_path.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

/// Take the exclusive lock. `Ok(Some(file))` = acquired (keep the file alive to
/// hold the lock). `Ok(None)` = another daemon holds it (caller should exit 0).
pub fn acquire(db_path: &Path) -> anyhow::Result<Option<File>> {
    let path = lock_path(db_path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Some(file));
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(None),
        _ => Err(err.into()),
    }
}
