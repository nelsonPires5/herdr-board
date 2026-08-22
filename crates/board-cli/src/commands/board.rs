use std::path::Path;

use anyhow::{anyhow, Result};
use board_core::client::BoardClient;
use board_core::model::Board;
use board_core::protocol::{BoardSnapshot, Visibility};

use crate::args::BoardCmd;
use crate::context::Ctx;
use crate::render::{emit, emit_line};
use crate::scope::{current_scope_path, resolved_scope_path};

fn parse_visibility(s: Option<&str>) -> Result<Option<Visibility>> {
    match s {
        None => Ok(None),
        Some(v) => Visibility::parse_str(v)
            .map(Some)
            .ok_or_else(|| anyhow!("unknown visibility: {v}")),
    }
}

pub(crate) fn cmd_board(sub: BoardCmd, ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    match sub {
        BoardCmd::List {
            all,
            project,
            visibility,
        } => {
            let vis = parse_visibility(visibility.as_deref())?;
            if all {
                let boards = ctx.client()?.board_list_visible(None, vis)?.boards;
                return emit(&boards, json);
            }
            let project_id = project_id_for(ctx, project.as_deref())?;
            // When visibility filter is used without --all, we must scope to that project but
            // apply the requested visibility. board_list_visible with project_id does that.
            let boards = ctx
                .client()?
                .board_list_visible(Some(project_id), vis)?
                .boards;
            emit(&boards, json)
        }
        BoardCmd::Show { selector: local } => {
            let board = match local.as_deref() {
                // A command-local selector overrides the memoized global one.
                Some(local) => resolve_board(ctx, local)?,
                None => ctx.board()?.clone(),
            };
            emit(&board, json)
        }
        BoardCmd::Open { path } => {
            // `board open` keeps its legacy output contract (a BoardSnapshot):
            // opening a path now opens the project's context board, and the
            // richer project payload is available via `board project open`.
            let scope = resolved_scope_path(&path)?;
            let result = ctx.client()?.project_open(&scope)?;
            emit_line(
                &result.board,
                json,
                format!(
                    "Opened project {} (board {})",
                    result.project.name, result.board.board.name
                ),
            )
        }
        BoardCmd::Create { name, project } => {
            let project_id = project_id_for(ctx, project.as_deref())?;
            let board = ctx.client()?.board_create(project_id, &name)?;
            emit_line(
                &board,
                json,
                format!(
                    "Created board #{} {} in project {}",
                    board.board.id, board.board.name, project_id
                ),
            )
        }
        BoardCmd::Select { selector, project } => {
            let project_id = project_id_for(ctx, project.as_deref())?;
            let boards = ctx.client()?.board_list_for_project(project_id)?.boards;
            let board =
                resolve_board_in_list(&boards, &selector, &format!("project {project_id}"))?;
            let board = ctx.client()?.board_select(board.id)?;
            emit_line(
                &board,
                json,
                format!("Selected board #{} {}", board.board.id, board.board.name),
            )
        }
        BoardCmd::Rename { values } => {
            let (local_selector, name) = match values.as_slice() {
                [name] => (None, name.clone()),
                [local_selector, name] => (Some(local_selector.clone()), name.clone()),
                _ => {
                    return Err(anyhow!(
                        "board rename expects <NAME> or <ID|PATH|NAME> <NAME>"
                    ))
                }
            };
            let board_id = match local_selector.as_deref() {
                Some(local) => resolve_board(ctx, local)?.board.id,
                None => ctx.board_id()?,
            };
            let renamed = ctx.client()?.board_rename(board_id, &name)?;
            emit_line(
                &renamed,
                json,
                format!("Renamed board #{} to {}", renamed.id, renamed.name),
            )
        }
        BoardCmd::Archive { selector } => {
            let board_id = match selector.as_deref() {
                Some(local) => resolve_board(ctx, local)?.board.id,
                None => ctx.board_id()?,
            };
            let board = ctx.client()?.board_archive(board_id, true)?;
            emit_line(
                &board,
                json,
                format!("Archived board #{} {}", board.id, board.name),
            )
        }
        BoardCmd::Restore { selector } => {
            let board_id = match selector.as_deref() {
                Some(local) => resolve_board_for_restore(ctx, local)?.board.id,
                None => ctx.board_id()?,
            };
            let board = ctx.client()?.board_archive(board_id, false)?;
            emit_line(
                &board,
                json,
                format!("Restored board #{} {}", board.id, board.name),
            )
        }
    }
}

/// The project a board command targets: an explicit `--project <path>`
/// (canonicalized; a NotFound propagates with the create hint), else the
/// daemon's selected project, else the current directory's project opened as
/// the bootstrap (an explicit open, allowed — it selects).
fn project_id_for(ctx: &mut Ctx, explicit: Option<&str>) -> Result<i64> {
    let c = ctx.client()?;
    match explicit {
        Some(path) => {
            let scope = resolved_scope_path(path)?;
            Ok(c.project_get(&scope)?.project.id)
        }
        None => match c.project_selected()?.project {
            Some(project) => Ok(project.id),
            None => Ok(c.project_open(&current_scope_path()?)?.project.id),
        },
    }
}

/// Resolve a board selector in the `<ID|PATH|NAME>` grammar: a numeric id
/// (any project), else a scope path (`board.open`, non-selecting), else a
/// board name within the context project.
fn resolve_board(ctx: &mut Ctx, value: &str) -> Result<BoardSnapshot> {
    if let Ok(id) = value.parse::<i64>() {
        return ctx.client()?.board_get_by_id(id);
    }
    let path = Path::new(value);
    if value.contains('/') || path.exists() {
        let scope = resolved_scope_path(value)?;
        return ctx.client()?.board_open(&scope);
    }
    let project_id = project_id_for(ctx, None)?;
    let boards = ctx.client()?.board_list_for_project(project_id)?.boards;
    let board = resolve_board_in_list(&boards, value, &format!("project {project_id}"))?;
    ctx.client()?.board_get_by_id(board.id)
}

fn resolve_board_for_restore(ctx: &mut Ctx, value: &str) -> Result<BoardSnapshot> {
    if let Ok(id) = value.parse::<i64>() {
        return ctx.client()?.board_get_by_id(id);
    }
    let path = Path::new(value);
    if value.contains('/') || path.exists() {
        let scope = resolved_scope_path(value)?;
        return ctx.client()?.board_open(&scope);
    }
    let project_id = project_id_for(ctx, None)?;
    // Include archived boards so a by-name restore can find them.
    let boards = ctx
        .client()?
        .board_list_visible(Some(project_id), Some(Visibility::All))?
        .boards;
    let board = resolve_board_in_list(&boards, value, &format!("project {project_id}"))?;
    ctx.client()?.board_get_by_id(board.id)
}

/// Resolve an id-or-name board reference within one project's board list. A
/// numeric reference must be one of the listed boards (never a bare
/// cross-project id); anything else is a case-insensitive name match.
pub(crate) fn resolve_board_in_list(
    boards: &[Board],
    reference: &str,
    project_label: &str,
) -> Result<Board> {
    if let Ok(id) = reference.parse::<i64>() {
        return boards
            .iter()
            .find(|board| board.id == id)
            .cloned()
            .ok_or_else(|| anyhow!("no board with id {id} in {project_label}"));
    }
    boards
        .iter()
        .find(|board| board.name.eq_ignore_ascii_case(reference))
        .cloned()
        .ok_or_else(|| anyhow!("no board named {reference:?} in {project_label}"))
}
