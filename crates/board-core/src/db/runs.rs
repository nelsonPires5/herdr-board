use rusqlite::params;

use super::rows;
use super::{Db, EnqueueRun, FinalizeEffects, FinalizeRun, LifecycleFaultPoint};
use crate::model::{Card, Run};
use crate::protocol::{ActiveRunSummary, AwaitingReason};
use crate::{Error, Result};

impl Db {
    // -- runs ----------------------------------------------------------------

    /// Atomically insert a queued run and publish the card's queued state.
    /// No process, socket, notification, or other external I/O occurs here.
    pub fn enqueue_run_uow(&self, p: &EnqueueRun<'_>) -> Result<Run> {
        let card = self.require_card(p.card_id)?;
        let column = self.require_column(p.column_id)?;
        if card.board_id != column.board_id {
            return Err(Error::InvalidState(
                "run column belongs to another board".into(),
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO runs
             (card_id,column_id,harness,argv_json,prompt_snapshot,system_prompt_snapshot,launch_spec_json,session_id,session)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![p.card_id,p.column_id,p.harness,p.argv_json,p.prompt_snapshot,
                    p.system_prompt_snapshot,p.launch_spec_json,p.session_id,p.session],
        )?;
        let id = tx.last_insert_rowid();
        self.lifecycle_fault(LifecycleFaultPoint::EnqueueAfterRunInsert)?;
        let changed = tx.execute(
            "UPDATE cards SET status='queued',awaiting_reason=NULL,session_id=COALESCE(?2,session_id),updated_at=datetime('now') WHERE id=?1",
            params![p.card_id, p.session_id],
        )?;
        if changed != 1 {
            return Err(Error::NotFound(format!("card {}", p.card_id)));
        }
        tx.commit()?;
        self.get_run(id)
    }

    /// Atomically promote an exact queued run and its card to running.
    pub fn promote_run_uow(
        &self,
        run_id: i64,
        workspace_id: Option<&str>,
        pane_id: Option<&str>,
        timeout_deadline_at_ms: Option<i64>,
    ) -> Result<Run> {
        let tx = self.conn.unchecked_transaction()?;
        let card_id: i64 = tx
            .query_row(
                "SELECT card_id FROM runs WHERE id=?1 AND started_at IS NULL AND ended_at IS NULL",
                params![run_id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::InvalidState(format!("run {run_id} is not queued"))
                }
                other => Error::Sqlite(other),
            })?;
        tx.execute(
            "UPDATE runs SET started_at=datetime('now'),herdr_workspace_id=?1,herdr_pane_id=?2,timeout_deadline_at_ms=?4,timeout_paused_at_ms=NULL WHERE id=?3",
            params![workspace_id,pane_id,run_id,timeout_deadline_at_ms],
        )?;
        self.lifecycle_fault(LifecycleFaultPoint::PromoteAfterRunUpdate)?;
        tx.execute(
            "UPDATE cards SET status='running',awaiting_reason=NULL,updated_at=datetime('now') WHERE id=?1",
            params![card_id],
        )?;
        tx.commit()?;
        self.get_run(run_id)
    }

    /// Atomically close a run, append its optional durable comment, transition
    /// the card, and optionally enqueue the already-planned next auto-hop.
    pub fn finalize_run_uow(&self, p: &FinalizeRun<'_>) -> Result<FinalizeEffects> {
        let tx = self.conn.unchecked_transaction()?;
        let card_id: i64 = tx
            .query_row(
                "SELECT card_id FROM runs WHERE id=?1 AND ended_at IS NULL",
                params![p.run_id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::InvalidState(format!("run {} is not open", p.run_id))
                }
                other => Error::Sqlite(other),
            })?;
        if let Some(next) = &p.next {
            if next.card_id != card_id {
                return Err(Error::InvalidState(
                    "next run belongs to another card".into(),
                ));
            }
            let board_matches: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM cards c JOIN columns col ON col.id=?1
                               WHERE c.id=?2 AND c.board_id=col.board_id)",
                params![next.column_id, card_id],
                |row| row.get(0),
            )?;
            if !board_matches {
                return Err(Error::InvalidState(
                    "next run column belongs to another board".into(),
                ));
            }
        }
        tx.execute(
            "UPDATE runs SET ended_at=datetime('now'),outcome=?1,result_summary=?2 WHERE id=?3",
            params![p.outcome.as_str(), p.summary, p.run_id],
        )?;
        self.lifecycle_fault(LifecycleFaultPoint::FinalizeAfterRunUpdate)?;
        for (author, body) in p.comments {
            tx.execute(
                "INSERT INTO comments(card_id,author,body) VALUES(?1,?2,?3)",
                params![card_id, author, body],
            )?;
        }
        if let Some(column_id) = p.target_column_id {
            let board_matches: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM cards c JOIN columns col ON col.id=?1
                               WHERE c.id=?2 AND c.board_id=col.board_id)",
                params![column_id, card_id],
                |r| r.get(0),
            )?;
            if !board_matches {
                return Err(Error::InvalidState(
                    "target column belongs to another board".into(),
                ));
            }
            let source_column_id: i64 = tx.query_row(
                "SELECT column_id FROM cards WHERE id=?1",
                params![card_id],
                |row| row.get(0),
            )?;
            tx.execute(
                "UPDATE cards SET column_id=?1 WHERE id=?2",
                params![column_id, card_id],
            )?;
            let mut target_ids: Vec<i64> = {
                let mut statement = tx.prepare(
                    "SELECT id FROM cards WHERE column_id=?1 AND id<>?2 ORDER BY position,id",
                )?;
                let ids = statement
                    .query_map(params![column_id, card_id], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                ids
            };
            target_ids.push(card_id);
            for (position, target_card_id) in target_ids.iter().enumerate() {
                tx.execute(
                    "UPDATE cards SET position=?1 WHERE id=?2",
                    params![position as i64, target_card_id],
                )?;
            }
            if source_column_id != column_id {
                let source_ids: Vec<i64> = {
                    let mut statement =
                        tx.prepare("SELECT id FROM cards WHERE column_id=?1 ORDER BY position,id")?;
                    let ids = statement
                        .query_map(params![source_column_id], |row| row.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
                    ids
                };
                for (position, source_card_id) in source_ids.iter().enumerate() {
                    tx.execute(
                        "UPDATE cards SET position=?1 WHERE id=?2",
                        params![position as i64, source_card_id],
                    )?;
                }
            }
        }
        tx.execute(
            "UPDATE cards SET status=?1,awaiting_reason=?2,updated_at=datetime('now') WHERE id=?3",
            params![
                p.final_status.as_str(),
                p.final_awaiting_reason.as_ref().map(AwaitingReason::as_str),
                card_id
            ],
        )?;
        let next_id = if let Some(next) = &p.next {
            tx.execute(
                "INSERT INTO runs(card_id,column_id,harness,argv_json,prompt_snapshot,
                 system_prompt_snapshot,launch_spec_json,session_id,session) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    next.card_id,
                    next.column_id,
                    next.harness,
                    next.argv_json,
                    next.prompt_snapshot,
                    next.system_prompt_snapshot,
                    next.launch_spec_json,
                    next.session_id,
                    next.session
                ],
            )?;
            tx.execute(
                "UPDATE cards SET status='queued',awaiting_reason=NULL WHERE id=?1",
                params![card_id],
            )?;
            Some(tx.last_insert_rowid())
        } else {
            None
        };
        tx.commit()?;
        Ok(FinalizeEffects {
            card: self.require_card(card_id)?,
            finished_run: self.get_run(p.run_id)?,
            next_run: next_id.map(|id| self.get_run(id)).transpose()?,
        })
    }

    pub fn get_run(&self, id: i64) -> Result<Run> {
        rows::opt(self.conn.query_row(
            "SELECT * FROM runs WHERE id=?1",
            params![id],
            rows::row_to_run,
        ))?
        .ok_or_else(|| Error::NotFound(format!("run {id}")))
    }

    pub fn list_runs(&self, card_id: i64) -> Result<Vec<Run>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM runs WHERE card_id=?1 ORDER BY id")?;
        let rows = stmt
            .query_map(params![card_id], rows::row_to_run)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The card's open (queued or started, not ended) run, if any.
    pub fn open_run_for_card(&self, card_id: i64) -> Result<Option<Run>> {
        rows::opt(self.conn.query_row(
            "SELECT * FROM runs WHERE card_id=?1 AND ended_at IS NULL
             ORDER BY id DESC LIMIT 1",
            params![card_id],
            rows::row_to_run,
        ))
    }

    /// Whether any card currently in `column_id` has an open run.
    pub fn column_has_open_run(&self, column_id: i64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM runs r JOIN cards c ON c.id=r.card_id
               WHERE c.column_id=?1 AND r.ended_at IS NULL
             )",
            params![column_id],
            |row| row.get(0),
        )?)
    }

    /// The card's active (started, not ended) run, if any.
    pub fn active_run_for_card(&self, card_id: i64) -> Result<Option<Run>> {
        rows::opt(self.conn.query_row(
            "SELECT * FROM runs WHERE card_id=?1 AND started_at IS NOT NULL AND ended_at IS NULL
             ORDER BY id DESC LIMIT 1",
            params![card_id],
            rows::row_to_run,
        ))
    }

    /// Most recent run for the card that still records a target pane.
    pub fn latest_run_with_pane(&self, card_id: i64) -> Result<Option<Run>> {
        self.require_card(card_id)?;
        rows::opt(self.conn.query_row(
            "SELECT * FROM runs WHERE card_id=?1 AND herdr_pane_id IS NOT NULL
             ORDER BY id DESC LIMIT 1",
            params![card_id],
            rows::row_to_run,
        ))
    }

    /// Started and open runs paired with their cards, using only the partial
    /// active index rather than scanning card run histories.
    pub fn active_runs_with_cards(&self) -> Result<Vec<(Run, Card)>> {
        self.open_runs_with_cards(
            "SELECT id, card_id FROM runs INDEXED BY idx_runs_active_open
             WHERE started_at IS NOT NULL AND ended_at IS NULL ORDER BY id",
        )
    }

    /// Started and open runs for cards on one board, reduced to the fields
    /// needed by board snapshot consumers. This query is board-scoped at the
    /// SQL boundary; it never exposes queued, ended, or other-board runs.
    pub fn active_run_summaries(&self, board_id: i64) -> Result<Vec<ActiveRunSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.card_id, r.started_at
             FROM runs r
             JOIN cards c ON c.id=r.card_id
             WHERE c.board_id=?1
               AND r.started_at IS NOT NULL
               AND r.ended_at IS NULL
             ORDER BY r.id",
        )?;
        let rows = stmt.query_map(params![board_id], |row| {
            Ok(ActiveRunSummary {
                card_id: row.get("card_id")?,
                started_at: row.get("started_at")?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Alias using the list-oriented naming used by board snapshot callers.
    pub fn list_active_run_summaries(&self, board_id: i64) -> Result<Vec<ActiveRunSummary>> {
        self.active_run_summaries(board_id)
    }

    /// Never-started open runs paired with cards in global FIFO order.
    pub fn queued_runs_with_cards(&self) -> Result<Vec<(Run, Card)>> {
        self.open_runs_with_cards(
            "SELECT id, card_id FROM runs INDEXED BY idx_runs_queued_fifo
             WHERE started_at IS NULL AND ended_at IS NULL ORDER BY id",
        )
    }

    fn open_runs_with_cards(&self, sql: &str) -> Result<Vec<(Run, Card)>> {
        let ids = {
            let mut stmt = self.conn.prepare(sql)?;
            let rows =
                stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        ids.into_iter()
            .map(|(run_id, card_id)| {
                let run = self.get_run(run_id)?;
                let card = self
                    .get_card(card_id)?
                    .ok_or_else(|| Error::NotFound(format!("card {card_id}")))?;
                Ok((run, card))
            })
            .collect()
    }

    pub fn count_active_runs(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM runs WHERE started_at IS NOT NULL AND ended_at IS NULL",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn count_queued_runs(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM runs WHERE started_at IS NULL AND ended_at IS NULL",
            [],
            |r| r.get(0),
        )?)
    }
}
