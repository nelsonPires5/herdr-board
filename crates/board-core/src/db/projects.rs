//! Projects, per-project boards, the persistent selection, and recency.
//!
//! A Project is a named collection of boards identified by a canonical path
//! (Git root or plain directory); Global (scope NULL) is the special project
//! that preserves the pre-v14 flat board. The first board of every project is
//! named `main`. Selection and recency are persisted here and only ever
//! touched by explicit open/create/select operations — queries and card moves
//! go through [`Db::open_board`] / board lookups that have no side effects.

use rusqlite::{params, OptionalExtension, Transaction};

use super::rows;
use super::{constraints, Db};
use crate::model::{Board, Project};
use crate::protocol::{ProjectDetail, ProjectInfo, ProjectListResult};
use crate::{Error, Result};

/// A fresh board gets exactly one seeded column: manual `Todo` at position 0.
fn seed_todo_tx(tx: &Transaction, board_id: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO columns (board_id, name, position, trigger, fresh_session)
         SELECT ?1, 'Todo', 0, 'manual', 0
         WHERE NOT EXISTS (SELECT 1 FROM columns WHERE board_id = ?1)",
        params![board_id],
    )?;
    Ok(())
}

impl Db {
    // -- projects -----------------------------------------------------------

    pub fn get_project(&self, id: i64) -> Result<Project> {
        self.conn
            .query_row(
                "SELECT id, scope_path FROM projects WHERE id=?1",
                params![id],
                rows::row_to_project,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("project {id}")),
                other => Error::Sqlite(other),
            })
    }

    pub fn get_project_by_scope(&self, scope_path: &str) -> Result<Option<Project>> {
        rows::opt(self.conn.query_row(
            "SELECT id, scope_path FROM projects WHERE scope_path=?1",
            params![scope_path],
            rows::row_to_project,
        ))
    }

    /// `project.get` / `project.select` share this error: selecting a project
    /// that does not exist must point the caller at the create command.
    pub fn require_project_by_scope(&self, scope_path: &str) -> Result<Project> {
        self.get_project_by_scope(scope_path)?.ok_or_else(|| {
            Error::NotFound(format!(
                "project {scope_path:?} not found; create it with `board project create {scope_path}`"
            ))
        })
    }

    /// All projects, ordered by folder name (case-insensitive) with the
    /// special Global project last — the deterministic picker source order.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare("SELECT id, scope_path FROM projects")?;
        let mut projects = stmt
            .query_map([], rows::row_to_project)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        projects.sort_by(|a, b| {
            (a.scope_path.is_none(), a.name.to_lowercase())
                .cmp(&(b.scope_path.is_none(), b.name.to_lowercase()))
        });
        Ok(projects)
    }

    /// Get-or-create the project for an already-canonical path, seeding its
    /// first board `main` (with one manual `Todo` column) on creation. This is
    /// the resolution primitive behind `board.open`: it never touches the
    /// persistent selection or recency.
    pub fn get_or_create_project(&self, scope_path: &str) -> Result<Project> {
        if scope_path.trim().is_empty() {
            return Err(Error::BadRequest("scope_path must not be empty".into()));
        }
        if let Some(project) = self.get_project_by_scope(scope_path)? {
            return Ok(project);
        }
        let tx = self.conn.unchecked_transaction()?;
        // Re-check inside the transaction so a concurrent open cannot
        // double-create (the UNIQUE index is the final guard either way).
        let existing = tx
            .query_row(
                "SELECT id, scope_path FROM projects WHERE scope_path=?1",
                params![scope_path],
                rows::row_to_project,
            )
            .optional()?;
        if let Some(project) = existing {
            tx.commit()?;
            return Ok(project);
        }
        tx.execute(
            "INSERT INTO projects (scope_path) VALUES (?1)",
            params![scope_path],
        )?;
        let project_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO boards (project_id, name) VALUES (?1, 'main')",
            params![project_id],
        )?;
        let board_id = tx.last_insert_rowid();
        seed_todo_tx(&tx, board_id)?;
        tx.commit()?;
        self.get_project(project_id)
    }

    /// The board a project context lands on: its persisted selected board,
    /// else its first board `main` (falling back to the earliest board if the
    /// `main` name was renamed away).
    pub fn project_context_board(&self, project_id: i64) -> Result<Board> {
        if let Some(board) = self.selected_board_for(project_id)? {
            return Ok(board);
        }
        self.main_board(project_id)
    }

    fn main_board(&self, project_id: i64) -> Result<Board> {
        let main = self
            .conn
            .query_row(
                &format!(
                    "{} WHERE b.project_id=?1 AND b.name='main' COLLATE NOCASE",
                    rows::BOARD_SELECT
                ),
                params![project_id],
                rows::row_to_board,
            )
            .optional()?;
        if let Some(board) = main {
            return Ok(board);
        }
        self.conn
            .query_row(
                &format!(
                    "{} WHERE b.project_id=?1 ORDER BY b.id LIMIT 1",
                    rows::BOARD_SELECT
                ),
                params![project_id],
                rows::row_to_board,
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    Error::NotFound(format!("project {project_id} has no boards"))
                }
                other => Error::Sqlite(other),
            })
    }

    /// `board.open` resolution: get-or-create the project for the scope and
    /// return the board its context lands on. Never touches selection or
    /// recency — queries and card moves share this path, and the spec says
    /// they must not update recency.
    pub fn open_board(&self, scope_path: &str) -> Result<Board> {
        let project = self.get_or_create_project(scope_path)?;
        self.project_context_board(project.id)
    }

    /// `project.create`: the project must not exist yet (folder existence is
    /// validated by the caller), then create it with its first board `main`,
    /// select project + board, and touch both recencies in one transaction.
    pub fn create_project_context(&self, scope_path: &str) -> Result<(Project, Board)> {
        if scope_path.trim().is_empty() {
            return Err(Error::BadRequest("scope_path must not be empty".into()));
        }
        let tx = self.conn.unchecked_transaction()?;
        constraints::reject_duplicate(
            tx.execute(
                "INSERT INTO projects (scope_path) VALUES (?1)",
                params![scope_path],
            ),
            || {
                format!(
                    "project {scope_path:?} already exists; select it with `board project select {scope_path}`"
                )
            },
        )?;
        let project_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO boards (project_id, name) VALUES (?1, 'main')",
            params![project_id],
        )?;
        let board_id = tx.last_insert_rowid();
        seed_todo_tx(&tx, board_id)?;
        Self::write_selection_tx(&tx, project_id, board_id)?;
        Self::touch_project_recency_tx(&tx, project_id)?;
        Self::touch_board_recency_tx(&tx, project_id, board_id)?;
        tx.commit()?;
        Ok((self.get_project(project_id)?, self.get_board(board_id)?))
    }

    /// `project.open`: explicit opening — get-or-create the project, land on
    /// its context board, persist selection, and touch recency.
    pub fn open_project_context(&self, scope_path: &str) -> Result<(Project, Board)> {
        if scope_path.trim().is_empty() {
            return Err(Error::BadRequest("scope_path must not be empty".into()));
        }
        let project = self.get_or_create_project(scope_path)?;
        let board = self.project_context_board(project.id)?;
        let tx = self.conn.unchecked_transaction()?;
        Self::write_selection_tx(&tx, project.id, board.id)?;
        Self::touch_project_recency_tx(&tx, project.id)?;
        Self::touch_board_recency_tx(&tx, project.id, board.id)?;
        tx.commit()?;
        Ok((project, board))
    }

    /// `project.select`: the project must exist; the optional explicit board
    /// choice must belong to it. Persists selection and recency.
    pub fn select_project_by_scope(
        &self,
        scope_path: &str,
        board_id: Option<i64>,
    ) -> Result<(Project, Board)> {
        let project = self.require_project_by_scope(scope_path)?;
        let board = match board_id {
            Some(id) => {
                let board = self.get_board(id)?;
                if board.project_id != project.id {
                    return Err(Error::InvalidState(format!(
                        "board {id} belongs to project {}, selected project is {}",
                        board.project_id, project.id
                    )));
                }
                board
            }
            None => self.project_context_board(project.id)?,
        };
        let tx = self.conn.unchecked_transaction()?;
        Self::write_selection_tx(&tx, project.id, board.id)?;
        Self::touch_project_recency_tx(&tx, project.id)?;
        Self::touch_board_recency_tx(&tx, project.id, board.id)?;
        tx.commit()?;
        Ok((project, board))
    }

    /// `board.select`: persist this board — and its project — as the context.
    pub fn select_board(&self, board_id: i64) -> Result<(Project, Board)> {
        let board = self.get_board(board_id)?;
        let project = self.get_project(board.project_id)?;
        let tx = self.conn.unchecked_transaction()?;
        Self::write_selection_tx(&tx, project.id, board.id)?;
        Self::touch_project_recency_tx(&tx, project.id)?;
        Self::touch_board_recency_tx(&tx, project.id, board.id)?;
        tx.commit()?;
        Ok((project, board))
    }

    /// `board.create`: a named board in a project, seeded with the `Todo`
    /// column and auto-selected (its project becomes the selected project).
    /// Name uniqueness is case-insensitive within the project only.
    pub fn create_board(&self, project_id: i64, name: &str) -> Result<Board> {
        if name.trim().is_empty() {
            return Err(Error::BadRequest("board name must not be empty".into()));
        }
        self.get_project(project_id)?;
        let tx = self.conn.unchecked_transaction()?;
        constraints::reject_duplicate(
            tx.execute(
                "INSERT INTO boards (project_id, name) VALUES (?1, ?2)",
                params![project_id, name],
            ),
            || constraints::duplicate_board(name),
        )?;
        let board_id = tx.last_insert_rowid();
        seed_todo_tx(&tx, board_id)?;
        Self::write_selection_tx(&tx, project_id, board_id)?;
        Self::touch_project_recency_tx(&tx, project_id)?;
        Self::touch_board_recency_tx(&tx, project_id, board_id)?;
        tx.commit()?;
        self.get_board(board_id)
    }

    // -- selection & recency ---------------------------------------------------

    fn selection_project_id(&self) -> Result<Option<i64>> {
        rows::opt(
            self.conn
                .query_row("SELECT project_id FROM selection WHERE id=1", [], |r| {
                    r.get(0)
                }),
        )
    }

    pub fn selected_project_id(&self) -> Result<Option<i64>> {
        self.selection_project_id()
    }

    pub fn selected_project(&self) -> Result<Option<Project>> {
        match self.selection_project_id()? {
            Some(id) => self.get_project(id).map(Some),
            None => Ok(None),
        }
    }

    pub fn selected_board_id_for(&self, project_id: i64) -> Result<Option<i64>> {
        rows::opt(self.conn.query_row(
            "SELECT board_id FROM board_selection WHERE project_id=?1",
            params![project_id],
            |r| r.get(0),
        ))
    }

    pub fn selected_board_for(&self, project_id: i64) -> Result<Option<Board>> {
        match self.selected_board_id_for(project_id)? {
            Some(id) => self.get_board(id).map(Some),
            None => Ok(None),
        }
    }

    /// Recent project ids, most recent first, capped at 3, excluding the
    /// selected project (the picker shows the current separately).
    pub fn recent_project_ids_excluding(&self, exclude: Option<i64>) -> Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT project_id FROM project_recents ORDER BY rank LIMIT 3")?;
        let ids = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids.into_iter().filter(|id| Some(*id) != exclude).collect())
    }

    /// One project's recent board ids, most recent first, capped at 3,
    /// excluding its selected board.
    pub fn recent_board_ids_excluding(
        &self,
        project_id: i64,
        exclude: Option<i64>,
    ) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT board_id FROM board_recents WHERE project_id=?1 ORDER BY rank LIMIT 3",
        )?;
        let ids = stmt
            .query_map(params![project_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids.into_iter().filter(|id| Some(*id) != exclude).collect())
    }

    fn write_selection_tx(tx: &Transaction, project_id: i64, board_id: i64) -> Result<()> {
        tx.execute(
            "INSERT INTO selection (id, project_id) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET project_id=excluded.project_id, updated_at=datetime('now')",
            params![project_id],
        )?;
        tx.execute(
            "INSERT INTO board_selection (project_id, board_id) VALUES (?1, ?2)
             ON CONFLICT(project_id) DO UPDATE SET board_id=excluded.board_id, updated_at=datetime('now')",
            params![project_id, board_id],
        )?;
        Ok(())
    }

    /// Move `project_id` to the front of the project recency list and cap at 3.
    fn touch_project_recency_tx(tx: &Transaction, project_id: i64) -> Result<()> {
        let mut ids: Vec<i64> = tx
            .prepare("SELECT project_id FROM project_recents ORDER BY rank")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids.retain(|id| *id != project_id);
        ids.insert(0, project_id);
        ids.truncate(3);
        tx.execute("DELETE FROM project_recents", [])?;
        for (rank, id) in ids.iter().enumerate() {
            tx.execute(
                "INSERT INTO project_recents (project_id, rank) VALUES (?1, ?2)",
                params![id, rank as i64],
            )?;
        }
        Ok(())
    }

    /// Move `board_id` to the front of its project's board recency list, cap 3.
    fn touch_board_recency_tx(tx: &Transaction, project_id: i64, board_id: i64) -> Result<()> {
        let mut ids: Vec<i64> = tx
            .prepare("SELECT board_id FROM board_recents WHERE project_id=?1 ORDER BY rank")?
            .query_map(params![project_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        ids.retain(|id| *id != board_id);
        ids.insert(0, board_id);
        ids.truncate(3);
        tx.execute(
            "DELETE FROM board_recents WHERE project_id=?1",
            params![project_id],
        )?;
        for (rank, id) in ids.iter().enumerate() {
            tx.execute(
                "INSERT INTO board_recents (project_id, board_id, rank) VALUES (?1, ?2, ?3)",
                params![project_id, id, rank as i64],
            )?;
        }
        Ok(())
    }

    // -- serving ----------------------------------------------------------------

    /// The full `project.list` payload: per-project boards plus the selection
    /// and recency data the pickers need, deterministically ordered.
    pub fn project_list_result(&self) -> Result<ProjectListResult> {
        let projects = self.list_projects()?;
        let selected_project_id = self.selected_project_id()?;
        let recent_project_ids = self.recent_project_ids_excluding(selected_project_id)?;
        let mut infos = Vec::with_capacity(projects.len());
        for project in projects {
            let mut boards = self.list_boards_for_project(project.id)?;
            boards.sort_by_key(|a| a.name.to_lowercase());
            let selected_board_id = self.selected_board_id_for(project.id)?;
            let recent_board_ids =
                self.recent_board_ids_excluding(project.id, selected_board_id)?;
            infos.push(ProjectInfo {
                project,
                boards,
                selected_board_id,
                recent_board_ids,
            });
        }
        Ok(ProjectListResult {
            projects: infos,
            selected_project_id,
            recent_project_ids,
        })
    }

    /// `project.get`: one project plus its boards, no side effects.
    pub fn project_detail(&self, scope_path: &str) -> Result<ProjectDetail> {
        let project = self.require_project_by_scope(scope_path)?;
        let mut boards = self.list_boards_for_project(project.id)?;
        boards.sort_by_key(|a| a.name.to_lowercase());
        let selected_board = self.selected_board_for(project.id)?;
        Ok(ProjectDetail {
            project,
            boards,
            selected_board,
        })
    }
}
