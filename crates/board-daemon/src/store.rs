//! Serialized access to the synchronous `board_core::db::Db`.
//!
//! `boardd` is the only SQLite writer. We wrap the (sync) `Db` in an
//! `Arc<Mutex<_>>` and expose a `lock()` guard plus a few composite queries the
//! scheduler needs. All access is short-lived: callers lock, run queries, and
//! drop the guard **before** awaiting — the guard is never held across an
//! `.await`, so no async leaks into core and the single-writer invariant holds.
//! SQLite-on-a-local-file operations are sub-millisecond, so briefly blocking a
//! worker thread here is acceptable for v1.

use std::sync::{Arc, Mutex, MutexGuard};

use board_core::db::Db;
use board_core::model::{Card, Run};
use board_core::Result;

/// A cheap-to-clone handle to the serialized store.
#[derive(Clone)]
pub struct Store {
    db: Arc<Mutex<Db>>,
}

impl Store {
    pub fn new(db: Db) -> Store {
        Store {
            db: Arc::new(Mutex::new(db)),
        }
    }

    /// Lock the underlying store. Never hold the guard across an `.await`.
    pub fn lock(&self) -> MutexGuard<'_, Db> {
        // A poisoned lock means a prior holder panicked mid-write; the process
        // is no longer trustworthy, so propagating the panic is correct.
        self.db.lock().expect("board db mutex poisoned")
    }

    /// All runs that have started but not ended, paired with their card.
    pub fn active_runs(&self) -> Result<Vec<(Run, Card)>> {
        self.lock().active_runs_with_cards()
    }

    /// All queued runs (never started, not ended), FIFO by run id, with card.
    pub fn queued_runs(&self) -> Result<Vec<(Run, Card)>> {
        self.lock().queued_runs_with_cards()
    }
}
