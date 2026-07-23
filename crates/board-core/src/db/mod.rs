//! rusqlite store: migrations, CRUD, position management, and the queries the
//! daemon needs. `boardd` is the only writer; access is serialized upstream, so
//! this type is deliberately synchronous.

mod boards_columns;
mod cards_comments;
mod migrations;
mod rows;
mod runs;

use std::path::Path;

use rusqlite::Connection;

use crate::model::{Card, Run};
use crate::protocol::{AwaitingReason, CardStatus, RunOutcome};
use crate::Result;

/// Deterministic statement boundaries used by crash-atomicity tests. Hooks are
/// opt-in per [`Db`] instance and receive no row or effect DTO.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleFaultPoint {
    EnqueueAfterRunInsert,
    PromoteAfterRunUpdate,
    FinalizeAfterRunUpdate,
}

/// All values needed to insert one queued run. Prompt building and external I/O
/// happen before this value enters the transaction.
#[derive(Debug, Clone)]
pub struct EnqueueRun<'a> {
    pub card_id: i64,
    pub column_id: i64,
    pub harness: &'a str,
    pub argv_json: &'a str,
    pub prompt_snapshot: &'a str,
    pub system_prompt_snapshot: Option<&'a str>,
    pub launch_spec_json: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub session: Option<&'a str>,
}

/// Optional next run inserted atomically with finalization (an auto hop).
pub struct FinalizeRun<'a> {
    pub run_id: i64,
    pub outcome: RunOutcome,
    pub summary: Option<&'a str>,
    pub comments: &'a [(&'a str, &'a str)],
    pub target_column_id: Option<i64>,
    pub final_status: CardStatus,
    pub final_awaiting_reason: Option<AwaitingReason>,
    pub next: Option<EnqueueRun<'a>>,
}

/// Durable values callers may use for effects only after commit.
#[derive(Debug, Clone)]
pub struct FinalizeEffects {
    pub card: Card,
    pub finished_run: Run,
    pub next_run: Option<Run>,
}

/// The preserved Global board id and legacy protocol default.
pub const BOARD_ID: i64 = 1;

/// SQLite-backed board store.
pub struct Db {
    pub(super) conn: Connection,
    pub(super) lifecycle_fault_hook:
        Option<Box<dyn Fn(LifecycleFaultPoint) -> Result<()> + Send + Sync>>,
}

pub(super) fn conv_err(field: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid enum value in {field}"),
        )),
    )
}

impl Db {
    /// Open (or create) a file-backed db: makes parent dirs, enables WAL +
    /// foreign keys, and runs migrations.
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Db::init(conn, None)
    }

    /// Open a file DB with an opt-in lifecycle statement hook. This is exposed
    /// only for deterministic fault tests; production callers use [`Db::open`].
    #[doc(hidden)]
    pub fn open_with_lifecycle_fault_hook(
        path: &Path,
        hook: impl Fn(LifecycleFaultPoint) -> Result<()> + Send + Sync + 'static,
    ) -> Result<Db> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Db::init(conn, Some(Box::new(hook)))
    }

    /// Open an in-memory db (tests, `FakeBoardClient`).
    pub fn open_in_memory() -> Result<Db> {
        Db::init(Connection::open_in_memory()?, None)
    }

    fn init(
        conn: Connection,
        lifecycle_fault_hook: Option<Box<dyn Fn(LifecycleFaultPoint) -> Result<()> + Send + Sync>>,
    ) -> Result<Db> {
        conn.pragma_update(None, "foreign_keys", true)?;
        let db = Db {
            conn,
            lifecycle_fault_hook,
        };
        db.migrate()?;
        Ok(db)
    }

    pub(super) fn lifecycle_fault(&self, point: LifecycleFaultPoint) -> Result<()> {
        if let Some(hook) = &self.lifecycle_fault_hook {
            hook(point)?;
        }
        Ok(())
    }

    /// The current schema version.
    pub fn user_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }
}
