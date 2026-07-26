use anyhow::{anyhow, bail, Result};
use board_core::client::BoardClient;
use board_core::protocol::{ColumnCreateParams, ColumnUpdateParams, Patch};

use crate::args::ColumnCmd;
use crate::daemon::connect_or_start;
use crate::helpers::{confirm_action, parse_effort, parse_trigger, print_json};
use crate::scope::{open_selected_board, resolve_column_in};

pub(crate) fn cmd_column(sub: ColumnCmd, selector: Option<&str>) -> Result<()> {
    if let ColumnCmd::Delete { yes, .. } = &sub {
        confirm_action("column deletion", *yes)?;
    }

    let mut c = connect_or_start()?;
    match sub {
        ColumnCmd::List { json } => {
            let board = open_selected_board(&mut c, selector)?;
            if json {
                print_json(&board.columns)?;
            } else {
                for column in &board.columns {
                    print_column(column);
                }
            }
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
            json,
        } => {
            if fresh_session && reuse_session {
                bail!("--fresh-session and --reuse-session are mutually exclusive")
            }
            let board = open_selected_board(&mut c, selector)?;
            let params = ColumnCreateParams {
                name,
                board_id: Some(board.board.id),
                position,
                system_prompt: prompt,
                trigger: parse_trigger(trigger)?,
                on_success_column_id: resolve_optional_column(&board, on_success)?,
                on_fail_column_id: resolve_optional_column(&board, on_fail)?,
                fresh_session: bool_option(fresh_session, reuse_session),
                harness_override: harness,
                model_override: model,
                effort_override: parse_effort(effort)?.map(|value| value.to_string()),
                permission_override: permission,
                timeout_minutes: timeout,
            };
            let column = c.column_create(&params)?;
            if json {
                print_json(&column)?;
            } else {
                println!("Created column #{} {}", column.id, column.name);
            }
        }
        ColumnCmd::Show { column, json } => {
            let board = open_selected_board(&mut c, selector)?;
            let id = resolve_column_in(&board, &column)?;
            let column = board
                .columns
                .iter()
                .find(|candidate| candidate.id == id)
                .ok_or_else(|| anyhow!("column {id} not found"))?;
            if json {
                print_json(column)?;
            } else {
                print_column(column);
            }
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
            json,
        } => {
            if fresh_session && reuse_session {
                bail!("--fresh-session and --reuse-session are mutually exclusive")
            }
            let board = open_selected_board(&mut c, selector)?;
            let id = resolve_column_reference(&board, &column)?;
            let params = ColumnUpdateParams {
                id,
                name,
                position: None,
                system_prompt: patch(clear_prompt, prompt),
                trigger: parse_trigger(trigger)?,
                on_success_column_id: patch(
                    clear_on_success,
                    resolve_optional_column(&board, on_success)?,
                ),
                on_fail_column_id: patch(clear_on_fail, resolve_optional_column(&board, on_fail)?),
                fresh_session: bool_option(fresh_session, reuse_session),
                harness_override: patch(clear_harness, harness),
                model_override: patch(clear_model, model),
                effort_override: patch(
                    clear_effort,
                    parse_effort(effort)?.map(|value| value.to_string()),
                ),
                permission_override: patch(clear_permission, permission),
                timeout_minutes: patch(clear_timeout, timeout),
            };
            let column = c.column_update(&params)?;
            if json {
                print_json(&column)?;
            } else {
                println!("Updated column #{} {}", column.id, column.name);
            }
        }
        ColumnCmd::Reorder {
            column,
            position,
            json,
        } => {
            let board = open_selected_board(&mut c, selector)?;
            let id = resolve_column_reference(&board, &column)?;
            let columns = c.column_reorder(id, position)?;
            if json {
                print_json(&columns)?;
            } else {
                for column in columns {
                    print_column(&column);
                }
            }
        }
        ColumnCmd::Delete {
            column,
            move_cards_to,
            json,
            ..
        } => {
            let board = open_selected_board(&mut c, selector)?;
            let id = resolve_column_reference(&board, &column)?;
            let destination = resolve_optional_column(&board, move_cards_to)?;
            let result = c.column_delete(id, destination)?;
            if json {
                print_json(&result)?;
            } else {
                println!("Deleted column #{id}");
            }
        }
    }
    Ok(())
}

fn resolve_optional_column(
    board: &board_core::protocol::BoardSnapshot,
    value: Option<String>,
) -> Result<Option<i64>> {
    value
        .as_deref()
        .map(|value| resolve_column_reference(board, value))
        .transpose()
}

fn resolve_column_reference(
    board: &board_core::protocol::BoardSnapshot,
    value: &str,
) -> Result<i64> {
    resolve_column_in(board, value)
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

fn patch<T>(clear: bool, value: Option<T>) -> Patch<T> {
    if clear {
        Patch::Clear
    } else if let Some(value) = value {
        Patch::Set(value)
    } else {
        Patch::Unchanged
    }
}

fn print_column(column: &board_core::model::Column) {
    println!(
        "#{}\tpos={}\t[{}]\t{}",
        column.id, column.position, column.trigger, column.name
    );
}
