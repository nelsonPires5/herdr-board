use anyhow::Result;
use board_core::client::BoardClient;

use crate::args::TemplateCmd;
use crate::daemon::connect_or_start;
use crate::helpers::print_json;
use crate::scope::open_selected_board;

pub(crate) fn cmd_template(sub: TemplateCmd, selector: Option<&str>) -> Result<()> {
    let mut c = connect_or_start()?;
    match sub {
        TemplateCmd::Apply { name, json } => {
            let board = open_selected_board(&mut c, selector)?;
            let columns = c.template_apply_for_board(&name, Some(board.board.id))?;
            if json {
                print_json(&columns)?;
            } else {
                println!("Applied template {name} to board #{}", board.board.id);
                for column in columns {
                    println!("#{}\t{}", column.id, column.name);
                }
            }
        }
    }
    Ok(())
}
