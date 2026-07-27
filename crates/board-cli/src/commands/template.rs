use anyhow::Result;
use board_core::client::BoardClient;

use crate::args::TemplateCmd;
use crate::context::Ctx;
use crate::render::{emit_line, table};

pub(crate) fn cmd_template(sub: TemplateCmd, ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    match sub {
        TemplateCmd::Apply { name } => {
            let board_id = ctx.board_id()?;
            let columns = ctx
                .client()?
                .template_apply_for_board(&name, Some(board_id))?;
            let mut text = format!("Applied template {name} to board #{board_id}\n");
            let rows: Vec<Vec<String>> = columns
                .iter()
                .map(|column| vec![format!("#{}", column.id), column.name.clone()])
                .collect();
            let mut rendered = Vec::new();
            table(&mut rendered, &rows)?;
            text.push_str(&String::from_utf8_lossy(&rendered));
            emit_line(&columns, json, text.trim_end().to_string())
        }
    }
}
