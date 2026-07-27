use anyhow::Result;
use board_core::client::BoardClient;
use board_core::protocol::{ColumnCreateParams, ColumnUpdateParams, Patch};

use crate::args::ColumnCmd;
use crate::context::Ctx;
use crate::helpers::{confirm_action, parse_effort, parse_trigger};
use crate::render::{emit, emit_line};

pub(crate) fn cmd_column(sub: ColumnCmd, ctx: &mut Ctx) -> Result<()> {
    if let ColumnCmd::Delete { confirm, .. } = &sub {
        confirm_action("column deletion", confirm.yes)?;
    }

    let json = ctx.json();
    match sub {
        ColumnCmd::List => {
            let columns = ctx.board()?.columns.clone();
            emit(&columns, json)
        }
        ColumnCmd::Create {
            name,
            prompt,
            trigger,
            on_success,
            on_fail,
            fresh_session,
            reuse_session,
            harness,
            model,
            effort,
            permission,
            timeout,
            position,
        } => {
            // `--fresh-session` / `--reuse-session` exclusion is enforced by clap.
            let params = ColumnCreateParams {
                name,
                board_id: Some(ctx.board_id()?),
                position,
                system_prompt: prompt,
                trigger: parse_trigger(trigger)?,
                on_success_column_id: ctx.optional_column_id(on_success.as_deref())?,
                on_fail_column_id: ctx.optional_column_id(on_fail.as_deref())?,
                fresh_session: bool_option(fresh_session, reuse_session),
                harness_override: harness,
                model_override: model,
                effort_override: parse_effort(effort)?.map(|value| value.to_string()),
                permission_override: permission,
                timeout_minutes: timeout,
            };
            let column = ctx.client()?.column_create(&params)?;
            emit_line(
                &column,
                json,
                format!("Created column #{} {}", column.id, column.name),
            )
        }
        ColumnCmd::Show { column } => {
            let column = ctx.column(&column)?;
            emit(&column, json)
        }
        ColumnCmd::Edit {
            column,
            name,
            prompt,
            clear_prompt,
            trigger,
            on_success,
            clear_on_success,
            on_fail,
            clear_on_fail,
            fresh_session,
            reuse_session,
            harness,
            clear_harness,
            model,
            clear_model,
            effort,
            clear_effort,
            permission,
            clear_permission,
            timeout,
            clear_timeout,
        } => {
            let params = ColumnUpdateParams {
                id: ctx.column_id(&column)?,
                name,
                position: None,
                system_prompt: Patch::from_flags(clear_prompt, prompt),
                trigger: parse_trigger(trigger)?,
                on_success_column_id: Patch::from_flags(
                    clear_on_success,
                    ctx.optional_column_id(on_success.as_deref())?,
                ),
                on_fail_column_id: Patch::from_flags(
                    clear_on_fail,
                    ctx.optional_column_id(on_fail.as_deref())?,
                ),
                fresh_session: bool_option(fresh_session, reuse_session),
                harness_override: Patch::from_flags(clear_harness, harness),
                model_override: Patch::from_flags(clear_model, model),
                effort_override: Patch::from_flags(
                    clear_effort,
                    parse_effort(effort)?.map(|value| value.to_string()),
                ),
                permission_override: Patch::from_flags(clear_permission, permission),
                timeout_minutes: Patch::from_flags(clear_timeout, timeout),
            };
            let column = ctx.client()?.column_update(&params)?;
            emit_line(
                &column,
                json,
                format!("Updated column #{} {}", column.id, column.name),
            )
        }
        ColumnCmd::Reorder { column, position } => {
            let id = ctx.column_id(&column)?;
            let columns = ctx.client()?.column_reorder(id, position)?;
            emit(&columns, json)
        }
        ColumnCmd::Delete {
            column,
            move_cards_to,
            ..
        } => {
            let id = ctx.column_id(&column)?;
            let destination = ctx.optional_column_id(move_cards_to.as_deref())?;
            let result = ctx.client()?.column_delete(id, destination)?;
            emit_line(&result, json, format!("Deleted column #{id}"))
        }
    }
}

fn bool_option(fresh: bool, reuse: bool) -> Option<bool> {
    if fresh {
        Some(true)
    } else if reuse {
        Some(false)
    } else {
        None
    }
}
