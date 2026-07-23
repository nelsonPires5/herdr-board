use anyhow::Result;

use crate::args::ColumnCmd;
use crate::daemon::connect_or_start;
use crate::helpers::print_json;
use crate::scope::open_current_board;

pub(crate) fn cmd_column(sub: ColumnCmd) -> Result<()> {
    let mut c = connect_or_start()?;
    match sub {
        ColumnCmd::List { json } => {
            let snap = open_current_board(&mut c)?;
            if json {
                print_json(&snap.columns)?;
            } else {
                for col in &snap.columns {
                    println!(
                        "#{}\tpos={}\t[{}]\t{}",
                        col.id, col.position, col.trigger, col.name
                    );
                }
            }
        }
    }
    Ok(())
}
