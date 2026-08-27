use rusqlite::{params, OptionalExtension};

use super::rows;
use super::{Db, EnqueueRun, BOARD_ID};
use crate::model::{Card, Comment, CommentHistory, CommentRecord};
use crate::protocol::{
    AwaitingReason, CardCreateParams, CardDetail, CardStatus, CardUpdateParams, CardVisibility,
    Effort, Patch, SpaceKind,
};
use crate::{Error, Result};

impl Db {
    // -- cards ---------------------------------------------------------------

    /// List every card on a board for board snapshots, including archived
    /// cards. Callers that expose an archive filter should use
    /// [`Self::list_cards_visible`] explicitly.
    pub fn list_cards(&self, board_id: i64) -> Result<Vec<Card>> {
        self.list_cards_visible(board_id, CardVisibility::All)
    }

    /// List cards in one board according to an explicit archive visibility.
    pub fn list_cards_visible(
        &self,
        board_id: i64,
        visibility: CardVisibility,
    ) -> Result<Vec<Card>> {
        let archived_clause = match visibility {
            CardVisibility::Active => "AND c.archived_at IS NULL",
            CardVisibility::All => "",
            CardVisibility::Archived => "AND c.archived_at IS NOT NULL",
        };
        let sql = format!(
            "SELECT c.* FROM cards c JOIN columns col ON col.id=c.column_id
             WHERE c.board_id=?1 {archived_clause}
             ORDER BY col.position, c.position, c.id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![board_id], rows::row_to_card)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Column-scoped equivalent used by card-list filters. Internal lifecycle
    /// callers can continue using [`Self::list_cards_in_column`] for all rows.
    pub fn list_cards_in_column_visible(
        &self,
        column_id: i64,
        visibility: CardVisibility,
    ) -> Result<Vec<Card>> {
        let archived_clause = match visibility {
            CardVisibility::Active => "AND archived_at IS NULL",
            CardVisibility::All => "",
            CardVisibility::Archived => "AND archived_at IS NOT NULL",
        };
        let sql = format!(
            "SELECT * FROM cards WHERE column_id=?1 {archived_clause} ORDER BY position, id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![column_id], rows::row_to_card)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_all_cards(&self) -> Result<Vec<Card>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.* FROM cards c
             JOIN boards b ON b.id=c.board_id
             JOIN projects p ON p.id=b.project_id
             JOIN columns col ON col.id=c.column_id
             ORDER BY CASE WHEN p.scope_path IS NULL THEN 0 ELSE 1 END,
                      p.scope_path, col.position, c.position, c.id",
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

    /// [`Db::get_card`] with the missing-row case already mapped onto
    /// [`Error::NotFound`], so callers stop open-coding that lookup.
    pub fn require_card(&self, id: i64) -> Result<Card> {
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

    /// Create a card and its initial queued run in one transaction. The run's
    /// `card_id` is replaced with the newly allocated card id; all other run
    /// fields are supplied by the caller after any pure preparation has
    /// completed. No external I/O belongs in this unit of work.
    pub fn create_card_and_enqueue_uow(
        &self,
        p: &CardCreateParams,
        run: &EnqueueRun<'_>,
    ) -> Result<(Card, crate::model::Run)> {
        let board_id = p.board_id.unwrap_or(BOARD_ID);
        self.get_board(board_id)?;
        let column_id = p.column_id.unwrap_or(self.default_column_id(board_id)?);
        let column = self.require_column(column_id)?;
        if column.board_id != board_id {
            return Err(Error::InvalidState(format!(
                "column {column_id} belongs to board {}, expected {board_id}",
                column.board_id
            )));
        }
        if run.column_id != column_id {
            return Err(Error::InvalidState(
                "queued run column does not match new card column".into(),
            ));
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
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
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
        let card_id = tx.last_insert_rowid();
        if p.position.is_some() {
            Self::place_card_in_column_tx(&tx, card_id, column_id, p.position)?;
        }
        let run_id = self.enqueue_run_tx(&tx, card_id, run)?;
        tx.commit()?;
        Ok((self.require_card(card_id)?, self.get_run(run_id)?))
    }

    /// Create a card and link an already-running external pane in one commit.
    /// The caller must verify the live Herdr identity before entering this UoW.
    pub fn create_card_and_adopt_uow(
        &self,
        p: &CardCreateParams,
        run: &EnqueueRun<'_>,
        workspace_id: &str,
        pane_id: &str,
    ) -> Result<(Card, crate::model::Run)> {
        let board_id = p.board_id.unwrap_or(BOARD_ID);
        self.get_board(board_id)?;
        let column_id = p.column_id.unwrap_or(self.default_column_id(board_id)?);
        let column = self.require_column(column_id)?;
        if column.board_id != board_id || run.column_id != column_id {
            return Err(Error::InvalidState(
                "adopted run column does not match new card board".into(),
            ));
        }

        let end: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position)+1, 0) FROM cards WHERE column_id=?1",
            params![column_id],
            |row| row.get(0),
        )?;
        let description = p.description.clone().unwrap_or_default();
        let harness = p
            .harness
            .clone()
            .unwrap_or_else(|| crate::harness::DEFAULT_HARNESS.to_string());
        let space_kind = p.space_kind.unwrap_or(SpaceKind::Workspace).as_str();
        let effort = p.effort.map(|value| value.as_str());
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO cards
             (board_id,column_id,position,title,description,harness,model,effort,permission_mode,
              session,space_kind,space_ref,space_cwd,status,session_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'idle',?14)",
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
                run.session_id,
            ],
        )?;
        let card_id = tx.last_insert_rowid();
        if p.position.is_some() {
            Self::place_card_in_column_tx(&tx, card_id, column_id, p.position)?;
        }
        let run_id = self.enqueue_run_tx(&tx, card_id, run)?;
        tx.execute(
            "UPDATE runs SET started_at=datetime('now'),herdr_workspace_id=?1,herdr_pane_id=?2
             WHERE id=?3 AND started_at IS NULL AND ended_at IS NULL",
            params![workspace_id, pane_id, run_id],
        )?;
        tx.execute(
            "UPDATE cards SET status='running',awaiting_reason=NULL,updated_at=datetime('now')
             WHERE id=?1",
            params![card_id],
        )?;
        tx.execute(
            "INSERT INTO comments(card_id,author,body) VALUES (?1,'system',?2)",
            params![
                card_id,
                format!("Linked existing Herdr agent in pane {pane_id}")
            ],
        )?;
        tx.commit()?;
        Ok((self.require_card(card_id)?, self.get_run(run_id)?))
    }

    /// Duplicate `id` into a fresh idle card inserted immediately below it.
    ///
    /// The copy inherits the full run configuration (title with a ` (copy)`
    /// suffix, description, harness, model, effort, permission mode, session,
    /// and space settings) but none of the execution state: status `idle`,
    /// no `session_id`, no runs, comments, or archive flag, and fresh
    /// timestamps. Duplication never enqueues a run — the caller owns whether
    /// anything is dispatched — and the whole insert + renumber happens in one
    /// transaction, so a failure leaves the column untouched.
    pub fn duplicate_card(&self, id: i64) -> Result<Card> {
        let card = self.require_card(id)?;
        let tx = self.conn.unchecked_transaction()?;
        let end: i64 = tx.query_row(
            "SELECT COALESCE(MAX(position)+1, 0) FROM cards WHERE column_id=?1",
            params![card.column_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO cards
             (board_id,column_id,position,title,description,harness,model,effort,permission_mode,
              session,space_kind,space_ref,space_cwd,status,session_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'idle',NULL)",
            params![
                card.board_id,
                card.column_id,
                end,
                format!("{} (copy)", card.title),
                card.description,
                card.harness,
                card.model,
                card.effort.map(|e| e.as_str()),
                card.permission_mode,
                card.session,
                card.space_kind.as_str(),
                card.space_ref,
                card.space_cwd,
            ],
        )?;
        let copy_id = tx.last_insert_rowid();
        // Compacts the whole column: every card from `card.position + 1` on
        // (including the fresh row at the end) shifts one slot down.
        Self::place_card_in_column_tx(&tx, copy_id, card.column_id, Some(card.position + 1))?;
        tx.commit()?;
        self.require_card(copy_id)
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
    /// both the source and target columns. The target column must belong to the
    /// card's current board; use [`Db::transfer_card`] to cross boards.
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
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE cards SET column_id=?1, updated_at=datetime('now') WHERE id=?2",
            params![target_column, id],
        )?;
        Self::place_card_in_column_tx(&tx, id, target_column, position)?;
        if old_column != target_column {
            Self::renumber_column_cards_tx(&tx, old_column)?;
        }
        tx.commit()?;
        self.require_card(id)
    }

    /// Move a card and enqueue its next run in one transaction. The caller
    /// prepares all run values before entering this DB-only unit of work.
    pub fn move_card_and_enqueue_uow(
        &self,
        id: i64,
        target_column: i64,
        position: Option<i64>,
        run: &EnqueueRun<'_>,
    ) -> Result<(Card, crate::model::Run)> {
        let card = self.require_card(id)?;
        let target = self.require_column(target_column)?;
        if target.board_id != card.board_id {
            return Err(Error::InvalidState(format!(
                "column {target_column} belongs to board {}, card {id} belongs to board {}",
                target.board_id, card.board_id
            )));
        }
        if run.card_id != id || run.column_id != target_column {
            return Err(Error::InvalidState(
                "queued run does not match moved card and destination column".into(),
            ));
        }
        if self.open_run_for_card(id)?.is_some() {
            return Err(Error::InvalidState("card already has an open run".into()));
        }

        let old_column = card.column_id;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE cards SET column_id=?1, updated_at=datetime('now') WHERE id=?2",
            params![target_column, id],
        )?;
        Self::place_card_in_column_tx(&tx, id, target_column, position)?;
        if old_column != target_column {
            Self::renumber_column_cards_tx(&tx, old_column)?;
        }
        let run_id = self.enqueue_run_tx(&tx, id, run)?;
        tx.commit()?;
        Ok((self.require_card(id)?, self.get_run(run_id)?))
    }

    /// Transfer a card and enqueue its next run in one transaction. This is
    /// the cross-board counterpart to [`Self::move_card_and_enqueue_uow`].
    pub fn transfer_card_and_enqueue_uow(
        &self,
        id: i64,
        target_board: i64,
        target_column: i64,
        position: Option<i64>,
        run: &EnqueueRun<'_>,
    ) -> Result<(Card, crate::model::Run)> {
        let card = self.require_card(id)?;
        self.get_board(target_board)?;
        let target = self.require_column(target_column)?;
        if target.board_id != target_board {
            return Err(Error::InvalidState(format!(
                "column {target_column} belongs to board {}, declared destination board is {target_board}",
                target.board_id
            )));
        }
        if run.card_id != id || run.column_id != target_column {
            return Err(Error::InvalidState(
                "queued run does not match moved card and destination column".into(),
            ));
        }
        if self.open_run_for_card(id)?.is_some() {
            return Err(Error::InvalidState("card already has an open run".into()));
        }

        let old_column = card.column_id;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE cards SET board_id=?1, column_id=?2, updated_at=datetime('now') WHERE id=?3",
            params![target_board, target_column, id],
        )?;
        Self::place_card_in_column_tx(&tx, id, target_column, position)?;
        Self::renumber_column_cards_tx(&tx, old_column)?;
        let run_id = self.enqueue_run_tx(&tx, id, run)?;
        tx.commit()?;
        Ok((self.require_card(id)?, self.get_run(run_id)?))
    }

    /// Transfer a card to `target_column` (which must belong to `target_board`)
    /// at `position` (append if `None`), atomically moving its `board_id` and
    /// `column_id` and compacting both the source and destination columns.
    /// Unlike [`Db::move_card`], this is wrapped in a single transaction so a
    /// failure leaves the source board untouched.
    pub fn transfer_card(
        &self,
        id: i64,
        target_board: i64,
        target_column: i64,
        position: Option<i64>,
    ) -> Result<Card> {
        let tx = self.conn.unchecked_transaction()?;
        let card = self.require_card(id)?;
        self.get_board(target_board)?;
        let target = self.require_column(target_column)?;
        if target.board_id != target_board {
            return Err(Error::InvalidState(format!(
                "column {target_column} belongs to board {}, declared destination board is {target_board}",
                target.board_id
            )));
        }
        let old_column = card.column_id;
        tx.execute(
            "UPDATE cards SET board_id=?1, column_id=?2, updated_at=datetime('now') WHERE id=?3",
            params![target_board, target_column, id],
        )?;
        Self::place_card_in_column_tx(&tx, id, target_column, position)?;
        Self::renumber_column_cards_tx(&tx, old_column)?;
        tx.commit()?;
        self.require_card(id)
    }

    /// (Re)place `id` within `target_column` at `position` (append if `None`),
    /// zero-basing every card's position in that column. Shared by move and
    /// transfer. Caller must already have set the card's `column_id`.
    pub(super) fn place_card_in_column_tx(
        tx: &rusqlite::Transaction<'_>,
        id: i64,
        target_column: i64,
        position: Option<i64>,
    ) -> Result<()> {
        let mut ids: Vec<i64> = tx
            .prepare("SELECT id FROM cards WHERE column_id=?1 AND id<>?2 ORDER BY position, id")?
            .query_map(params![target_column, id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let idx = position
            .map(|p| p.max(0) as usize)
            .unwrap_or(ids.len())
            .min(ids.len());
        ids.insert(idx, id);
        for (i, cid) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE cards SET position=?1 WHERE id=?2",
                params![i as i64, cid],
            )?;
        }
        Ok(())
    }

    pub(super) fn renumber_column_cards_tx(
        tx: &rusqlite::Transaction<'_>,
        column_id: i64,
    ) -> Result<()> {
        let ids: Vec<i64> = tx
            .prepare("SELECT id FROM cards WHERE column_id=?1 ORDER BY position, id")?
            .query_map(params![column_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for (i, cid) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE cards SET position=?1 WHERE id=?2",
                params![i as i64, cid],
            )?;
        }
        Ok(())
    }

    pub(super) fn renumber_column_cards(&self, column_id: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        Self::renumber_column_cards_tx(&tx, column_id)?;
        tx.commit()?;
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

    /// Get a comment's current row, including a soft-deletion marker. Unlike
    /// [`Self::list_comments`], this deliberately includes deleted records.
    pub fn get_comment(&self, id: i64) -> Result<Option<CommentRecord>> {
        rows::opt(self.conn.query_row(
            "SELECT * FROM comments WHERE id=?1",
            params![id],
            rows::row_to_comment_record,
        ))
    }

    /// Edit a user/agent comment while retaining its original owner. Every
    /// successful edit appends a snapshot; system comments are immutable at the
    /// store boundary regardless of which caller invokes this method.
    pub fn update_comment(&self, id: i64, body: &str) -> Result<CommentRecord> {
        let current = self
            .get_comment(id)?
            .ok_or_else(|| Error::NotFound(format!("comment {id}")))?;
        if current.author == "system" {
            return Err(Error::InvalidState("system comments are immutable".into()));
        }
        if current.deleted_at.is_some() {
            return Err(Error::InvalidState("deleted comments are immutable".into()));
        }
        let tx = self.conn.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE comments SET body=?1 WHERE id=?2 AND deleted_at IS NULL",
            params![body, id],
        )?;
        if changed != 1 {
            return Err(Error::NotFound(format!("comment {id}")));
        }
        tx.execute(
            "INSERT INTO comment_history(comment_id,card_id,author,body,created_at,deleted_at)
             VALUES(?1,?2,?3,?4,datetime('now'),NULL)",
            params![id, current.card_id, current.author, body],
        )?;
        tx.commit()?;
        self.get_comment(id)?
            .ok_or_else(|| Error::NotFound(format!("comment {id}")))
    }

    /// Soft-delete a user/agent comment and mark its latest audit snapshot.
    pub fn soft_delete_comment(&self, id: i64) -> Result<CommentRecord> {
        let current = self
            .get_comment(id)?
            .ok_or_else(|| Error::NotFound(format!("comment {id}")))?;
        if current.author == "system" {
            return Err(Error::InvalidState("system comments are immutable".into()));
        }
        if current.deleted_at.is_some() {
            return Err(Error::InvalidState("comment is already deleted".into()));
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE comments SET deleted_at=datetime('now') WHERE id=?1",
            params![id],
        )?;
        tx.execute(
            "UPDATE comment_history SET deleted_at=datetime('now')
             WHERE id=(SELECT MAX(id) FROM comment_history WHERE comment_id=?1)",
            params![id],
        )?;
        tx.commit()?;
        self.get_comment(id)?
            .ok_or_else(|| Error::NotFound(format!("comment {id}")))
    }

    /// Return immutable snapshots from creation through the current edit.
    /// Deletion annotates the final snapshot rather than adding a duplicate
    /// body, keeping the history a state timeline.
    pub fn list_comment_history(&self, comment_id: i64) -> Result<Vec<CommentHistory>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM comment_history WHERE comment_id=?1 ORDER BY id")?;
        let rows = stmt
            .query_map(params![comment_id], rows::row_to_comment_history)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Card detail uses the ordinary current-comment projection, so deleted
    /// comments never leak into prompts or normal card views.
    pub fn get_card_detail(&self, id: i64) -> Result<CardDetail> {
        let card = self
            .get_card(id)?
            .ok_or_else(|| Error::NotFound(format!("card {id}")))?;
        Ok(CardDetail {
            card,
            comments: self.list_comments(id)?,
            runs: self.list_runs(id)?,
        })
    }

    pub fn list_comments(&self, card_id: i64) -> Result<Vec<Comment>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM comments
             WHERE card_id=?1 AND deleted_at IS NULL ORDER BY created_at, id",
        )?;
        let rows = stmt
            .query_map(params![card_id], rows::row_to_comment)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
