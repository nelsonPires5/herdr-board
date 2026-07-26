use anyhow::{Context, Result};
use board_core::client::BoardClient;
use board_core::protocol::RunOutcome;

use crate::args::{CommentCmd, RunCmd};
use crate::daemon::connect_or_start;
use crate::helpers::{
    actor_author, actor_run_id, env_card_id, origin_socket as resolve_origin_socket, parse_outcome,
    print_json,
};

/// Legacy top-level comment command.
pub(crate) fn cmd_comment(first: String, body: Option<String>, json: bool) -> Result<()> {
    let (card_id, body) = match body {
        Some(body) => (first.parse::<i64>().context("card id")?, body),
        None => (env_card_id()?, first),
    };
    let mut c = connect_or_start()?;
    let run_id = actor_run_id()?;
    let author = actor_author(run_id);
    let comment = c.comment_add_for_run(card_id, &body, author.as_deref(), run_id)?;
    if json {
        print_json(&comment)?;
    } else {
        println!("Commented on card #{card_id} (comment #{})", comment.id);
    }
    Ok(())
}

pub(crate) fn cmd_card_comment(sub: CommentCmd) -> Result<()> {
    let mut c = connect_or_start()?;
    let actor = actor_run_id()?;
    let result: Result<()> = (|| {
        match sub {
            CommentCmd::Add {
                card_id,
                body,
                json,
            } => {
                let author = actor_author(actor);
                let comment = c.comment_add_for_run(card_id, &body, author.as_deref(), actor)?;
                if json {
                    print_json(&comment)?;
                } else {
                    println!("Commented on card #{card_id} (comment #{})", comment.id);
                }
            }
            CommentCmd::Show { comment_id, json } => {
                let comment = c.comment_get(comment_id)?;
                if json {
                    print_json(&comment)?;
                } else {
                    print_comment(&comment);
                }
            }
            CommentCmd::Edit {
                comment_id,
                body,
                json,
            } => {
                let comment = c.comment_update(comment_id, &body, actor)?;
                if json {
                    print_json(&comment)?;
                } else {
                    println!("Updated comment #{}", comment.id);
                }
            }
            CommentCmd::Delete {
                comment_id, json, ..
            } => {
                let result = c.comment_delete(comment_id, actor)?;
                if json {
                    print_json(&result)?;
                } else {
                    println!("Deleted comment #{comment_id}");
                }
            }
            CommentCmd::History { comment_id, json } => {
                let history = c.comment_history(comment_id)?;
                if json {
                    print_json(&history)?;
                } else {
                    for entry in history {
                        println!(
                            "#{} comment=#{} {} ({}): {}{}",
                            entry.id,
                            entry.comment_id,
                            entry.author,
                            entry.created_at,
                            entry.body,
                            if entry.deleted_at.is_some() {
                                " [deleted]"
                            } else {
                                ""
                            }
                        );
                    }
                }
            }
        }
        Ok(())
    })();
    result.context("comment operation")
}

fn print_comment(comment: &board_core::model::CommentRecord) {
    println!(
        "#{} card={} {} ({}): {}{}",
        comment.id,
        comment.card_id,
        comment.author,
        comment.created_at,
        comment.body,
        if comment.deleted_at.is_some() {
            " [deleted]"
        } else {
            ""
        }
    );
}

pub(crate) fn cmd_done(
    card_id: Option<i64>,
    outcome: String,
    summary: Option<String>,
    json: bool,
) -> Result<()> {
    let card_id = match card_id {
        Some(card_id) => card_id,
        None => env_card_id()?,
    };
    let outcome = parse_outcome(&outcome)?;
    let run_id = actor_run_id()?;
    let mut c = connect_or_start()?;
    let result = c.run_done_for_run(card_id, outcome, summary.as_deref(), run_id)?;
    print_run_result(&result, outcome, json)
}

pub(crate) fn cmd_card_run(sub: RunCmd) -> Result<()> {
    match sub {
        RunCmd::Done {
            card_id,
            outcome,
            summary,
            json,
        } => cmd_done(card_id, outcome, summary, json),
        RunCmd::Confirm {
            card_id,
            summary,
            json,
        } => cmd_done(card_id, "ok".into(), summary, json),
        RunCmd::Cancel { card_id, json } => cmd_run_action(card_id, json, false),
        RunCmd::Retry { card_id, json } => cmd_run_action(card_id, json, true),
        RunCmd::Focus {
            card_id,
            origin_socket,
            json,
        } => {
            let socket = resolve_origin_socket(origin_socket)?;
            let result = connect_or_start()?.run_focus(card_id, &socket)?;
            if json {
                print_json(&result)?;
            } else {
                println!("Focused run #{} pane {}", result.run_id, result.pane_id);
            }
            Ok(())
        }
    }
}

pub(crate) fn cmd_pane_exited(card_id: Option<i64>, run_id: i64) -> Result<()> {
    let card_id = match card_id {
        Some(card_id) => card_id,
        None => env_card_id()?,
    };
    connect_or_start()?.run_pane_exited(card_id, run_id)?;
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

fn print_run_result(
    result: &board_core::protocol::RunActionResult,
    outcome: RunOutcome,
    json: bool,
) -> Result<()> {
    if json {
        print_json(result)?;
    } else {
        println!(
            "Run #{} closed ({}); card #{} now [{}] in column {}",
            result.run.id, outcome, result.card.id, result.card.status, result.card.column_id
        );
    }
    Ok(())
}
