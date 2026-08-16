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

/// A self-minting harness card (codex/opencode) whose prior durable run ended
/// in `w1:p-prior` — the pane stays open in herdr (finished panes remain
/// visible by design). When `source` is set, the prior run captured that
/// conversation id and the card keeps it, so the next hop carries the same
/// `session_id` whether it resumes or forks.
fn self_minted_card_with_ended_prior_run(
    d: &Arc<Daemon>,
    harness: &str,
    source: Option<&str>,
) -> (i64, i64) {
    let db = d.store.lock();
    let column = db
        .create_column(&ColumnCreateParams {
            name: harness.into(),
            trigger: Some(Trigger::Auto),
            ..Default::default()
        })
        .unwrap();
    let card = db
        .create_card(&CardCreateParams {
            column_id: Some(column.id),
            title: format!("{harness} retry"),
            harness: Some(harness.into()),
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
        argv: vec![harness.into()],
        env: Vec::new(),
        agent_kind: Some(harness.into()),
        initial_prompt: Some("prior".into()),
        system_prompt: Some("system".into()),
    });
    let prior = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness,
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

/// The codex view of [`self_minted_card_with_ended_prior_run`].
fn codex_card_with_ended_prior_run(d: &Arc<Daemon>, source: Option<&str>) -> (i64, i64) {
    self_minted_card_with_ended_prior_run(d, "codex", source)
}

/// A daemon whose default herdr session is the fake server, with a recording
/// spawner — the full launch path through `spawn_one` runs for real.
fn reuse_decision_daemon(spawner: Arc<dyn Spawner>, herdr: &FakeHerdr) -> Arc<Daemon> {
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

/// An opencode retry records the SOURCE session id (the fork's new `ses_…` id
/// arrives only at promotion) and its argv ends `-s <source-id> --fork`. With
/// a live prior pane holding that same id, the reuse candidate resolves — but
/// re-prompting it would keep the old conversation and the fork would never
/// execute. The retry must launch fresh with `reuse_pane_id: None`.
#[tokio::test]
async fn opencode_fork_retry_never_reuses_the_prior_live_pane() {
    let spawner = Arc::new(CapturingSpawner::default());
    let herdr = reuse_decision_server();
    let d = reuse_decision_daemon(spawner.clone(), &herdr);
    let (card_id, column_id) =
        self_minted_card_with_ended_prior_run(&d, "opencode", Some(SOURCE_SESSION));

    let retry = enqueue_run(&d, card_id, column_id, true).unwrap();
    let argv: Vec<String> = serde_json::from_str(&retry.argv_json).unwrap();
    assert_eq!(
        argv,
        ["opencode", "-s", SOURCE_SESSION, "--fork"],
        "the opencode fork spelling is `-s <source-id> --fork`"
    );
    assert_eq!(retry.session_id.as_deref(), Some(SOURCE_SESSION));

    dispatch_pass(&d).await;

    let requests = spawner.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "exactly one launch request");
    let req = &requests[0];
    assert_eq!(req.argv, ["opencode", "-s", SOURCE_SESSION, "--fork"]);
    assert_eq!(
        req.reuse_pane_id, None,
        "an opencode retry (`-s <source-id> --fork`) must never re-prompt the \
         prior live same-session pane — it must launch fresh so opencode actually forks"
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

/// An antigravity retry records the SOURCE conversation id and its argv ends
/// `--conversation <source-id>`. agy has no fork — the retry re-attaches to
/// the SAME conversation — but the retry contract still demands a NEW pane.
/// Since the resume hop's argv is byte-identical to the retry's (the flag is
/// the only session shape), every `--conversation` hop is treated as a fork:
/// it must launch fresh, never re-prompt the prior live same-session pane.
#[tokio::test]
async fn antigravity_retry_never_reuses_the_prior_live_pane() {
    let spawner = Arc::new(CapturingSpawner::default());
    let herdr = reuse_decision_server();
    let d = reuse_decision_daemon(spawner.clone(), &herdr);
    let (card_id, column_id) =
        self_minted_card_with_ended_prior_run(&d, "antigravity", Some(SOURCE_SESSION));

    let retry = enqueue_run(&d, card_id, column_id, true).unwrap();
    let argv: Vec<String> = serde_json::from_str(&retry.argv_json).unwrap();
    assert_eq!(
        argv,
        ["agy", "--conversation", SOURCE_SESSION],
        "the antigravity retry spelling is `--conversation <source-id>` (no fork flag exists)"
    );
    assert_eq!(retry.session_id.as_deref(), Some(SOURCE_SESSION));

    dispatch_pass(&d).await;

    let requests = spawner.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "exactly one launch request");
    let req = &requests[0];
    assert_eq!(req.argv, ["agy", "--conversation", SOURCE_SESSION]);
    assert_eq!(
        req.reuse_pane_id, None,
        "an antigravity retry (`--conversation <id>`) must never re-prompt the \
         prior live same-session pane — the retry contract demands a fresh pane"
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
        "a conversation-carrying launch must not prompt any pane"
    );
    // The fresh launch registered as a running run.
    let run = d.store.lock().get_run(retry.id).unwrap();
    assert!(run.started_at.is_some());
}

/// A non-fresh antigravity resume hop carries the SAME `--conversation <id>`
/// argv as a retry — agy cannot distinguish them and neither can the board —
/// so it too launches a fresh pane instead of re-prompting the prior live
/// one. Safe by construction: a resume that lost the reuse shortcut only
/// spawns one extra pane; a retry that wrongly reused would break the
/// retry-new-pane contract.
#[tokio::test]
async fn antigravity_resume_hop_never_reuses_either() {
    let spawner = Arc::new(CapturingSpawner::default());
    let herdr = reuse_decision_server();
    let d = reuse_decision_daemon(spawner.clone(), &herdr);
    let (card_id, column_id) =
        self_minted_card_with_ended_prior_run(&d, "antigravity", Some(SOURCE_SESSION));

    let resume = enqueue_run(&d, card_id, column_id, false).unwrap();
    let argv: Vec<String> = serde_json::from_str(&resume.argv_json).unwrap();
    assert_eq!(argv, ["agy", "--conversation", SOURCE_SESSION]);

    dispatch_pass(&d).await;

    let requests = spawner.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "exactly one launch request");
    assert_eq!(requests[0].argv, ["agy", "--conversation", SOURCE_SESSION]);
    assert_eq!(
        requests[0].reuse_pane_id, None,
        "antigravity resume argv is byte-identical to retry argv, so it never \
         reuses the prior conversation pane either"
    );
}

/// An antigravity mint hop carries no conversation flag: fresh pane, no
/// source to reuse, and the run persists NULL (agy mints its own id).
#[tokio::test]
async fn antigravity_mint_hop_finds_no_reuse_match() {
    let spawner = Arc::new(CapturingSpawner::default());
    let herdr = reuse_decision_server();
    let d = reuse_decision_daemon(spawner.clone(), &herdr);
    let (card_id, column_id) = self_minted_card_with_ended_prior_run(&d, "antigravity", None);

    let mint = enqueue_run(&d, card_id, column_id, false).unwrap();
    let argv: Vec<String> = serde_json::from_str(&mint.argv_json).unwrap();
    assert_eq!(argv, ["agy"]);
    assert_eq!(mint.session_id, None);

    dispatch_pass(&d).await;

    let requests = spawner.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "exactly one launch request");
    assert_eq!(requests[0].argv, ["agy"]);
    assert_eq!(
        requests[0].reuse_pane_id, None,
        "a mint hop has no source conversation to reuse"
    );
}

// ---------------------------------------------------------------------------
// Antigravity fallback detection (A7): when agy cannot find the recorded
// conversation it starts a new one; the integration reports the NEW id, and
// `spawn_one` turns the mismatch into a visible system warning on the card.
// A missing capture (integration not installed) degrades to a warning and the
// run still proceeds.
// ---------------------------------------------------------------------------

/// A recording spawner that returns a configurable captured conversation id,
/// so the fallback detector can observe the integration report differ from
/// the requested id.
struct CapturingSessionSpawner {
    requests: Mutex<Vec<HerdrLaunchPlan>>,
    captured: Option<String>,
}

impl Spawner for CapturingSessionSpawner {
    fn spawn(&self, req: &HerdrLaunchPlan) -> std::result::Result<RuntimeHandle, SpawnError> {
        self.requests.lock().unwrap().push(req.clone());
        Ok(RuntimeHandle {
            pid: Some(4242),
            captured_session_id: self.captured.clone(),
            ..Default::default()
        })
    }
    fn kill(&self, _h: &RuntimeHandle) -> anyhow::Result<()> {
        Ok(())
    }
    fn is_alive(&self, _h: &RuntimeHandle) -> anyhow::Result<bool> {
        Ok(false)
    }
}

fn capturing_session_spawner(captured: Option<&str>) -> Arc<CapturingSessionSpawner> {
    Arc::new(CapturingSessionSpawner {
        requests: Mutex::new(Vec::new()),
        captured: captured.map(str::to_string),
    })
}

/// The recorded conversation no longer exists: agy starts a new one and the
/// integration reports it. The NEW id is persisted atomically with the
/// promotion (run AND card), and the mismatch is recorded as a visible
/// system warning on the card.
#[tokio::test]
async fn antigravity_fallback_persists_new_conversation_and_warns() {
    let spawner = capturing_session_spawner(Some("conv-new"));
    let herdr = reuse_decision_server();
    let d = reuse_decision_daemon(spawner.clone(), &herdr);
    let (card_id, column_id) =
        self_minted_card_with_ended_prior_run(&d, "antigravity", Some("conv-old"));

    let retry = enqueue_run(&d, card_id, column_id, true).unwrap();
    assert_eq!(retry.session_id.as_deref(), Some("conv-old"));
    let argv: Vec<String> = serde_json::from_str(&retry.argv_json).unwrap();
    assert_eq!(argv, ["agy", "--conversation", "conv-old"]);

    dispatch_pass(&d).await;

    // The captured conversation replaced the recorded one atomically.
    let db = d.store.lock();
    let card = db.get_card(card_id).unwrap().unwrap();
    assert_eq!(
        card.session_id.as_deref(),
        Some("conv-new"),
        "the fallback conversation is now the card's recorded one"
    );
    let run = db.get_run(retry.id).unwrap();
    assert_eq!(run.session_id.as_deref(), Some("conv-new"));
    assert!(run.started_at.is_some());
    // The mismatch is a visible system warning naming both ids.
    let warnings: Vec<String> = db
        .list_comments(card_id)
        .unwrap()
        .into_iter()
        .filter(|c| c.is_system())
        .map(|c| c.body)
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "exactly one system warning: {warnings:?}"
    );
    assert!(
        warnings[0].contains("conv-old") && warnings[0].contains("conv-new"),
        "the warning must name the dead and the new conversation: {}",
        warnings[0]
    );
}

/// No conversation was captured at all (the herdr antigravity_cli integration
/// is not installed): the Mint run still proceeds, persists no session, and
/// warns that focus/retry of this run may be unavailable.
#[tokio::test]
async fn antigravity_mint_without_capture_warns_but_run_proceeds() {
    let spawner = capturing_session_spawner(None);
    let herdr = reuse_decision_server();
    let d = reuse_decision_daemon(spawner.clone(), &herdr);
    let (card_id, column_id) = self_minted_card_with_ended_prior_run(&d, "antigravity", None);

    let mint = enqueue_run(&d, card_id, column_id, false).unwrap();
    assert_eq!(mint.session_id, None);

    dispatch_pass(&d).await;

    let db = d.store.lock();
    let card = db.get_card(card_id).unwrap().unwrap();
    assert_eq!(card.session_id, None, "no capture → nothing to persist");
    let run = db.get_run(mint.id).unwrap();
    assert!(
        run.started_at.is_some(),
        "the run proceeds even without a capture"
    );
    let warnings: Vec<String> = db
        .list_comments(card_id)
        .unwrap()
        .into_iter()
        .filter(|c| c.is_system())
        .map(|c| c.body)
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "exactly one system warning: {warnings:?}"
    );
    assert!(
        warnings[0].contains("integration"),
        "the warning must point at the missing integration: {}",
        warnings[0]
    );
}

/// A resume whose capture is also missing keeps the recorded id (the fallback
/// is undetectable without the report) and warns about it.
#[tokio::test]
async fn antigravity_resume_without_capture_keeps_recorded_id_and_warns() {
    let spawner = capturing_session_spawner(None);
    let herdr = reuse_decision_server();
    let d = reuse_decision_daemon(spawner.clone(), &herdr);
    let (card_id, column_id) =
        self_minted_card_with_ended_prior_run(&d, "antigravity", Some("conv-old"));

    let resume = enqueue_run(&d, card_id, column_id, false).unwrap();
    assert_eq!(resume.session_id.as_deref(), Some("conv-old"));

    dispatch_pass(&d).await;

    let db = d.store.lock();
    let card = db.get_card(card_id).unwrap().unwrap();
    assert_eq!(
        card.session_id.as_deref(),
        Some("conv-old"),
        "without a capture the run keeps the recorded id"
    );
    let warnings: Vec<String> = db
        .list_comments(card_id)
        .unwrap()
        .into_iter()
        .filter(|c| c.is_system())
        .map(|c| c.body)
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "exactly one system warning: {warnings:?}"
    );
    assert!(
        warnings[0].contains("conv-old") && warnings[0].contains("integration"),
        "the warning must name the kept id and the integration: {}",
        warnings[0]
    );
}
