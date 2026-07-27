use anyhow::{anyhow, Result};
use board_core::client::BoardClient;
use board_core::scope::resolve_scope_path;

use crate::args::BoardCmd;
use crate::context::Ctx;
use crate::render::{emit, emit_line};
use crate::scope::open_selected_board;

pub(crate) fn cmd_board(sub: BoardCmd, ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    match sub {
        BoardCmd::List => {
            let boards = ctx.client()?.board_list()?.boards;
            emit(&boards, json)
        }
        BoardCmd::Show { selector: local } => {
            let board = match local.as_deref() {
                // A command-local selector overrides the memoized global one.
                Some(local) => open_selected_board(ctx.client()?, Some(local))?,
                None => ctx.board()?.clone(),
            };
            emit(&board, json)
        }
        BoardCmd::Open { path } => {
            let path = resolve_scope_path(std::path::Path::new(&path))?;
            let path = path
                .to_str()
                .ok_or_else(|| anyhow!("board path is not valid UTF-8"))?
                .to_string();
            let board = ctx.client()?.board_open(&path)?;
            emit_line(
                &board,
                json,
                format!("Opened board #{} {}", board.board.id, board.board.name),
            )
        }
        BoardCmd::Rename { values } => {
            let (local_selector, name) = match values.as_slice() {
                [name] => (None, name.clone()),
                [local_selector, name] => (Some(local_selector.clone()), name.clone()),
                _ => return Err(anyhow!("board rename expects <NAME> or <ID|PATH> <NAME>")),
            };
            let board_id = match local_selector.as_deref() {
                Some(local) => open_selected_board(ctx.client()?, Some(local))?.board.id,
                None => ctx.board_id()?,
            };
            let renamed = ctx.client()?.board_rename(board_id, &name)?;
            emit_line(
                &renamed,
                json,
                format!("Renamed board #{} to {}", renamed.id, renamed.name),
            )
        }
    }
}
