use rusqlite::{params, OptionalExtension};

use super::{constraints, rows};
use super::{ColumnTarget, ColumnWiring, Db, BOARD_ID};
use crate::model::{Board, Column};
use crate::protocol::{ColumnCreateParams, ColumnUpdateParams, Patch, Trigger};
use crate::{Error, Result};

impl Db {
    // -- board ---------------------------------------------------------------

    pub fn get_board(&self, id: i64) -> Result<Board> {
        self.conn
            .query_row(
                &format!("{} WHERE b.id=?1", rows::BOARD_SELECT),
                params![id],
                rows::row_to_board,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("board {id}")),
                other => Error::Sqlite(other),
            })
    }

    /// Rename a board without changing its id, project, columns, or cards.
    /// The schema's per-project UNIQUE (project_id, name COLLATE NOCASE) index
    /// remains the final guard against duplicate names;
    /// [`constraints::reject_duplicate`] reports that refusal as a bad request
    /// rather than as an internal storage failure.
    pub fn rename_board(&self, id: i64, name: &str) -> Result<Board> {
        if name.trim().is_empty() {
            return Err(Error::BadRequest("board name must not be empty".into()));
        }
        self.get_board(id)?;
        constraints::reject_duplicate(
            self.conn
                .execute("UPDATE boards SET name=?1 WHERE id=?2", params![name, id]),
            || constraints::duplicate_board(name),
        )?;
        self.get_board(id)
    }

    /// Every board across every project (legacy flat listing); Global project
    /// first, then projects by path and boards by name.
    pub fn list_boards(&self) -> Result<Vec<Board>> {
        let mut stmt = self.conn.prepare(&format!(
            "{} ORDER BY CASE WHEN p.scope_path IS NULL THEN 0 ELSE 1 END,
             p.scope_path, b.name COLLATE NOCASE, b.id",
            rows::BOARD_SELECT
        ))?;
        let rows = stmt
            .query_map([], rows::row_to_board)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// One project's boards, ordered by name (case-insensitive).
    pub fn list_boards_for_project(&self, project_id: i64) -> Result<Vec<Board>> {
        let mut stmt = self.conn.prepare(&format!(
            "{} WHERE b.project_id=?1 ORDER BY b.name COLLATE NOCASE, b.id",
            rows::BOARD_SELECT
        ))?;
        let rows = stmt
            .query_map(params![project_id], rows::row_to_board)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Resolve an id-or-name board reference within one project. A numeric
    /// reference is an id and must belong to the project; anything else
    /// matches the board name case-insensitively.
    pub fn resolve_board(&self, project_id: i64, reference: &str) -> Result<Board> {
        if let Ok(id) = reference.parse::<i64>() {
            let board = self.get_board(id)?;
            if board.project_id == project_id {
                return Ok(board);
            }
        }
        let lower = reference.to_lowercase();
        let mut stmt = self.conn.prepare(&format!(
            "{} WHERE b.project_id=?1 AND lower(b.name)=?2 LIMIT 1",
            rows::BOARD_SELECT
        ))?;
        stmt.query_row(params![project_id, lower], rows::row_to_board)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::NotFound(format!("no board {reference:?} in this project"))
                }
                other => Error::Sqlite(other),
            })
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

    /// [`Db::get_column`] with the missing-row case already mapped onto
    /// [`Error::NotFound`], so callers stop open-coding that lookup.
    pub fn require_column(&self, id: i64) -> Result<Column> {
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
        // `UNIQUE (board_id, name)` is the guard against a duplicate column
        // name, and it is reachable from an ordinary `column.create`.
        constraints::reject_duplicate(
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
            ),
            || constraints::duplicate_column(&p.name),
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
        // Renaming a column onto a sibling's name hits the same per-board
        // UNIQUE index as creating one, and is just as much a bad request.
        constraints::reject_duplicate(
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
            ),
            || constraints::duplicate_column(&c.name),
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

    /// Create a set of columns and wire their transitions as one database
    /// unit of work. The targets may refer to columns already on the board or
    /// to another column in this batch. This is deliberately a DB-only
    /// boundary: callers must do validation and all external I/O before it.
    pub fn apply_template_columns_uow(
        &self,
        board_id: i64,
        specs: &[ColumnCreateParams],
        wiring: &[ColumnWiring],
    ) -> Result<Vec<Column>> {
        self.get_board(board_id)?;
        for spec in specs {
            if spec.board_id.unwrap_or(BOARD_ID) != board_id {
                return Err(Error::InvalidState(format!(
                    "column belongs to another board, expected {board_id}"
                )));
            }
            if spec.position.is_some() {
                return Err(Error::BadRequest(
                    "template columns cannot specify positions".into(),
                ));
            }
        }

        let tx = self.conn.unchecked_transaction()?;
        let mut ids = Vec::with_capacity(specs.len());
        for spec in specs {
            let position: i64 = tx.query_row(
                "SELECT COALESCE(MAX(position)+1, 0) FROM columns WHERE board_id=?1",
                params![board_id],
                |row| row.get(0),
            )?;
            let trigger = spec.trigger.unwrap_or(Trigger::Manual).as_str();
            let fresh = i64::from(spec.fresh_session.unwrap_or(false));
            // Callers apply templates only to an otherwise-empty board, so a
            // clash here should be unreachable; classify it the same way
            // anyway, since it is the same index refusing the same user-chosen
            // name, and the whole batch rolls back with the transaction.
            constraints::reject_duplicate(
                tx.execute(
                    "INSERT INTO columns
                 (board_id,name,position,system_prompt,trigger,on_success_column_id,on_fail_column_id,
                  fresh_session,harness_override,model_override,effort_override,permission_override,timeout_minutes)
                 VALUES (?1,?2,?3,?4,?5,NULL,NULL,?6,?7,?8,?9,?10,?11)",
                    params![
                        board_id,
                        spec.name,
                        position,
                        spec.system_prompt,
                        trigger,
                        fresh,
                        spec.harness_override,
                        spec.model_override,
                        spec.effort_override,
                        spec.permission_override,
                        spec.timeout_minutes,
                    ],
                ),
                || constraints::duplicate_column(&spec.name),
            )?;
            ids.push(tx.last_insert_rowid());
        }

        let resolve = |target: Option<ColumnTarget>| -> Result<Option<i64>> {
            let Some(target) = target else {
                return Ok(None);
            };
            let id = match target {
                ColumnTarget::Created(index) => ids.get(index).copied().ok_or_else(|| {
                    Error::BadRequest(format!("template target index {index} is out of range"))
                })?,
                ColumnTarget::Existing(id) => {
                    let target_board: Option<i64> = tx
                        .query_row(
                            "SELECT board_id FROM columns WHERE id=?1",
                            params![id],
                            |row| row.get(0),
                        )
                        .optional()?;
                    let target_board =
                        target_board.ok_or_else(|| Error::NotFound(format!("column {id}")))?;
                    if target_board != board_id {
                        return Err(Error::InvalidState(format!(
                            "column {id} belongs to board {target_board}, expected {board_id}"
                        )));
                    }
                    id
                }
            };
            Ok(Some(id))
        };

        for wire in wiring {
            let column_id = ids.get(wire.column_index).copied().ok_or_else(|| {
                Error::BadRequest(format!(
                    "template source index {} is out of range",
                    wire.column_index
                ))
            })?;
            let on_success = resolve(wire.on_success)?;
            let on_fail = resolve(wire.on_fail)?;
            tx.execute(
                "UPDATE columns SET on_success_column_id=?1,on_fail_column_id=?2 WHERE id=?3",
                params![on_success, on_fail, column_id],
            )?;
        }
        tx.commit()?;
        self.list_columns(board_id)
    }

    /// Move a column to `position` and compact the whole board's ordering.
    pub fn reorder_column(&self, id: i64, position: i64) -> Result<Vec<Column>> {
        let board_id = self.require_column(id)?.board_id;
        let tx = self.conn.unchecked_transaction()?;
        let mut ids: Vec<i64> = tx
            .prepare("SELECT id FROM columns WHERE board_id=?1 AND id<>?2 ORDER BY position, id")?
            .query_map(params![board_id, id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let idx = (position.max(0) as usize).min(ids.len());
        ids.insert(idx, id);
        for (i, cid) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE columns SET position=?1 WHERE id=?2",
                params![i as i64, cid],
            )?;
        }
        tx.commit()?;
        self.list_columns(board_id)
    }

    /// Delete a column, optionally moving its cards to `move_cards_to` first.
    /// Callers should validate with the engine beforehand.
    pub fn delete_column(&self, id: i64, move_cards_to: Option<i64>) -> Result<()> {
        let board_id = self.require_column(id)?.board_id;
        if let Some(dst) = move_cards_to {
            let destination = self.require_column(dst)?;
            if destination.board_id != board_id {
                return Err(Error::InvalidState(format!(
                    "destination column {dst} belongs to another board"
                )));
            }
        }
        let tx = self.conn.unchecked_transaction()?;
        if let Some(dst) = move_cards_to {
            let card_ids: Vec<i64> = tx
                .prepare("SELECT id FROM cards WHERE column_id=?1 ORDER BY position, id")?
                .query_map(params![id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for cid in card_ids {
                tx.execute(
                    "UPDATE cards SET column_id=?1, updated_at=datetime('now') WHERE id=?2",
                    params![dst, cid],
                )?;
                Db::place_card_in_column_tx(&tx, cid, dst, None)?;
            }
        }
        tx.execute("DELETE FROM columns WHERE id=?1", params![id])?;
        // Compact remaining columns.
        let ids: Vec<i64> = tx
            .prepare("SELECT id FROM columns WHERE board_id=?1 ORDER BY position, id")?
            .query_map(params![board_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for (i, cid) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE columns SET position=?1 WHERE id=?2",
                params![i as i64, cid],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
