use std::sync::Arc;

use board_core::db::Db;
use board_core::engine::{decide_resumability, validate_effective_settings, ResumabilityDecision};
use board_core::harness::{build_invocation, plan_session, SessionPlan};
use board_core::launch::{ExecutionSpec, RunLaunchSpec};
use board_core::model::{Card, Run};
use board_core::prompt::{assemble_prompt, effective_settings};
use board_core::{Error, Result};
use uuid::Uuid;

use crate::dispatch::{map_harness_err, PreparedEnqueue};
use crate::state::Daemon;

/// Create a queued run row for `card` in `column`, minting/resuming/forking the
/// session per policy. Sets the card to `queued`. Does not spawn.
pub(crate) fn enqueue_run(
    d: &Arc<Daemon>,
    card_id: i64,
    column_id: i64,
    is_retry: bool,
) -> Result<Run> {
    enqueue_run_inner(d, card_id, column_id, is_retry)
}

fn enqueue_run_inner(d: &Arc<Daemon>, card_id: i64, column_id: i64, is_retry: bool) -> Result<Run> {
    // Scheduler state and every enqueue input share one critical section.
    // In particular, do not prepare an invocation from a card snapshot before
    // this lock: a concurrent edit could otherwise update `card.session` (or
    // its settings/prompt) before this run persists the stale value.
    let _sched = d.sched.lock().unwrap();
    let db = d.store.lock();
    let card = db
        .get_card(card_id)?
        .ok_or_else(|| Error::NotFound(format!("card {card_id}")))?;
    if card.archived_at.is_some() {
        return Err(Error::InvalidState(
            "archived card must be restored before starting a run".into(),
        ));
    }
    if db.open_run_for_card(card_id)?.is_some() {
        return Err(Error::InvalidState(
            "card has an open run; complete or cancel it before starting another".into(),
        ));
    }
    if card.column_id != column_id {
        return Err(Error::InvalidState(
            "card moved to another column while its run was being prepared".into(),
        ));
    }

    let prepared = prepare_enqueue_values(d, &db, &card, column_id, is_retry)?;
    let run = db.enqueue_run_uow(&prepared.borrowed())?;
    Ok(run)
}

pub(crate) fn prepare_enqueue_values(
    d: &Daemon,
    db: &Db,
    card: &Card,
    column_id: i64,
    is_retry: bool,
) -> Result<PreparedEnqueue> {
    let column = db
        .get_column(column_id)?
        .ok_or_else(|| Error::NotFound(format!("column {column_id}")))?;
    let comments = db.list_comments(card.id)?;
    let session_used = matches!(
        decide_resumability(
            card.session_id.as_deref(),
            &db.list_runs(card.id)?,
            &comments
        ),
        ResumabilityDecision::Resumable
    );
    validate_effective_settings(card, &column, &d.config)?;
    let settings = effective_settings(card, &column)?;
    let prompt = assemble_prompt(&card.description, &comments);
    let existing_session = card.session_id.as_deref().filter(|_| session_used);
    let plan = plan_session(existing_session, settings.fresh_session, is_retry);
    let target_session = matches!(plan, SessionPlan::Mint | SessionPlan::Fork(_))
        .then(|| Uuid::new_v4().to_string());
    let invocation = build_invocation(
        &settings.harness,
        &d.config,
        &settings,
        &plan,
        target_session.as_deref(),
        &prompt,
    )
    .map_err(map_harness_err)?;
    let session_id = invocation
        .resulting_session_id
        .clone()
        .or_else(|| match &plan {
            SessionPlan::Mint => target_session.clone(),
            SessionPlan::Resume(id) | SessionPlan::Fork(id) => Some(id.clone()),
        });
    Ok(PreparedEnqueue {
        card_id: card.id,
        column_id,
        harness: settings.harness.clone(),
        argv_json: serde_json::to_string(&invocation.argv)?,
        prompt,
        system_prompt: invocation.system_prompt.clone().unwrap_or_else(|| {
            board_core::harness::protocol_system_prompt(settings.system_prompt.as_deref())
        }),
        launch_spec_json: serde_json::to_string(&RunLaunchSpec::v1(ExecutionSpec {
            argv: invocation.argv.clone(),
            env: invocation.env.clone(),
            agent_kind: invocation.agent_kind.clone(),
            initial_prompt: invocation.initial_prompt.clone(),
            system_prompt: invocation.system_prompt.clone(),
        }))?,
        session_id,
        session: card.session.clone(),
    })
}
