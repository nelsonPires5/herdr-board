//! Project commands: list, show, create, and select projects (and boards).

use anyhow::Result;
use board_core::client::BoardClient;

use crate::args::ProjectCmd;
use crate::commands::board::resolve_board_in_list;
use crate::context::Ctx;
use crate::render::{emit, emit_line};
use crate::scope::{current_scope_path, resolved_scope_path};

pub(crate) fn cmd_project(sub: ProjectCmd, ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    match sub {
        ProjectCmd::List => {
            let result = ctx.client()?.project_list()?;
            emit(&result, json)
        }
        ProjectCmd::Show { path } => {
            let scope = match path {
                Some(path) => resolved_scope_path(&path)?,
                // No path: the selected project, or the current directory's
                // project when nothing is selected yet (bootstrap — a get,
                // never a select).
                None => match ctx.client()?.project_selected()?.project {
                    Some(project) => match project.scope_path {
                        Some(scope) => scope,
                        // The special Global project has no path; fall back to
                        // the current directory's project.
                        None => current_scope_path()?,
                    },
                    None => current_scope_path()?,
                },
            };
            let detail = ctx.client()?.project_get(&scope)?;
            emit(&detail, json)
        }
        ProjectCmd::Create { path } => {
            let scope = resolved_scope_path(&path)?;
            let result = ctx.client()?.project_create(&scope)?;
            emit_line(
                &result,
                json,
                format!(
                    "Created project {} ({})",
                    result.project.name,
                    result.project.scope_path.as_deref().unwrap_or("(global)")
                ),
            )
        }
        ProjectCmd::Select { path, board } => {
            let scope = resolved_scope_path(&path)?;
            let board_id = match board {
                Some(reference) => {
                    let detail = ctx.client()?.project_get(&scope)?;
                    let boards = detail.boards.to_vec();
                    let board = resolve_board_in_list(
                        &boards,
                        &reference,
                        &format!("project {:?}", scope),
                    )?;
                    Some(board.id)
                }
                None => None,
            };
            let result = ctx.client()?.project_select(&scope, board_id)?;
            emit_line(
                &result,
                json,
                format!(
                    "Selected project {} (board {})",
                    result.project.name, result.board.board.name
                ),
            )
        }
    }
}
