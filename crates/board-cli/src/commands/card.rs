use anyhow::{bail, Result};
use board_core::client::BoardClient;
use board_core::protocol::{
    BoardSnapshot, CardCreateParams, CardMoveParams, CardUpdateParams, Patch,
};

use crate::args::CardCmd;
use crate::commands::run::{cmd_card_comment, cmd_card_run};
use crate::context::Ctx;
use crate::helpers::{confirm_action, parse_effort, parse_space_kind, parse_visibility};
use crate::render::{emit, emit_line};
use crate::scope::{open_selected_board, resolve_column_in};

pub(crate) fn cmd_card(sub: CardCmd, ctx: &mut Ctx) -> Result<()> {
    // Do not even auto-start boardd for a refused non-interactive deletion.
    if let CardCmd::Delete { confirm, .. } = &sub {
        confirm_action("card deletion", confirm.yes)?;
    }
    if let CardCmd::Comment {
        sub: crate::args::CommentCmd::Delete { confirm, .. },
    } = &sub
    {
        confirm_action("comment deletion", confirm.yes)?;
    }

    let json = ctx.json();
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
        } => {
            let column_id = ctx.optional_column_id(column.as_deref())?;
            let p = CardCreateParams {
                title,
                board_id: Some(ctx.board_id()?),
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
            let card = ctx.client()?.card_create(&p)?;
            emit_line(
                &card,
                json,
                format!(
                    "Created card #{} \"{}\" in column {}",
                    card.id, card.title, card.column_id
                ),
            )
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
                model: Patch::from_flags(clear_model, model),
                effort: Patch::from_flags(clear_effort, parse_effort(effort)?),
                permission_mode: Patch::from_flags(clear_permission, permission),
                session: Patch::from_flags(clear_session, session),
                space_kind: None,
                space_ref: Patch::from_flags(clear_space_ref, space_ref),
                space_cwd: Patch::from_flags(clear_space_cwd, space_cwd),
            };
            let card = ctx.client()?.card_update(&p)?;
            emit_line(&card, json, format!("Updated card #{}", card.id))
        }
        CardCmd::Delete { id, .. } => {
            let result = ctx.client()?.card_delete(id)?;
            emit_line(&result, json, format!("Deleted card #{id}"))
        }
        CardCmd::Duplicate { id } => {
            let card = ctx.client()?.card_duplicate(id)?;
            emit_line(
                &card,
                json,
                format!("Duplicated card #{id} as #{} \"{}\"", card.id, card.title),
            )
        }
        CardCmd::Archive { id } => card_archive(ctx, id, true),
        CardCmd::Restore { id } => card_archive(ctx, id, false),
        CardCmd::Show { id } => {
            let detail = ctx.client()?.card_get(id)?;
            emit(&detail, json)
        }
        CardCmd::List { column, visibility } => {
            let column_id = ctx.optional_column_id(column.as_deref())?;
            let board_id = ctx.board_id()?;
            let visibility = parse_visibility(visibility)?;
            let cards =
                ctx.client()?
                    .card_list_for_board_visible(Some(board_id), column_id, visibility)?;
            emit(&cards, json)
        }
        CardCmd::Move {
            id,
            column,
            position,
            destination_board,
        } => cmd_move(ctx, id, &column, destination_board.as_deref(), position),
        CardCmd::Comment { sub } => cmd_card_comment(sub, ctx),
        CardCmd::Run { sub } => cmd_card_run(sub, ctx),
    }
}

fn card_archive(ctx: &mut Ctx, id: i64, archived: bool) -> Result<()> {
    let json = ctx.json();
    let card = ctx.client()?.card_archive(id, archived)?;
    let action = if archived { "Archived" } else { "Restored" };
    emit_line(&card, json, format!("{action} card #{}", card.id))
}

/// Move a card, resolving the destination column against the board the card is
/// landing on.
///
/// Destination precedence, unchanged: an explicit `--destination-board`, else
/// the global `--board` selector (deprecated — it warns), else the card's own
/// board. A named destination costs two RPCs (`board.open`/`board.get` plus
/// `card.move`); the "stay on the card's board" case still needs three, because
/// protocol v1 has no way to learn a card's board without `card.get` and no way
/// to move a card by column *name*. A `card.move` that accepted a column name
/// would collapse every case to one round-trip.
pub(crate) fn cmd_move(
    ctx: &mut Ctx,
    card_id: i64,
    column: &str,
    explicit_destination: Option<&str>,
    position: Option<i64>,
) -> Result<()> {
    let json = ctx.json();
    if position.is_some_and(|p| p < 0) {
        bail!("--position must be zero-based (0 = first card)")
    }
    let destination = match explicit_destination {
        Some(destination) => Some(destination),
        None => match ctx.selector() {
            Some(selector) => {
                eprintln!(
                    "board: warning: `move` is using the global --board {selector} as the move \
                     destination; this fallback is deprecated, pass --destination-board \
                     {selector} to cross boards"
                );
                Some(selector)
            }
            None => None,
        },
    };

    let board = destination_board(ctx, card_id, destination)?;
    let column_id = resolve_column_in(&board, column)?;
    let card = ctx.client()?.card_move(&CardMoveParams {
        id: card_id,
        column_id,
        board_id: Some(board.board.id),
        position,
    })?;
    emit_line(
        &card,
        json,
        format!(
            "Moved card #{} to column {} [{}]",
            card.id, card.column_id, card.status
        ),
    )
}

fn destination_board(
    ctx: &mut Ctx,
    card_id: i64,
    destination: Option<&str>,
) -> Result<BoardSnapshot> {
    match destination {
        Some(selector) => open_selected_board(ctx.client()?, Some(selector)),
        None => {
            let board_id = ctx.client()?.card_get(card_id)?.card.board_id;
            ctx.client()?.board_get_by_id(board_id)
        }
    }
}
