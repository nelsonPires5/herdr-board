use anyhow::{anyhow, Result};
use board_core::client::{BoardClient, UnixClient};
use board_core::protocol::{CardCreateParams, Effort};

use crate::args::CardCmd;
use crate::daemon::connect_or_start;
use crate::helpers::{parse_space_kind, print_json};
use crate::scope::{open_current_board, resolve_column_in};

pub(crate) fn cmd_card(sub: CardCmd) -> Result<()> {
    let mut c = connect_or_start()?;
    match sub {
        CardCmd::New {
            title,
            description,
            column,
            harness,
            model,
            effort,
            permission,
            session,
            space_kind,
            space_ref,
            space_cwd,
            json,
        } => {
            let board = open_current_board(&mut c)?;
            let column_id = match column {
                Some(s) => Some(resolve_column_in(&board, &s)?),
                None => None,
            };
            let effort = match effort {
                Some(s) => {
                    Some(Effort::parse_str(&s).ok_or_else(|| anyhow!("invalid effort: {s}"))?)
                }
                None => None,
            };
            let space_kind = match space_kind {
                Some(s) => Some(parse_space_kind(&s)?),
                None => None,
            };
            let p = CardCreateParams {
                title,
                board_id: Some(board.board.id),
                description,
                column_id,
                harness,
                model,
                effort,
                permission_mode: permission,
                session,
                space_kind,
                space_ref,
                space_cwd,
                position: None,
            };
            let card = c.card_create(&p)?;
            if json {
                print_json(&card)?;
            } else {
                println!(
                    "Created card #{} \"{}\" in column {}",
                    card.id, card.title, card.column_id
                );
            }
        }
        CardCmd::Archive { id, json } => card_archive(&mut c, id, true, json)?,
        CardCmd::Restore { id, json } => card_archive(&mut c, id, false, json)?,
        CardCmd::Show { id, json } => {
            let d = c.card_get(id)?;
            if json {
                print_json(&d)?;
            } else {
                println!(
                    "#{} {}  [{}{}]",
                    d.card.id,
                    d.card.title,
                    d.card.status,
                    if d.card.archived_at.is_some() {
                        ", archived"
                    } else {
                        ""
                    }
                );
                if let Some(session) = &d.card.session {
                    println!("session: {session}");
                }
                if !d.card.description.is_empty() {
                    println!("\n{}", d.card.description);
                }
                if !d.comments.is_empty() {
                    println!("\nComments:");
                    for cm in &d.comments {
                        println!(
                            "  [{}] {} ({}): {}",
                            cm.id, cm.author, cm.created_at, cm.body
                        );
                    }
                }
                if !d.runs.is_empty() {
                    println!("\nRuns:");
                    for r in &d.runs {
                        println!(
                            "  #{} col={} {} started={:?} ended={:?}",
                            r.id,
                            r.column_id,
                            r.outcome
                                .map(|o| o.to_string())
                                .unwrap_or_else(|| "-".into()),
                            r.started_at,
                            r.ended_at
                        );
                    }
                }
            }
        }
        CardCmd::List { column, json } => {
            let board = open_current_board(&mut c)?;
            let column_id = match column {
                Some(s) => Some(resolve_column_in(&board, &s)?),
                None => None,
            };
            let cards = c.card_list_for_board(Some(board.board.id), column_id)?;
            if json {
                print_json(&cards)?;
            } else {
                for card in &cards {
                    let session = card
                        .session
                        .as_deref()
                        .map(|s| format!("\tsession={s}"))
                        .unwrap_or_default();
                    let archived = if card.archived_at.is_some() {
                        "\tarchived"
                    } else {
                        ""
                    };
                    println!(
                        "#{}\t[{}]\tcol={}\t{}{}{}",
                        card.id, card.status, card.column_id, card.title, session, archived
                    );
                }
            }
        }
    }
    Ok(())
}

fn card_archive(c: &mut UnixClient, id: i64, archived: bool, json: bool) -> Result<()> {
    let card = c.card_archive(id, archived)?;
    if json {
        print_json(&card)?;
    } else if archived {
        println!("Archived card #{}", card.id);
    } else {
        println!("Restored card #{}", card.id);
    }
    Ok(())
}
