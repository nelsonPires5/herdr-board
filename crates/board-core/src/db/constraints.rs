//! Turning SQLite constraint failures into typed, user-facing errors.
//!
//! A UNIQUE index is the *final* guard against duplicate board and column
//! names, so ordinary user input reaches it: asking for a second `Todo` column
//! is a perfectly well-formed request that the schema refuses. Letting
//! `rusqlite`'s error propagate into [`Error::Sqlite`] would report that as
//! protocol code 5 ("internal") and print SQLite's table and column names —
//! telling a scripting agent the daemon broke and to retry a request that can
//! never succeed.
//!
//! So a duplicate name is translated here into [`Error::BadRequest`] (code 1):
//! the request itself is wrong, and only changing the requested *name* can fix
//! it. Nothing else is translated. Detection is by SQLite's extended result
//! code, never by matching the message text, and it is narrowed to
//! `SQLITE_CONSTRAINT_UNIQUE`; a CHECK, FOREIGN KEY, NOT NULL or trigger abort
//! means a caller skipped a validation or the database is damaged, which stays
//! an internal error.

use rusqlite::{ffi, ErrorCode};

use crate::{Error, Result};

/// Whether SQLite refused the write because it would duplicate a UNIQUE key.
///
/// `ErrorCode::ConstraintViolation` alone is too coarse — it also covers CHECK,
/// FOREIGN KEY, NOT NULL and `RAISE(ABORT)` — so the extended code decides.
fn is_unique_violation(error: &rusqlite::Error) -> bool {
    match error {
        rusqlite::Error::SqliteFailure(sqlite, _) => {
            sqlite.code == ErrorCode::ConstraintViolation
                && sqlite.extended_code == ffi::SQLITE_CONSTRAINT_UNIQUE
        }
        _ => false,
    }
}

/// Run a write whose UNIQUE index is a user-visible rule, reporting a duplicate
/// as [`Error::BadRequest`] with `duplicate()`'s message. Every other failure —
/// including every other constraint kind — stays [`Error::Sqlite`].
///
/// The message is built lazily so the success path formats nothing.
pub(super) fn reject_duplicate<T>(
    outcome: rusqlite::Result<T>,
    duplicate: impl FnOnce() -> String,
) -> Result<T> {
    outcome.map_err(|error| {
        if is_unique_violation(&error) {
            Error::BadRequest(duplicate())
        } else {
            Error::Sqlite(error)
        }
    })
}

/// `boards(project_id, name)` is unique (COLLATE NOCASE), so the message says
/// *in this project*: the same name on another project is legal and must not
/// read as a conflict.
pub(super) fn duplicate_column(name: &str) -> String {
    format!("column {name:?} already exists on this board; pick another name")
}

/// `boards(project_id, name)` is unique (COLLATE NOCASE) within a project.
pub(super) fn duplicate_board(name: &str) -> String {
    format!("board {name:?} already exists in this project; pick another name")
}
