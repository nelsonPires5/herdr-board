use anyhow::{anyhow, bail, Context, Result};
use board_core::client::BoardClient;
use board_core::protocol::{CardMoveParams, RunOutcome};

use crate::daemon::connect_or_start;
use crate::helpers::{env_card_id, print_json};
use crate::scope::resolve_column_in;

pub(crate) fn cmd_comment(first: String, body: Option<String>, json: bool) -> Result<()> {
    let (card_id, body) = match body {
        Some(b) => (first.parse::<i64>().context("card id")?, b),
        None => (env_card_id()?, first),
    };
    let author = std::env::var("BOARD_RUN_ID")
        .ok()
        .map(|r| format!("agent:{r}"));
    let mut c = connect_or_start()?;
    let comment = c.comment_add(card_id, &body, author.as_deref())?;
    if json {
        print_json(&comment)?;
    } else {
        println!("Commented on card #{card_id} (comment #{})", comment.id);
    }
    Ok(())
}

pub(crate) fn cmd_done(
    card_id: Option<i64>,
    outcome: String,
    summary: Option<String>,
    json: bool,
) -> Result<()> {
    let card_id = match card_id {
        Some(id) => id,
        None => env_card_id()?,
    };
    let outcome = RunOutcome::parse_str(&outcome).ok_or_else(|| anyhow!("invalid outcome"))?;
    let run_id = match std::env::var("BOARD_RUN_ID") {
        Ok(value) => Some(
            value
                .parse::<i64>()
                .map_err(|_| anyhow!("invalid $BOARD_RUN_ID '{value}': expected an integer"))?,
        ),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("invalid $BOARD_RUN_ID: value is not valid UTF-8")
        }
    };
    let mut c = connect_or_start()?;
    let res = c.run_done_for_run(card_id, outcome, summary.as_deref(), run_id)?;
    if json {
        print_json(&res)?;
    } else {
        println!(
            "Run #{} closed ({}); card #{} now [{}] in column {}",
            res.run.id, outcome, res.card.id, res.card.status, res.card.column_id
        );
    }
    Ok(())
}

pub(crate) fn cmd_pane_exited(card_id: Option<i64>, run_id: i64) -> Result<()> {
    let card_id = match card_id {
        Some(id) => id,
        None => env_card_id()?,
    };
    connect_or_start()?.run_pane_exited(card_id, run_id)?;
    Ok(())
}

pub(crate) fn cmd_move(card_id: i64, column: String, json: bool) -> Result<()> {
    let mut c = connect_or_start()?;
    let card = c.card_get(card_id)?.card;
    let board = c.board_get_by_id(card.board_id)?;
    let column_id = resolve_column_in(&board, &column)?;
    let card = c.card_move(&CardMoveParams {
        id: card_id,
        column_id,
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

pub(crate) fn cmd_run_action(card_id: i64, json: bool, retry: bool) -> Result<()> {
    let mut c = connect_or_start()?;
    let result = if retry {
        c.run_retry(card_id)?
    } else {
        c.run_cancel(card_id)?
    };
    if json {
        print_json(&result)?;
    } else {
        let action = if retry { "Retried" } else { "Cancelled" };
        println!("{action} card #{card_id}");
    }
    Ok(())
}
