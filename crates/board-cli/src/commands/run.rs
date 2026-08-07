use anyhow::{Context, Result};
use board_core::client::BoardClient;
use board_core::protocol::RunFocusAction;

use crate::args::{CommentCmd, RunCmd};
use crate::context::Ctx;
use crate::helpers::{
    actor_author, actor_pane_id, actor_run_id, env_card_id, origin_socket as resolve_origin_socket,
    parse_outcome,
};
use crate::render::{emit, emit_line};

/// The legacy top-level `board comment [CARD_ID] BODY`. It only resolves the
/// two accepted shapes and then re-enters the canonical nested handler; there is
/// exactly one comment-add code path.
pub(crate) fn cmd_comment(first: String, body: Option<String>, ctx: &mut Ctx) -> Result<()> {
    let (card_id, body) = match body {
        Some(body) => (first.parse::<i64>().context("card id")?, body),
        None => (env_card_id()?, first),
    };
    cmd_card_comment(CommentCmd::Add { card_id, body }, ctx)
}

pub(crate) fn cmd_card_comment(sub: CommentCmd, ctx: &mut Ctx) -> Result<()> {
    let json = ctx.json();
    let actor = actor_run_id()?;
    let pane_id = actor_pane_id();
    let result: Result<()> = (|| match sub {
        CommentCmd::Add { card_id, body } => {
            let author = actor_author(actor);
            let comment = ctx.client()?.comment_add_for_run(
                card_id,
                &body,
                author.as_deref(),
                actor,
                pane_id.as_deref(),
            )?;
            emit_line(
                &comment,
                json,
                format!("Commented on card #{card_id} (comment #{})", comment.id),
            )
        }
        CommentCmd::Show { comment_id } => {
            let comment = ctx.client()?.comment_get(comment_id)?;
            emit(&comment, json)
        }
        CommentCmd::Edit { comment_id, body } => {
            let comment = ctx.client()?.comment_update(comment_id, &body, actor)?;
            emit_line(&comment, json, format!("Updated comment #{}", comment.id))
        }
        CommentCmd::Delete { comment_id, .. } => {
            let result = ctx.client()?.comment_delete(comment_id, actor)?;
            emit_line(&result, json, format!("Deleted comment #{comment_id}"))
        }
        CommentCmd::History { comment_id } => {
            let history = ctx.client()?.comment_history(comment_id)?;
            emit(&history, json)
        }
    })();
    result.context("comment operation")
}

pub(crate) fn cmd_done(
    card_id: Option<i64>,
    outcome: String,
    summary: Option<String>,
    ctx: &mut Ctx,
) -> Result<()> {
    let json = ctx.json();
    let card_id = match card_id {
        Some(card_id) => card_id,
        None => env_card_id()?,
    };
    let outcome = parse_outcome(&outcome)?;
    let run_id = actor_run_id()?;
    let pane_id = actor_pane_id();
    let result = ctx.client()?.run_done_for_run(
        card_id,
        outcome,
        summary.as_deref(),
        run_id,
        pane_id.as_deref(),
    )?;
    emit_line(
        &result,
        json,
        format!(
            "Run #{} closed ({}); card #{} now [{}] in column {}",
            result.run.id, outcome, result.card.id, result.card.status, result.card.column_id
        ),
    )
}

pub(crate) fn cmd_card_run(sub: RunCmd, ctx: &mut Ctx) -> Result<()> {
    match sub {
        RunCmd::Done {
            card_id,
            outcome,
            summary,
        } => cmd_done(card_id, outcome, summary, ctx),
        RunCmd::Confirm { card_id, summary } => cmd_done(card_id, "ok".into(), summary, ctx),
        RunCmd::Cancel { card_id } => cmd_run_action(card_id, ctx, false),
        RunCmd::Retry { card_id } => cmd_run_action(card_id, ctx, true),
        RunCmd::Focus {
            card_id,
            run_id,
            origin_socket,
        } => {
            let json = ctx.json();
            let socket = resolve_origin_socket(origin_socket)?;
            let result = ctx.client()?.run_focus(card_id, run_id, &socket)?;
            let text = match result.action {
                RunFocusAction::FocusedRecordedPane => format!(
                    "Focused run #{} of card #{} ({}) pane {}",
                    result.run_id, result.card_id, result.harness, result.pane_id
                ),
                RunFocusAction::FocusedRescuedPane => format!(
                    "Focused the already-reopened pane {} of run #{} of card #{} ({})",
                    result.pane_id, result.run_id, result.card_id, result.harness
                ),
                // Say plainly that this pane is not a run: it has no runs
                // row, so the daemon does not watch or time it out.
                RunFocusAction::Rescued => format!(
                    "Reopened run #{} of card #{}: resumed the {} conversation in new pane {} \
                     (the previous pane {} is gone; this pane is ephemeral and not tracked \
                     as a run)",
                    result.run_id,
                    result.card_id,
                    result.harness,
                    result.pane_id,
                    result
                        .recorded_pane_id
                        .as_deref()
                        .unwrap_or("(none recorded)")
                ),
            };
            emit_line(&result, json, text)
        }
    }
}

pub(crate) fn cmd_pane_exited(card_id: Option<i64>, run_id: i64, ctx: &mut Ctx) -> Result<()> {
    let card_id = match card_id {
        Some(card_id) => card_id,
        None => env_card_id()?,
    };
    ctx.client()?.run_pane_exited(card_id, run_id)?;
    Ok(())
}

pub(crate) fn cmd_run_action(card_id: i64, ctx: &mut Ctx, retry: bool) -> Result<()> {
    let json = ctx.json();
    let result = if retry {
        ctx.client()?.run_retry(card_id)?
    } else {
        ctx.client()?.run_cancel(card_id)?
    };
    let action = if retry { "Retried" } else { "Cancelled" };
    emit_line(&result, json, format!("{action} card #{card_id}"))
}
