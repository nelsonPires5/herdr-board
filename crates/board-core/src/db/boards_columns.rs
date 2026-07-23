use rusqlite::params;

use super::rows;
use super::{Db, BOARD_ID};
use crate::model::{Board, Column};
use crate::protocol::{ColumnCreateParams, ColumnUpdateParams, Patch, Trigger};
use crate::{Error, Result};

impl Db {
    // -- board ---------------------------------------------------------------

    pub fn get_board(&self, id: i64) -> Result<Board> {
        self.conn
            .query_row(
                "SELECT id, name, scope_path FROM boards WHERE id=?1",
                params![id],
                rows::row_to_board,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("board {id}")),
                other => Error::Sqlite(other),
            })
    }

    pub fn list_boards(&self) -> Result<Vec<Board>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, scope_path FROM boards
             ORDER BY CASE WHEN scope_path IS NULL THEN 0 ELSE 1 END, scope_path, id",
        )?;
        let rows = stmt
            .query_map([], rows::row_to_board)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Get or create the independent board for an already-canonical scope path.
    /// New boards contain exactly one manual `Todo` column.
    pub fn open_board(&self, scope_path: &str) -> Result<Board> {
        if scope_path.trim().is_empty() {
            return Err(Error::BadRequest("scope_path must not be empty".into()));
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO boards(name,scope_path) VALUES(?1,?1)",
            params![scope_path],
        )?;
        let board = tx.query_row(
            "SELECT id,name,scope_path FROM boards WHERE scope_path=?1",
            params![scope_path],
            rows::row_to_board,
        )?;
        tx.execute(
            "INSERT INTO columns(board_id,name,position,trigger,fresh_session)
             SELECT ?1,'Todo',0,'manual',0
             WHERE NOT EXISTS(SELECT 1 FROM columns WHERE board_id=?1)",
            params![board.id],
        )?;
        tx.commit()?;
        Ok(board)
    }

    // -- columns -------------------------------------------------------------

    pub fn list_columns(&self, board_id: i64) -> Result<Vec<Column>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM columns WHERE board_id=?1 ORDER BY position, id")?;
        let rows = stmt
            .query_map(params![board_id], rows::row_to_column)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_column(&self, id: i64) -> Result<Option<Column>> {
        rows::opt(self.conn.query_row(
            "SELECT * FROM columns WHERE id=?1",
            params![id],
            rows::row_to_column,
        ))
    }

    pub(super) fn require_column(&self, id: i64) -> Result<Column> {
        self.get_column(id)?
            .ok_or_else(|| Error::NotFound(format!("column {id}")))
    }

    /// The default (first) column of a board — the seed `Todo`.
    pub fn default_column_id(&self, board_id: i64) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT id FROM columns WHERE board_id=?1 ORDER BY position, id LIMIT 1",
                params![board_id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Error::NotFound("no columns".into()),
                other => Error::Sqlite(other),
            })
    }

    pub fn create_column(&self, p: &ColumnCreateParams) -> Result<Column> {
        let board_id = p.board_id.unwrap_or(BOARD_ID);
        self.get_board(board_id)?;
        self.validate_column_targets(board_id, p.on_success_column_id, p.on_fail_column_id)?;
        let end: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position)+1, 0) FROM columns WHERE board_id=?1",
            params![board_id],
            |r| r.get(0),
        )?;
        let trigger = p.trigger.unwrap_or(Trigger::Manual).as_str();
        let fresh = i64::from(p.fresh_session.unwrap_or(false));
        self.conn.execute(
            "INSERT INTO columns
             (board_id,name,position,system_prompt,trigger,on_success_column_id,on_fail_column_id,
              fresh_session,harness_override,model_override,effort_override,permission_override,timeout_minutes)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                board_id,
                p.name,
                end,
                p.system_prompt,
                trigger,
                p.on_success_column_id,
                p.on_fail_column_id,
                fresh,
                p.harness_override,
                p.model_override,
                p.effort_override,
                p.permission_override,
                p.timeout_minutes,
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        if let Some(pos) = p.position {
            self.reorder_column(id, pos)?;
        }
        self.require_column(id)
    }

    pub fn update_column(&self, p: &ColumnUpdateParams) -> Result<Column> {
        let mut c = self.require_column(p.id)?;
        if let Some(v) = &p.name {
            c.name = v.clone();
        }
        match &p.system_prompt {
            Patch::Unchanged => {}
            Patch::Clear => c.system_prompt = None,
            Patch::Set(v) => c.system_prompt = Some(v.clone()),
        }
        if let Some(v) = p.trigger {
            c.trigger = v;
        }
        match p.on_success_column_id {
            Patch::Unchanged => {}
            Patch::Clear => c.on_success_column_id = None,
            Patch::Set(v) => c.on_success_column_id = Some(v),
        }
        match p.on_fail_column_id {
            Patch::Unchanged => {}
            Patch::Clear => c.on_fail_column_id = None,
            Patch::Set(v) => c.on_fail_column_id = Some(v),
        }
        if let Some(v) = p.fresh_session {
            c.fresh_session = v;
        }
        match &p.harness_override {
            Patch::Unchanged => {}
            Patch::Clear => c.harness_override = None,
            Patch::Set(v) => c.harness_override = Some(v.clone()),
        }
        match &p.model_override {
            Patch::Unchanged => {}
            Patch::Clear => c.model_override = None,
            Patch::Set(v) => c.model_override = Some(v.clone()),
        }
        match &p.effort_override {
            Patch::Unchanged => {}
            Patch::Clear => c.effort_override = None,
            Patch::Set(v) => c.effort_override = Some(v.clone()),
        }
        match &p.permission_override {
            Patch::Unchanged => {}
            Patch::Clear => c.permission_override = None,
            Patch::Set(v) => c.permission_override = Some(v.clone()),
        }
        match p.timeout_minutes {
            Patch::Unchanged => {}
            Patch::Clear => c.timeout_minutes = None,
            Patch::Set(v) => c.timeout_minutes = Some(v),
        }
        self.validate_column_targets(c.board_id, c.on_success_column_id, c.on_fail_column_id)?;
        self.conn.execute(
            "UPDATE columns SET name=?1,system_prompt=?2,trigger=?3,on_success_column_id=?4,
             on_fail_column_id=?5,fresh_session=?6,harness_override=?7,model_override=?8,
             effort_override=?9,permission_override=?10,timeout_minutes=?11 WHERE id=?12",
            params![
                c.name,
                c.system_prompt,
                c.trigger.as_str(),
                c.on_success_column_id,
                c.on_fail_column_id,
                i64::from(c.fresh_session),
                c.harness_override,
                c.model_override,
                c.effort_override,
                c.permission_override,
                c.timeout_minutes,
                c.id,
            ],
        )?;
        if let Some(pos) = p.position {
            self.reorder_column(c.id, pos)?;
        }
        self.require_column(c.id)
    }

    pub(super) fn validate_column_targets(
        &self,
        board_id: i64,
        on_success: Option<i64>,
        on_fail: Option<i64>,
    ) -> Result<()> {
        for target in [on_success, on_fail].into_iter().flatten() {
            let column = self.require_column(target)?;
            if column.board_id != board_id {
                return Err(Error::InvalidState(format!(
                    "column {target} belongs to board {}, expected {board_id}",
                    column.board_id
                )));
            }
        }
        Ok(())
    }

    /// Move a column to `position` and compact the whole board's ordering.
    pub fn reorder_column(&self, id: i64, position: i64) -> Result<Vec<Column>> {
        let board_id = self.require_column(id)?.board_id;
        let mut ids: Vec<i64> = self
            .conn
            .prepare("SELECT id FROM columns WHERE board_id=?1 AND id<>?2 ORDER BY position, id")?
            .query_map(params![board_id, id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let idx = (position.max(0) as usize).min(ids.len());
        ids.insert(idx, id);
        for (i, cid) in ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE columns SET position=?1 WHERE id=?2",
                params![i as i64, cid],
            )?;
        }
        self.list_columns(board_id)
    }

    /// Delete a column, optionally moving its cards to `move_cards_to` first.
    /// Callers should validate with the engine beforehand.
    pub fn delete_column(&self, id: i64, move_cards_to: Option<i64>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let board_id = self.require_column(id)?.board_id;
        if let Some(dst) = move_cards_to {
            let destination = self.require_column(dst)?;
            if destination.board_id != board_id {
                return Err(Error::InvalidState(format!(
                    "destination column {dst} belongs to another board"
                )));
            }
            let card_ids: Vec<i64> = self
                .conn
                .prepare("SELECT id FROM cards WHERE column_id=?1 ORDER BY position, id")?
                .query_map(params![id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for cid in card_ids {
                self.move_card(cid, dst, None)?;
            }
        }
        self.conn
            .execute("DELETE FROM columns WHERE id=?1", params![id])?;
        // Compact remaining columns.
        let ids: Vec<i64> = self
            .conn
            .prepare("SELECT id FROM columns WHERE board_id=?1 ORDER BY position, id")?
            .query_map(params![board_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for (i, cid) in ids.iter().enumerate() {
            self.conn.execute(
                "UPDATE columns SET position=?1 WHERE id=?2",
                params![i as i64, cid],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
