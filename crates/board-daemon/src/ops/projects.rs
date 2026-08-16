//! Project request operations: listing, opening, creating, and selecting
//! projects (and their boards). Selection/recency side effects live in the
//! store (`Db::*_context`); these handlers only assemble wire payloads.

use super::boards::board_snapshot;
use super::*;
use board_core::protocol::{
    ProjectCreateParams, ProjectGetParams, ProjectOpenParams, ProjectOpenResult,
    ProjectSelectParams, ProjectSelectedResult,
};

pub(super) fn project_list(d: &Arc<Daemon>) -> Result<Value> {
    Ok(json!(d.store.lock().project_list_result()?))
}

pub(super) fn project_get(d: &Arc<Daemon>, p: ProjectGetParams) -> Result<Value> {
    Ok(json!(d.store.lock().project_detail(&p.scope_path)?))
}

fn project_open_result(
    d: &Arc<Daemon>,
    pair: (board_core::model::Project, board_core::model::Board),
) -> Result<Value> {
    let (project, board) = pair;
    Ok(json!(ProjectOpenResult {
        project,
        board: board_snapshot(d, board.id)?,
    }))
}

/// `project.open`: explicit opening — get-or-create the project for the path,
/// land on its context board, persist selection, and update recency.
pub(super) fn project_open(d: &Arc<Daemon>, p: ProjectOpenParams) -> Result<Value> {
    let pair = d.store.lock().open_project_context(&p.scope_path)?;
    project_open_result(d, pair)
}

/// `project.create`: the folder must exist on disk (creating a project never
/// creates directories); the project must not exist yet. Selecting the new
/// project and its `main` board is part of the creation.
pub(super) fn project_create(d: &Arc<Daemon>, p: ProjectCreateParams) -> Result<Value> {
    board_core::scope::validate_existing_directory(&p.scope_path)?;
    let pair = d.store.lock().create_project_context(&p.scope_path)?;
    project_open_result(d, pair)
}

/// `project.select`: the project must exist (the error points at the create
/// command); an explicit board choice is optional.
pub(super) fn project_select(d: &Arc<Daemon>, p: ProjectSelectParams) -> Result<Value> {
    let pair = d
        .store
        .lock()
        .select_project_by_scope(&p.scope_path, p.board_id)?;
    project_open_result(d, pair)
}

/// `project.selected`: the persisted context, or both `None` before the first
/// board-aware command bootstraps it from the current directory.
pub(super) fn project_selected(d: &Arc<Daemon>) -> Result<Value> {
    // Bind the selection before matching: a guard temporary created in a
    // match scrutinee lives for the whole match (arms included), and
    // `board_snapshot` below takes the store lock itself — a non-reentrant
    // Mutex would self-deadlock. `let`-initializer temporaries drop at the
    // statement end, so each acquisition below is short-lived.
    let selected = d.store.lock().selected_project()?;
    let result = match selected {
        Some(project) => {
            let board = d.store.lock().project_context_board(project.id)?;
            let snap = board_snapshot(d, board.id)?;
            ProjectSelectedResult {
                project: Some(project),
                board: Some(snap),
            }
        }
        None => ProjectSelectedResult::default(),
    };
    Ok(json!(result))
}
