use anyhow::Result;
use board_core::client::BoardClient;

use crate::args::BoardCmd;
use crate::daemon::connect_or_start;
use crate::helpers::print_json;
use crate::scope::open_selected_board;
use board_core::scope::resolve_scope_path;

pub(crate) fn cmd_board(sub: BoardCmd, selector: Option<&str>) -> Result<()> {
    let mut c = connect_or_start()?;
    match sub {
        BoardCmd::List { json } => {
            let result = c.board_list()?;
            if json {
                print_json(&result.boards)?;
            } else {
                for board in result.boards {
                    println!(
                        "#{}\t{}{}",
                        board.id,
                        board.name,
                        board
                            .scope_path
                            .map(|path| format!("\t{path}"))
                            .unwrap_or_default()
                    );
                }
            }
        }
        BoardCmd::Show {
            selector: local,
            json,
        } => {
            let board = open_selected_board(&mut c, local.as_deref().or(selector))?;
            if json {
                print_json(&board)?;
            } else {
                println!("#{}\t{}", board.board.id, board.board.name);
                if let Some(path) = board.board.scope_path {
                    println!("scope: {path}");
                }
                println!(
                    "columns: {}\tcards: {}",
                    board.columns.len(),
                    board.cards.len()
                );
            }
        }
        BoardCmd::Open { path, json } => {
            let path = resolve_scope_path(std::path::Path::new(&path))?;
            let path = path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("board path is not valid UTF-8"))?;
            let board = c.board_open(path)?;
            if json {
                print_json(&board)?;
            } else {
                println!("Opened board #{} {}", board.board.id, board.board.name);
            }
        }
        BoardCmd::Rename { values, json } => {
            let (local_selector, name) = match values.as_slice() {
                [name] => (None, name.as_str()),
                [local_selector, name] => (Some(local_selector.as_str()), name.as_str()),
                _ => {
                    return Err(anyhow::anyhow!(
                        "board rename expects <NAME> or <ID|PATH> <NAME>"
                    ))
                }
            };
            let board = open_selected_board(&mut c, local_selector.or(selector))?;
            let renamed = c.board_rename(board.board.id, name)?;
            if json {
                print_json(&renamed)?;
            } else {
                println!("Renamed board #{} to {}", renamed.id, renamed.name);
            }
        }
    }
    Ok(())
}
