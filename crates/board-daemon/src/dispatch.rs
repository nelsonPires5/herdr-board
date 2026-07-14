//! Run lifecycle: enqueue, promote (spawn), and finalize (done / fail / timeout
//! / lost / cancel), plus the transition + auto-chain logic. All effects the
//! pure engine only *decides* are executed here.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use board_core::capability::{run_pane_name, run_pane_name_unique};
use board_core::db::BOARD_ID;
use board_core::engine::{decide_transition, TransitionDecision};
use board_core::harness::{build_invocation, plan_session, HarnessError, SessionPlan};
use board_core::model::{Card, Run};
use board_core::prompt::{assemble_prompt, effective_settings};
use board_core::protocol::{BoardChangedReason, CardStatus, RunOutcome, SpaceKind};
use board_core::spawn::SpawnReq;
use board_core::{Error, Result};
use board_herdr::{HerdrClient, NotificationSound, WorkspaceCreateParams, WorkspaceInfo};
use uuid::Uuid;

use crate::state::{ActiveRun, Daemon, MAX_AUTO_HOPS};
use crate::store::space_key_str;

fn map_harness_err(e: HarnessError) -> Error {
    match e {
        HarnessError::UnknownHarness(h) => Error::BadRequest(format!("unknown harness: {h}")),
        HarnessError::MissingMintedSession => {
            Error::BadRequest("mint session requested without a uuid".into())
        }
    }
}

/// Create a queued run row for `card` in `column`, minting/resuming/forking the
/// session per policy. Sets the card to `queued`. Does not spawn.
pub fn enqueue_run(d: &Arc<Daemon>, card_id: i64, column_id: i64, is_retry: bool) -> Result<Run> {
    let (card, column, comments, session_used) = {
        let db = d.store.lock();
        let card = db
            .get_card(card_id)?
            .ok_or_else(|| Error::NotFound(format!("card {card_id}")))?;
        let column = db
            .get_column(column_id)?
            .ok_or_else(|| Error::NotFound(format!("column {column_id}")))?;
        let comments = db.list_comments(card_id)?;
        // Resume/fork only sessions PROVEN to exist on the harness side —
        // claude exits with "no conversation found" otherwise. A spawned pane
        // is not proof (claude may crash before creating the session); an
        // `agent:<run_id>` comment is: it can only be posted from inside a
        // live run (the skill mandates comment-before-done).
        let session_used = match &card.session_id {
            Some(sid) => db.list_runs(card_id)?.iter().any(|r| {
                r.started_at.is_some()
                    && r.session_id.as_deref() == Some(sid)
                    && comments
                        .iter()
                        .any(|c| c.author == format!("agent:{}", r.id))
            }),
            None => false,
        };
        (card, column, comments, session_used)
    };

    let settings = effective_settings(&card, &column)?;
    let prompt = assemble_prompt(&card.description, &comments);
    let existing_session = card.session_id.as_deref().filter(|_| session_used);
    let plan = plan_session(existing_session, settings.fresh_session, is_retry);
    let minted = matches!(plan, SessionPlan::Mint).then(|| Uuid::new_v4().to_string());
    let invocation = build_invocation(
        &settings.harness,
        &d.config,
        &settings,
        &plan,
        minted.as_deref(),
        &prompt,
    )
    .map_err(map_harness_err)?;

    let session_for_run = match &plan {
        SessionPlan::Mint => minted.clone(),
        SessionPlan::Resume(id) | SessionPlan::Fork(id) => Some(id.clone()),
    };
    let argv_json = serde_json::to_string(&invocation.argv)?;

    let db = d.store.lock();
    if let Some(u) = &minted {
        db.set_card_session(card_id, u)?;
    }
    let run = db.create_run(
        card_id,
        column_id,
        &settings.harness,
        &argv_json,
        &prompt,
        session_for_run.as_deref(),
        card.session.as_deref(),
    )?;
    db.set_card_status(card_id, CardStatus::Queued)?;
    Ok(run)
}

/// Evaluate the queue and promote as many queued runs as the per-space FIFO and
/// the global concurrency cap allow.
pub async fn dispatch_pass(d: &Arc<Daemon>) {
    let active = match d.store.active_runs() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("dispatch: active_runs failed: {e}");
            return;
        }
    };
    let mut busy: HashSet<String> = active.iter().map(|(_, c)| space_key_str(c)).collect();
    let mut active_count = active.len();
    let max = d.config.max_concurrent.max(1);

    let queued = match d.store.queued_runs() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("dispatch: queued_runs failed: {e}");
            return;
        }
    };

    for (run, card) in queued {
        if active_count >= max {
            break;
        }
        let key = space_key_str(&card);
        if busy.contains(&key) {
            continue;
        }
        match spawn_one(d, &run, &card).await {
            Ok(true) => {
                busy.insert(key);
                active_count += 1;
            }
            Ok(false) => {} // spawn failed; run finished, slot not taken
            Err(e) => tracing::error!("dispatch: spawn_one run {} failed: {e}", run.id),
        }
    }
}

/// Promote one queued run to running. Returns `Ok(true)` if it started,
/// `Ok(false)` if the spawn failed (the run is finished `fail`).
async fn spawn_one(d: &Arc<Daemon>, run: &Run, card: &Card) -> Result<bool> {
    let column = {
        let db = d.store.lock();
        db.get_column(run.column_id)?
            .ok_or_else(|| Error::NotFound(format!("column {}", run.column_id)))?
    };

    // Reconstruct the harness env (BOARD_PROMPT/BOARD_SYSTEM_PROMPT for custom
    // harnesses) plus the daemon-injected BOARD_* vars.
    let mut env: Vec<(String, String)> = Vec::new();
    if run.harness != "claude" {
        env.push(("BOARD_PROMPT".into(), run.prompt_snapshot.clone()));
        if let Some(sp) = &column.system_prompt {
            env.push(("BOARD_SYSTEM_PROMPT".into(), sp.clone()));
        }
    }
    env.push(("BOARD_CARD_ID".into(), card.id.to_string()));
    env.push(("BOARD_RUN_ID".into(), run.id.to_string()));
    env.push((
        "BOARD_SOCKET".into(),
        d.socket_path.to_string_lossy().into_owned(),
    ));

    let argv: Vec<String> = serde_json::from_str(&run.argv_json)?;
    let mut req = SpawnReq {
        // Stable, human-readable pane name `card-<id>-<column-slug>`. herdr
        // agent names are exclusive while a pane using one is open (and finished
        // panes stay open, visible, by design), so on collision the spawner
        // retries once with the run-scoped `name_fallback`.
        name: run_pane_name(card.id, &column.name),
        name_fallback: Some(run_pane_name_unique(card.id, &column.name, run.id)),
        // Both space kinds land in a `kanban` tab (find-or-create + grid layout).
        tab_label: Some("kanban".to_string()),
        cwd: None,
        workspace_ref: None,
        herdr_socket: None,
        env,
        argv,
    };

    // Resolve the card's herdr session to a concrete socket. `None` session →
    // the daemon's default socket. An unknown/stopped session fails the run
    // with a clear error listing the known sessions.
    if let Some(reg) = &d.session_registry {
        match reg.resolve(card.session.as_deref()) {
            Ok(resolved) => {
                // Only stamp a non-default socket on the req (keeps the default
                // path implicit, matching the spawner's fallback).
                if resolved.socket.as_path() != reg.default_socket() {
                    req.herdr_socket = Some(resolved.socket);
                }
            }
            Err(e) => {
                fail_queued_run(d, run, card, &format!("session resolve: {e:#}"))?;
                return Ok(false);
            }
        }
    }

    // Resolve the workspace (existing or freshly created) within the card's
    // session, plus its cwd — agent.start does not inherit the latter (the
    // daemon is not a pane, so herdr's "follow" policy resolves to the daemon's
    // own context). Skipped entirely under the local spawner (no session_registry).
    if d.session_registry.is_some() {
        let socket = req
            .herdr_socket
            .clone()
            .unwrap_or_else(|| d.default_herdr_socket());
        let kind = card.space_kind;
        let space_ref = card.space_ref.clone();
        let space_cwd = card.space_cwd.clone();
        let resolved =
            tokio::task::spawn_blocking(move || -> anyhow::Result<(String, Option<String>)> {
                let mut client = HerdrClient::connect(&socket)
                    .map_err(|e| anyhow::anyhow!("herdr unavailable: {e}"))?;
                resolve_space(
                    &mut client,
                    kind,
                    space_ref.as_deref(),
                    space_cwd.as_deref(),
                )
            })
            .await
            .map_err(|e| Error::BadRequest(format!("workspace resolve join: {e}")))?;
        match resolved {
            Ok((id, cwd)) => {
                req.workspace_ref = Some(id);
                req.cwd = cwd.map(PathBuf::from);
            }
            Err(e) => {
                fail_queued_run(d, run, card, &format!("{e:#}"))?;
                return Ok(false);
            }
        }
    }

    let spawner = d.spawner.clone();
    let req2 = req.clone();
    let spawn_res = tokio::task::spawn_blocking(move || spawner.spawn(&req2)).await;
    let handle = match spawn_res {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => {
            // `:#` prints the whole anyhow chain — the herdr protocol error
            // (e.g. "workspace not found") lives below the top context.
            fail_queued_run(d, run, card, &format!("spawn failed: {e:#}"))?;
            return Ok(false);
        }
        Err(e) => {
            fail_queued_run(d, run, card, &format!("spawn task panicked: {e}"))?;
            return Ok(false);
        }
    };

    let is_local = handle.pid.is_some();
    let pane_id = handle.pane_id.clone();
    let ws_id = handle.workspace_id.clone();
    {
        let db = d.store.lock();
        db.start_run(run.id, ws_id.as_deref(), pane_id.as_deref())?;
        db.set_card_status(card.id, CardStatus::Running)?;
    }

    let deadline = column.timeout_minutes.map(|m| {
        Instant::now() + Duration::from_secs(m.max(0) as u64 * d.settings.timeout_unit_secs)
    });
    {
        let mut sched = d.sched.lock().unwrap();
        sched.active.insert(
            run.id,
            ActiveRun {
                card_id: card.id,
                handle,
                started: Instant::now(),
                timeout_deadline: deadline,
                idle_since: None,
                is_local,
                pane_id,
            },
        );
    }
    d.refresh_watch();
    d.emit_changed(
        BoardChangedReason::RunStarted,
        Some(card.id),
        Some(run.column_id),
    );
    Ok(true)
}

/// Finish a never-started (queued) run as `fail` after a spawn error.
fn fail_queued_run(d: &Arc<Daemon>, run: &Run, card: &Card, reason: &str) -> Result<()> {
    let db = d.store.lock();
    db.finish_run(run.id, RunOutcome::Fail, Some(reason))?;
    db.add_comment(card.id, "system", reason)?;
    db.set_card_status(card.id, CardStatus::Failed)?;
    drop(db);
    d.emit_run_ended(card.id, run.id, RunOutcome::Fail);
    Ok(())
}

/// Finalize an active (started) run.
///
/// - `summary`: stored on the run as `result_summary`.
/// - `extra_comment`: an optional `system` comment posted before the transition
///   (e.g. the pane-exit / timeout reason). Distinct from the transition comment.
/// - `kill`: kill the underlying pane/process first (cancel/timeout).
/// - `transition`: apply the column's `on_success`/`on_fail` transition
///   (per [`decide_transition`]); `false` leaves the card put (pane-exit rule).
///
/// Returns the finished run and the card in its post-finalize state.
pub fn finalize_run(
    d: &Arc<Daemon>,
    run_id: i64,
    outcome: RunOutcome,
    summary: Option<String>,
    extra_comment: Option<String>,
    kill: bool,
    transition: bool,
) -> Result<(Run, Card)> {
    // Remove from the in-memory active set, capturing elapsed + handle.
    let removed = d.sched.lock().unwrap().active.remove(&run_id);
    let elapsed = removed
        .as_ref()
        .map(|a| a.started.elapsed().as_secs() as i64);

    // Idempotency: if another path already finalized this run, do nothing.
    {
        let db = d.store.lock();
        let existing = db.get_run(run_id)?;
        if existing.ended_at.is_some() {
            let card = db
                .get_card(existing.card_id)?
                .ok_or_else(|| Error::NotFound(format!("card {}", existing.card_id)))?;
            return Ok((existing, card));
        }
    }

    if kill {
        if let Some(a) = &removed {
            if let Err(e) = d.spawner.kill(&a.handle) {
                tracing::warn!("kill run {run_id} failed: {e}");
            }
        }
    }
    d.refresh_watch();

    let (finished, card_id, column_id) = {
        let db = d.store.lock();
        let finished = db.finish_run(run_id, outcome, summary.as_deref())?;
        (finished.clone(), finished.card_id, finished.column_id)
    };

    if let Some(c) = &extra_comment {
        d.store.lock().add_comment(card_id, "system", c)?;
    }

    let card = if transition {
        let (current_col, cols) = {
            let db = d.store.lock();
            let col = db
                .get_column(column_id)?
                .ok_or_else(|| Error::NotFound(format!("column {column_id}")))?;
            let cols = db.list_columns(BOARD_ID)?;
            (col, cols)
        };
        let dec = decide_transition(&current_col, &cols, outcome, elapsed);
        d.store
            .lock()
            .add_comment(card_id, "system", &dec.system_comment)?;
        apply_transition(d, card_id, &dec, &cols)?
    } else {
        d.sched.lock().unwrap().chain_hops.remove(&card_id);
        let status = match outcome {
            RunOutcome::Ok => CardStatus::Idle,
            _ => CardStatus::Failed,
        };
        d.store.lock().set_card_status(card_id, status)?
    };

    d.emit_run_ended(card_id, run_id, outcome);
    d.wake_dispatch();
    Ok((finished, card))
}

/// Apply a transition decision: move the card, enqueue the next auto run (with
/// cycle protection), or notify on a manual landing.
fn apply_transition(
    d: &Arc<Daemon>,
    card_id: i64,
    dec: &TransitionDecision,
    cols: &[board_core::model::Column],
) -> Result<Card> {
    let Some(tid) = dec.target_column_id else {
        // Stay put; chain ends.
        d.sched.lock().unwrap().chain_hops.remove(&card_id);
        return d.store.lock().set_card_status(card_id, dec.new_status);
    };

    d.store.lock().set_card_column(card_id, tid)?;
    let target = cols.iter().find(|c| c.id == tid);

    if dec.enqueue {
        let hops = {
            let mut s = d.sched.lock().unwrap();
            let h = s.chain_hops.entry(card_id).or_insert(0);
            *h += 1;
            *h
        };
        if hops > MAX_AUTO_HOPS {
            d.sched.lock().unwrap().chain_hops.remove(&card_id);
            let msg = format!(
                "auto-chain limit ({MAX_AUTO_HOPS}) reached without human action; stopping"
            );
            d.store.lock().add_comment(card_id, "system", &msg)?;
            return d.store.lock().set_card_status(card_id, CardStatus::Failed);
        }
        enqueue_run(d, card_id, tid, false)?;
        d.wake_dispatch();
        d.store
            .lock()
            .get_card(card_id)?
            .ok_or_else(|| Error::NotFound(format!("card {card_id}")))
    } else {
        // Manual (or non-auto) landing: chain ends; notify (auto-transition).
        d.sched.lock().unwrap().chain_hops.remove(&card_id);
        let card = d.store.lock().set_card_status(card_id, dec.new_status)?;
        if target
            .map(|t| t.trigger == board_core::protocol::Trigger::Manual)
            .unwrap_or(false)
        {
            d.notify(
                format!("Card #{card_id} ready for review"),
                target.map(|t| format!("Entered {}", t.name)),
                NotificationSound::Request,
            );
        }
        Ok(card)
    }
}

// ---------------------------------------------------------------------------
// Space resolution (per-session)
// ---------------------------------------------------------------------------

/// Resolve a card's space within its session to `(workspace_id, cwd)`.
///
/// - [`SpaceKind::Workspace`]: `space_ref` is an existing workspace id or a
///   case-insensitive label; cwd comes from the workspace's pane snapshot.
/// - [`SpaceKind::NewWorkspace`]: reuse an open workspace whose label matches
///   `space_ref`, else `workspace.create {label, cwd}`; cwd is `space_cwd`.
fn resolve_space(
    client: &mut HerdrClient,
    kind: SpaceKind,
    space_ref: Option<&str>,
    space_cwd: Option<&str>,
) -> anyhow::Result<(String, Option<String>)> {
    let workspaces = client.workspace_list()?;
    match kind {
        SpaceKind::Workspace => {
            let ws_ref =
                space_ref.ok_or_else(|| anyhow::anyhow!("workspace space requires a space_ref"))?;
            let id = resolve_workspace_ref(&workspaces, ws_ref).map_err(|m| anyhow::anyhow!(m))?;
            let cwd = workspace_cwd(client, &id);
            Ok((id, cwd))
        }
        SpaceKind::NewWorkspace => {
            let label = space_ref.filter(|s| !s.trim().is_empty()).ok_or_else(|| {
                anyhow::anyhow!("new_workspace space requires a label (space_ref)")
            })?;
            let cwd = space_cwd
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("new_workspace space requires space_cwd"))?;
            match find_workspace_by_label(&workspaces, label) {
                // Reuse: prefer the workspace's live cwd, fall back to the card's.
                Some(id) => {
                    let live = workspace_cwd(client, &id);
                    Ok((id, live.or_else(|| Some(cwd.to_string()))))
                }
                None => {
                    let created = client.workspace_create(&WorkspaceCreateParams {
                        label: Some(label.to_string()),
                        cwd: Some(cwd.to_string()),
                        focus: false,
                        ..Default::default()
                    })?;
                    Ok((created.workspace_id().to_string(), Some(cwd.to_string())))
                }
            }
        }
    }
}

/// Look up a workspace's cwd via its first pane in the session snapshot.
fn workspace_cwd(client: &mut HerdrClient, workspace_id: &str) -> Option<String> {
    client.session_snapshot().ok().and_then(|s| {
        s.panes
            .iter()
            .find(|p| p.workspace_id == workspace_id)
            .and_then(|p| p.cwd.clone())
    })
}

/// Resolve a `workspace` space_ref (id, else case-insensitive label) to a
/// workspace id among the open `workspaces`. Err message lists the known ones.
fn resolve_workspace_ref(
    workspaces: &[WorkspaceInfo],
    ws_ref: &str,
) -> std::result::Result<String, String> {
    workspaces
        .iter()
        .find(|w| w.workspace_id == ws_ref)
        .or_else(|| {
            workspaces
                .iter()
                .find(|w| w.label.eq_ignore_ascii_case(ws_ref))
        })
        .map(|w| w.workspace_id.clone())
        .ok_or_else(|| {
            let known: Vec<String> = workspaces
                .iter()
                .map(|w| format!("{} ({})", w.workspace_id, w.label))
                .collect();
            format!(
                "herdr workspace '{ws_ref}' not found by id or label; known: {}",
                known.join(", ")
            )
        })
}

/// Find an open workspace whose label case-insensitively matches `label`.
fn find_workspace_by_label(workspaces: &[WorkspaceInfo], label: &str) -> Option<String> {
    workspaces
        .iter()
        .find(|w| w.label.eq_ignore_ascii_case(label))
        .map(|w| w.workspace_id.clone())
}

#[cfg(test)]
mod tests {
    use super::{find_workspace_by_label, resolve_workspace_ref};
    use board_herdr::{AgentStatus, WorkspaceInfo};

    fn ws(id: &str, label: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            workspace_id: id.to_string(),
            label: label.to_string(),
            number: 0,
            focused: false,
            active_tab_id: String::new(),
            agent_status: AgentStatus::Unknown,
        }
    }

    #[test]
    fn resolve_ref_by_id_then_label() {
        let all = [ws("w1", "Alpha"), ws("w2", "Beta")];
        assert_eq!(resolve_workspace_ref(&all, "w2").unwrap(), "w2");
        // Case-insensitive label match.
        assert_eq!(resolve_workspace_ref(&all, "alpha").unwrap(), "w1");
    }

    #[test]
    fn resolve_ref_unknown_lists_known() {
        let all = [ws("w1", "Alpha")];
        let err = resolve_workspace_ref(&all, "ghost").unwrap_err();
        assert!(err.contains("ghost"));
        assert!(err.contains("w1"));
    }

    #[test]
    fn new_workspace_reuse_matches_label_case_insensitively() {
        let all = [ws("w1", "Alpha"), ws("w2", "MyFeature")];
        // Reuse: label already open → return its id (no create).
        assert_eq!(
            find_workspace_by_label(&all, "myfeature").as_deref(),
            Some("w2")
        );
    }

    #[test]
    fn new_workspace_create_when_absent() {
        let all = [ws("w1", "Alpha")];
        // Absent → None → dispatch will call workspace.create.
        assert!(find_workspace_by_label(&all, "brand-new").is_none());
    }
}
