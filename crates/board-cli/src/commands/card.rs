use anyhow::{bail, Result};
use board_core::client::{BoardClient, UnixClient};
use board_core::protocol::{CardCreateParams, CardMoveParams, CardUpdateParams, Patch};

use crate::args::CardCmd;
use crate::commands::run::{cmd_card_comment, cmd_card_run};
use crate::daemon::connect_or_start;
use crate::helpers::{
    confirm_action, parse_effort, parse_space_kind, parse_visibility, print_json,
};
use crate::scope::{open_selected_board, resolve_column_in};

pub(crate) fn cmd_card(sub: CardCmd, selector: Option<&str>) -> Result<()> {
    // Do not even auto-start boardd for a refused non-interactive deletion.
    if let CardCmd::Delete { yes, .. } = &sub {
        confirm_action("card deletion", *yes)?;
    }
    if let CardCmd::Comment {
        sub: crate::args::CommentCmd::Delete { yes, .. },
    } = &sub
    {
        confirm_action("comment deletion", *yes)?;
    }

    let mut c = connect_or_start()?;
    match sub {
        CardCmd::Create {
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
            let board = open_selected_board(&mut c, selector)?;
            let column_id = column
                .as_deref()
                .map(|value| resolve_column_in(&board, value))
                .transpose()?;
            let p = CardCreateParams {
                title,
                board_id: Some(board.board.id),
                description,
                column_id,
                harness,
                model,
                effort: parse_effort(effort)?,
                permission_mode: permission,
                session,
                space_kind: space_kind.as_deref().map(parse_space_kind).transpose()?,
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
        CardCmd::Edit {
            id,
            title,
            description,
            clear_description,
            harness,
            clear_harness,
            model,
            clear_model,
            effort,
            clear_effort,
            permission,
            clear_permission,
            session,
            clear_session,
            space_ref,
            clear_space_ref,
            space_cwd,
            clear_space_cwd,
            json,
        } => {
            if clear_harness {
                bail!("--clear-harness is not supported: harness is required")
            }
            let p = CardUpdateParams {
                id,
                title,
                description: if clear_description {
                    Some(String::new())
                } else {
                    description
                },
                harness,
                model: patch(clear_model, model),
                effort: patch(clear_effort, parse_effort(effort)?),
                permission_mode: patch(clear_permission, permission),
                session: patch(clear_session, session),
                space_kind: None,
                space_ref: patch(clear_space_ref, space_ref),
                space_cwd: patch(clear_space_cwd, space_cwd),
            };
            let card = c.card_update(&p)?;
            if json {
                print_json(&card)?;
            } else {
                println!("Updated card #{}", card.id);
            }
        }
        CardCmd::Delete { id, json, .. } => {
            let result = c.card_delete(id)?;
            if json {
                print_json(&result)?;
            } else {
                println!("Deleted card #{id}");
            }
        }
        CardCmd::Archive { id, json } => card_archive(&mut c, id, true, json)?,
        CardCmd::Restore { id, json } => card_archive(&mut c, id, false, json)?,
        CardCmd::Show { id, json } => {
            let detail = c.card_get(id)?;
            if json {
                print_json(&detail)?;
            } else {
                println!(
                    "#{} {}  [{}{}]",
                    detail.card.id,
                    detail.card.title,
                    detail.card.status,
                    if detail.card.archived_at.is_some() {
                        ", archived"
                    } else {
                        ""
                    }
                );
                if let Some(session) = &detail.card.session {
                    println!("session: {session}");
                }
                if !detail.card.description.is_empty() {
                    println!("\n{}", detail.card.description);
                }
                if !detail.comments.is_empty() {
                    println!("\nComments:");
                    for comment in &detail.comments {
                        println!(
                            "  [{}] {} ({}): {}",
                            comment.id, comment.author, comment.created_at, comment.body
                        );
                    }
                }
                if !detail.runs.is_empty() {
                    println!("\nRuns:");
                    for run in &detail.runs {
                        println!(
                            "  #{} col={} {} started={:?} ended={:?}",
                            run.id,
                            run.column_id,
                            run.outcome
                                .map(|outcome| outcome.to_string())
                                .unwrap_or_else(|| "-".into()),
                            run.started_at,
                            run.ended_at
                        );
                    }
                }
            }
        }
        CardCmd::List {
            column,
            visibility,
            json,
        } => {
            let board = open_selected_board(&mut c, selector)?;
            let column_id = column
                .as_deref()
                .map(|value| resolve_column_in(&board, value))
                .transpose()?;
            let cards = c.card_list_for_board_visible(
                Some(board.board.id),
                column_id,
                parse_visibility(visibility)?,
            )?;
            if json {
                print_json(&cards)?;
            } else {
                for card in &cards {
                    let session = card
                        .session
                        .as_deref()
                        .map(|value| format!("\tsession={value}"))
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
        CardCmd::Move {
            id,
            column,
            destination_board,
            json,
        } => cmd_move(
            &mut c,
            id,
            &column,
            destination_board.as_deref().or(selector),
            json,
        )?,
        CardCmd::Comment { sub } => cmd_card_comment(sub)?,
        CardCmd::Run { sub } => cmd_card_run(sub)?,
    }
    Ok(())
}

fn patch<T>(clear: bool, value: Option<T>) -> Patch<T> {
    if clear {
        Patch::Clear
    } else if let Some(value) = value {
        Patch::Set(value)
    } else {
        Patch::Unchanged
    }
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

pub(crate) fn cmd_move(
    c: &mut UnixClient,
    card_id: i64,
    column: &str,
    destination_selector: Option<&str>,
    json: bool,
) -> Result<()> {
    let card = c.card_get(card_id)?.card;
    let board = match destination_selector {
        Some(selector) => open_selected_board(c, Some(selector))?,
        None => c.board_get_by_id(card.board_id)?,
    };
    let column_id = resolve_column_in(&board, column)?;
    let card = c.card_move(&CardMoveParams {
        id: card_id,
        column_id,
        board_id: Some(board.board.id),
        position: None,
    })?;
    if json {
        print_json(&card)?;
    } else {
        println!(
            "Moved card #{} to column {} [{}]",
            card.id, card.column_id, card.status
        );
    }
    Ok(())
}
