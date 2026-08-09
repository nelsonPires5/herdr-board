//! Dispatch-side pane-reuse decision: which queued hop may re-prompt the
//! prior run's still-live pane.
//!
//! The spawner tests cover what a *reuse request* does to herdr; this module
//! covers when dispatch is allowed to make one. A Resume hop keeps the same
//! harness conversation and reuses the exact prior pane. A Fork (retry) hop
//! must never re-prompt it — the queued `fork <source-id>` argv needs a fresh
//! pane, and re-prompting the live pane would keep the old conversation so
//! the fork never executes. A Mint hop mints a new id and finds no match.

use super::*;
use board_core::launch::{ExecutionSpec, RunLaunchSpec};
use serde_json::json;

const SOURCE_SESSION: &str = "thread-7";
const PRIOR_PANE: &str = "w1:p-prior";

/// Serve exactly the herdr calls `spawn_one` makes for a durable v11 launch:
/// the protocol gate, workspace discovery, and the two live snapshots (cwd
/// proof, then card-tab reconstruction from the prior durable pane).
fn reuse_decision_server() -> FakeHerdr {
    let snapshot = json!({
        "panes": [{
            "pane_id": PRIOR_PANE, "terminal_id": "term-1",
            "workspace_id": "w1", "tab_id": "w1:t1",
            "cwd": "/repo", "focused": false, "revision": 1
        }]
    });
    testkit::herdr_server()
        .take(5)
        .on("workspace.list", |req| {
            testkit::reply(
                req,
                json!({"workspaces": [{
                    "workspace_id": "w1", "label": "Feature", "number": 1,
                    "focused": false, "active_tab_id": "", "agent_status": "idle"
                }]}),
            )
        })
        .on("session.snapshot", move |req| {
            testkit::reply(req, json!({"snapshot": snapshot}))
        })
        .serve()
}

/// A codex card whose prior durable run ended in `w1:p-prior` — the pane
/// stays open in herdr (finished panes remain visible by design). When
/// `source` is set, the prior run captured that conversation id and the card
/// keeps it, so the next hop carries the same `session_id` whether it
/// resumes or forks.
fn codex_card_with_ended_prior_run(d: &Arc<Daemon>, source: Option<&str>) -> (i64, i64) {
    let db = d.store.lock();
    let column = db
        .create_column(&ColumnCreateParams {
            name: "Codex".into(),
            trigger: Some(Trigger::Auto),
            ..Default::default()
        })
        .unwrap();
    let card = db
        .create_card(&CardCreateParams {
            column_id: Some(column.id),
            title: "codex retry".into(),
            harness: Some("codex".into()),
            description: Some("build the widget".into()),
            space_kind: Some(SpaceKind::NewWorkspace),
            space_ref: Some("Feature".into()),
            space_cwd: Some("/repo".into()),
            ..Default::default()
        })
        .unwrap();
    if let Some(source) = source {
        db.set_card_session(card.id, source).unwrap();
    }
    let spec = RunLaunchSpec::v1(ExecutionSpec {
        argv: vec!["codex".into()],
        env: Vec::new(),
        agent_kind: Some("codex".into()),
        initial_prompt: Some("prior".into()),
        system_prompt: Some("system".into()),
    });
    let prior = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "codex",
            argv_json: &serde_json::to_string(&spec.execution().argv).unwrap(),
            prompt_snapshot: "prior",
            system_prompt_snapshot: Some("system"),
            launch_spec_json: Some(&serde_json::to_string(&spec).unwrap()),
            session_id: source,
            session: None,
        })
        .unwrap();
    db.promote_run_uow(prior.id, Some("w1"), Some(PRIOR_PANE), None)
        .unwrap();
    db.finalize_run_uow(&FinalizeRun {
        run_id: prior.id,
        outcome: RunOutcome::Ok,
        summary: None,
        comments: &[(&format!("agent:{}", prior.id), "done")],
        target_column_id: None,
        final_status: CardStatus::Done,
        final_awaiting_reason: None,
        next: None,
    })
    .unwrap();
    (card.id, card.column_id)
}

/// A daemon whose default herdr session is the fake server, with a recording
/// spawner — the full launch path through `spawn_one` runs for real.
fn reuse_decision_daemon(spawner: Arc<CapturingSpawner>, herdr: &FakeHerdr) -> Arc<Daemon> {
    testkit::daemon()
        .spawner(spawner)
        .registry(Some(crate::session::SessionRegistry::with_entries(
            herdr.socket.clone(),
            Vec::new(),
        )))
        .build_daemon()
}

/// A codex retry records the SOURCE conversation id (the fork's new thread id
/// arrives only at promotion) and its argv ends `fork <source-id>`. With a
/// live prior pane holding that same id, the reuse candidate resolves — but
/// re-prompting it would keep the old conversation and the fork would never
/// execute. The retry must launch fresh with `reuse_pane_id: None`.
#[tokio::test]
async fn codex_fork_retry_never_reuses_the_prior_live_pane() {
    let spawner = Arc::new(CapturingSpawner::default());
    let herdr = reuse_decision_server();
    let d = reuse_decision_daemon(spawner.clone(), &herdr);
    let (card_id, column_id) = codex_card_with_ended_prior_run(&d, Some(SOURCE_SESSION));

    let retry = enqueue_run(&d, card_id, column_id, true).unwrap();
    let argv: Vec<String> = serde_json::from_str(&retry.argv_json).unwrap();
    assert_eq!(argv, ["codex", "fork", SOURCE_SESSION]);
    assert_eq!(retry.session_id.as_deref(), Some(SOURCE_SESSION));

    dispatch_pass(&d).await;

    let requests = spawner.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "exactly one launch request");
    let req = &requests[0];
    assert_eq!(req.argv, ["codex", "fork", SOURCE_SESSION]);
    assert_eq!(
        req.reuse_pane_id, None,
        "a codex retry (`fork <source-id>`) must never re-prompt the prior \
         live same-session pane — it must launch fresh so codex actually forks"
    );
    // Nothing was re-prompted or re-spawned against herdr: the launch made
    // only the resolution calls.
    assert_eq!(
        herdr.methods(),
        [
            "ping",
            "workspace.list",
            "session.snapshot",
            "session.snapshot"
        ]
    );
    assert!(
        herdr.requests_for("agent.prompt").is_empty(),
        "a fork launch must not prompt any pane"
    );
    // The fresh launch registered as a running run.
    let run = d.store.lock().get_run(retry.id).unwrap();
    assert!(run.started_at.is_some());
}

/// A non-fresh hop (resume) still reuses the exact same conversation pane:
/// the prior live pane holding the recorded id is re-prompted in place.
#[tokio::test]
async fn codex_resume_hop_reuses_the_exact_prior_pane() {
    let spawner = Arc::new(CapturingSpawner::default());
    let herdr = reuse_decision_server();
    let d = reuse_decision_daemon(spawner.clone(), &herdr);
    let (card_id, column_id) = codex_card_with_ended_prior_run(&d, Some(SOURCE_SESSION));

    let resume = enqueue_run(&d, card_id, column_id, false).unwrap();
    let argv: Vec<String> = serde_json::from_str(&resume.argv_json).unwrap();
    assert_eq!(argv, ["codex", "resume", SOURCE_SESSION]);

    dispatch_pass(&d).await;

    let requests = spawner.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "exactly one launch request");
    assert_eq!(requests[0].argv, ["codex", "resume", SOURCE_SESSION]);
    assert_eq!(
        requests[0].reuse_pane_id.as_deref(),
        Some(PRIOR_PANE),
        "a resume hop re-prompts the exact prior conversation pane"
    );
}

/// A mint hop mints a new (or self-minted) conversation and finds no reuse
/// match: no recorded source id, no reuse.
#[tokio::test]
async fn codex_mint_hop_finds_no_reuse_match() {
    let spawner = Arc::new(CapturingSpawner::default());
    let herdr = reuse_decision_server();
    let d = reuse_decision_daemon(spawner.clone(), &herdr);
    let (card_id, column_id) = codex_card_with_ended_prior_run(&d, None);

    let mint = enqueue_run(&d, card_id, column_id, false).unwrap();
    let argv: Vec<String> = serde_json::from_str(&mint.argv_json).unwrap();
    assert_eq!(argv, ["codex"]);
    assert_eq!(mint.session_id, None);

    dispatch_pass(&d).await;

    let requests = spawner.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "exactly one launch request");
    assert_eq!(requests[0].argv, ["codex"]);
    assert_eq!(
        requests[0].reuse_pane_id, None,
        "a mint hop has no source conversation to reuse"
    );
}
