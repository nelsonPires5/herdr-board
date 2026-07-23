use rusqlite::{params, OptionalExtension};

use super::rows;
use super::{Db, BOARD_ID};
use crate::model::{Card, Comment};
use crate::protocol::{
    AwaitingReason, CardCreateParams, CardStatus, CardUpdateParams, Effort, Patch, SpaceKind,
};
use crate::{Error, Result};

impl Db {
    // -- cards ---------------------------------------------------------------

    pub fn list_cards(&self, board_id: i64) -> Result<Vec<Card>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.* FROM cards c JOIN columns col ON col.id=c.column_id
             WHERE c.board_id=?1 ORDER BY col.position, c.position, c.id",
        )?;
        let rows = stmt
            .query_map(params![board_id], rows::row_to_card)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_all_cards(&self) -> Result<Vec<Card>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.* FROM cards c
             JOIN boards b ON b.id=c.board_id
             JOIN columns col ON col.id=c.column_id
             ORDER BY CASE WHEN b.scope_path IS NULL THEN 0 ELSE 1 END,
                      b.scope_path, col.position, c.position, c.id",
        )?;
        let rows = stmt
            .query_map([], rows::row_to_card)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_cards_in_column(&self, column_id: i64) -> Result<Vec<Card>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM cards WHERE column_id=?1 ORDER BY position, id")?;
        let rows = stmt
            .query_map(params![column_id], rows::row_to_card)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_card(&self, id: i64) -> Result<Option<Card>> {
        rows::opt(self.conn.query_row(
            "SELECT * FROM cards WHERE id=?1",
            params![id],
            rows::row_to_card,
        ))
    }

    pub(super) fn require_card(&self, id: i64) -> Result<Card> {
        self.get_card(id)?
            .ok_or_else(|| Error::NotFound(format!("card {id}")))
    }

    pub fn create_card(&self, p: &CardCreateParams) -> Result<Card> {
        let board_id = p.board_id.unwrap_or(BOARD_ID);
        self.get_board(board_id)?;
        let column_id = match p.column_id {
            Some(c) => c,
            None => self.default_column_id(board_id)?,
        };
        let column = self.require_column(column_id)?;
        if column.board_id != board_id {
            return Err(Error::InvalidState(format!(
                "column {column_id} belongs to board {}, expected {board_id}",
                column.board_id
            )));
        }
        let end: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position)+1, 0) FROM cards WHERE column_id=?1",
            params![column_id],
            |r| r.get(0),
        )?;
        let description = p.description.clone().unwrap_or_default();
        let harness = p
            .harness
            .clone()
            .unwrap_or_else(|| crate::harness::DEFAULT_HARNESS.to_string());
        let space_kind = p.space_kind.unwrap_or(SpaceKind::Workspace).as_str();
        let effort = p.effort.map(|e| e.as_str());
        self.conn.execute(
            "INSERT INTO cards
             (board_id,column_id,position,title,description,harness,model,effort,permission_mode,
              session,space_kind,space_ref,space_cwd,status,session_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'idle',NULL)",
            params![
                board_id,
                column_id,
                end,
                p.title,
                description,
                harness,
                p.model,
                effort,
                p.permission_mode,
                p.session,
                space_kind,
                p.space_ref,
                p.space_cwd,
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        if let Some(pos) = p.position {
            self.move_card(id, column_id, Some(pos))?;
        }
        self.require_card(id)
    }

    pub fn update_card(&self, p: &CardUpdateParams) -> Result<Card> {
        let mut c = self.require_card(p.id)?;
        if let Some(v) = &p.title {
            c.title = v.clone();
        }
        if let Some(v) = &p.description {
            c.description = v.clone();
        }
        if let Some(v) = &p.harness {
            c.harness = v.clone();
            if v == "pi" {
                c.permission_mode = None;
            } else if v == "claude" && matches!(c.effort, Some(Effort::Off | Effort::Minimal)) {
                c.effort = None;
            }
        }
        match &p.model {
            Patch::Unchanged => {}
            Patch::Clear => c.model = None,
            Patch::Set(v) => c.model = Some(v.clone()),
        }
        match p.effort {
            Patch::Unchanged => {}
            Patch::Clear => c.effort = None,
            Patch::Set(v) => c.effort = Some(v),
        }
        match &p.permission_mode {
            Patch::Unchanged => {}
            Patch::Clear => c.permission_mode = None,
            Patch::Set(v) => c.permission_mode = Some(v.clone()),
        }
        match &p.session {
            Patch::Unchanged => {}
            Patch::Clear => c.session = None,
            Patch::Set(v) => c.session = Some(v.clone()),
        }
        if let Some(v) = p.space_kind {
            c.space_kind = v;
        }
        match &p.space_ref {
            Patch::Unchanged => {}
            Patch::Clear => c.space_ref = None,
            Patch::Set(v) => c.space_ref = Some(v.clone()),
        }
        match &p.space_cwd {
            Patch::Unchanged => {}
            Patch::Clear => c.space_cwd = None,
            Patch::Set(v) => c.space_cwd = Some(v.clone()),
        }
        self.conn.execute(
            "UPDATE cards SET title=?1,description=?2,harness=?3,model=?4,effort=?5,
             permission_mode=?6,session=?7,space_kind=?8,space_ref=?9,space_cwd=?10,
             updated_at=datetime('now') WHERE id=?11",
            params![
                c.title,
                c.description,
                c.harness,
                c.model,
                c.effort.map(|e| e.as_str()),
                c.permission_mode,
                c.session,
                c.space_kind.as_str(),
                c.space_ref,
                c.space_cwd,
                c.id,
            ],
        )?;
        self.require_card(c.id)
    }

    pub fn set_card_archived(&self, id: i64, archived: bool) -> Result<Card> {
        self.require_card(id)?;
        if archived {
            self.conn.execute(
                "UPDATE cards SET archived_at=datetime('now'), updated_at=datetime('now') WHERE id=?1",
                params![id],
            )?;
        } else {
            self.conn.execute(
                "UPDATE cards SET archived_at=NULL, updated_at=datetime('now') WHERE id=?1",
                params![id],
            )?;
        }
        self.require_card(id)
    }

    pub fn delete_card(&self, id: i64) -> Result<()> {
        let card = self.require_card(id)?;
        self.conn
            .execute("DELETE FROM cards WHERE id=?1", params![id])?;
        self.renumber_column_cards(card.column_id)?;
        Ok(())
    }

    /// Move a card to `target_column` at `position` (append if `None`), compacting
    /// both the source and target columns.
    pub fn move_card(&self, id: i64, target_column: i64, position: Option<i64>) -> Result<Card> {
        let card = self.require_card(id)?;
        let target = self.require_column(target_column)?;
        if target.board_id != card.board_id {
            return Err(Error::InvalidState(format!(
                "column {target_column} belongs to board {}, card {id} belongs to board {}",
                target.board_id, card.board_id
            )));
        }
        let old_column = card.column_id;
        self.conn.execute(
            "UPDATE cards SET column_id=?1, updated_at=datetime('now') WHERE id=?2",
            params![target_column, id],
        )?;
        // Place within the target column.
        let mut ids: Vec<i64> = self
            .conn
            .prepare("SELECT id FROM cards WHERE column_id=?1 AND id<>?2 ORDER BY position, id")?
            .query_map(params![target_column, id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let idx = position
            .map(|p| p.max(0) as usize)
            .unwrap_or(ids.len())
            .min(ids.len());
        ids.insert(idx, id);
        for (i, cid) in ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE cards SET position=?1 WHERE id=?2",
                params![i as i64, cid],
            )?;
        }
        if old_column != target_column {
            self.renumber_column_cards(old_column)?;
        }
        self.require_card(id)
    }

    pub(super) fn renumber_column_cards(&self, column_id: i64) -> Result<()> {
        let ids: Vec<i64> = self
            .conn
            .prepare("SELECT id FROM cards WHERE column_id=?1 ORDER BY position, id")?
            .query_map(params![column_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for (i, cid) in ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE cards SET position=?1 WHERE id=?2",
                params![i as i64, cid],
            )?;
        }
        Ok(())
    }

    /// Set the card's status. Any status other than `awaiting` clears
    /// `awaiting_reason` (the reason is only meaningful while awaiting);
    /// use [`Db::set_card_awaiting`] to enter `awaiting` with a reason.
    pub fn set_card_status(&self, id: i64, status: CardStatus) -> Result<Card> {
        if status == CardStatus::Awaiting {
            return Err(Error::InvalidState(
                "enter awaiting with Db::set_card_awaiting so a reason is recorded".into(),
            ));
        }
        self.conn.execute(
            "UPDATE cards SET status=?1, awaiting_reason=NULL, updated_at=datetime('now')
             WHERE id=?2",
            params![status.as_str(), id],
        )?;
        self.require_card(id)
    }

    /// Enter (or re-enter, refreshing the reason) `awaiting` with `reason`.
    /// The active run stays open; the column timeout is paused upstream.
    pub fn set_card_awaiting(&self, id: i64, reason: AwaitingReason) -> Result<Card> {
        self.conn.execute(
            "UPDATE cards SET status='awaiting', awaiting_reason=?1, updated_at=datetime('now')
             WHERE id=?2",
            params![reason.as_str(), id],
        )?;
        self.require_card(id)
    }

    /// Atomically enter awaiting and pause the open run's durable timeout.
    /// Repeated calls preserve the original pause instant.
    pub fn pause_run_timeout_uow(
        &self,
        card_id: i64,
        reason: AwaitingReason,
        now_ms: i64,
    ) -> Result<Card> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE cards SET status='awaiting',awaiting_reason=?1,updated_at=datetime('now') WHERE id=?2",
            params![reason.as_str(), card_id],
        )?;
        tx.execute(
            "UPDATE runs SET timeout_paused_at_ms=COALESCE(timeout_paused_at_ms,?1)
             WHERE card_id=?2 AND ended_at IS NULL AND started_at IS NOT NULL",
            params![now_ms, card_id],
        )?;
        tx.commit()?;
        self.require_card(card_id)
    }

    /// Atomically leave awaiting and shift the deadline by the paused span.
    /// Clearing the pause marker makes retries idempotent.
    pub fn resume_run_timeout_uow(
        &self,
        card_id: i64,
        status: CardStatus,
        now_ms: i64,
    ) -> Result<Card> {
        if status == CardStatus::Awaiting {
            return Err(Error::InvalidState("cannot resume into awaiting".into()));
        }
        let tx = self.conn.unchecked_transaction()?;
        let timing: Option<(Option<i64>, Option<i64>)> = tx
            .query_row(
                "SELECT timeout_deadline_at_ms,timeout_paused_at_ms FROM runs
             WHERE card_id=?1 AND ended_at IS NULL AND started_at IS NOT NULL",
                params![card_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((deadline, Some(paused))) = timing {
            let shifted = deadline.map(|d| d.saturating_add(now_ms.saturating_sub(paused).max(0)));
            tx.execute(
                "UPDATE runs SET timeout_deadline_at_ms=?1,timeout_paused_at_ms=NULL
                 WHERE card_id=?2 AND ended_at IS NULL",
                params![shifted, card_id],
            )?;
        }
        tx.execute(
            "UPDATE cards SET status=?1,awaiting_reason=NULL,updated_at=datetime('now') WHERE id=?2",
            params![status.as_str(), card_id],
        )?;
        tx.commit()?;
        self.require_card(card_id)
    }

    pub fn set_card_column(&self, id: i64, column_id: i64) -> Result<Card> {
        self.move_card(id, column_id, None)
    }

    pub fn set_card_session(&self, id: i64, session_id: &str) -> Result<Card> {
        self.conn.execute(
            "UPDATE cards SET session_id=?1, updated_at=datetime('now') WHERE id=?2",
            params![session_id, id],
        )?;
        self.require_card(id)
    }

    // -- comments ------------------------------------------------------------

    pub fn add_comment(&self, card_id: i64, author: &str, body: &str) -> Result<Comment> {
        self.require_card(card_id)?;
        self.conn.execute(
            "INSERT INTO comments (card_id, author, body) VALUES (?1, ?2, ?3)",
            params![card_id, author, body],
        )?;
        let id = self.conn.last_insert_rowid();
        rows::opt(self.conn.query_row(
            "SELECT * FROM comments WHERE id=?1",
            params![id],
            rows::row_to_comment,
        ))?
        .ok_or_else(|| Error::NotFound(format!("comment {id}")))
    }

    pub fn list_comments(&self, card_id: i64) -> Result<Vec<Comment>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM comments WHERE card_id=?1 ORDER BY created_at, id")?;
        let rows = stmt
            .query_map(params![card_id], rows::row_to_comment)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
