//! Single-instance guard: an exclusive, non-blocking lock on `<db>.lock`.
//!
//! `File::try_lock` is `flock(LOCK_EX | LOCK_NB)` on unix — exactly the call
//! this guard always made — and `LockFileEx(EXCLUSIVE | FAIL_IMMEDIATELY)` on
//! Windows. Both locks belong to the open file, conflict across separate opens
//! (even within one process), and are released when the file is closed or the
//! process dies, so a crashed daemon never leaves a stale lock behind.

use std::fs::{File, OpenOptions, TryLockError};
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
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_second_acquire_loses_until_the_holder_releases() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("board.db");

        let held = super::acquire(&db).unwrap().expect("first acquire wins");
        assert!(
            super::acquire(&db).unwrap().is_none(),
            "a second daemon must see the lock as held"
        );
        assert!(super::lock_path(&db).is_file());

        drop(held);
        assert!(
            super::acquire(&db).unwrap().is_some(),
            "closing the holder's file releases the lock"
        );
    }

    #[test]
    fn acquire_creates_the_missing_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("nested").join("board.db");

        let held = super::acquire(&db).unwrap();
        assert!(held.is_some());
        assert!(super::lock_path(&db).is_file());
    }
}
