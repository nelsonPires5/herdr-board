//! Single-instance guard: an exclusive `File::try_lock` (`flock` on Unix,
//! `LockFileEx` on Windows) on `<db>.lock`.

use std::fs::{File, OpenOptions};
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
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn second_acquire_reports_held() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("board.db");
        let first = super::acquire(&db).unwrap();
        assert!(first.is_some());
        assert!(
            super::acquire(&db).unwrap().is_none(),
            "second daemon must exit quietly"
        );
        drop(first);
        assert!(
            super::acquire(&db).unwrap().is_some(),
            "lock released on drop"
        );
    }
}
