//! Antigravity harness adapter RED contracts (A7): session policy, argv,
//! permission modes, capability catalog and rescue re-threading.
//!
//! Field-verified facts (agy 1.1.13 — `agy --help`, `herdr agent start
//! --help`, the embedded herdr integration hook):
//! - the public harness name is `antigravity`; the Herdr managed-agent kind
//!   and the executable are `agy`;
//! - the TUI is `agy` with `--model <base>`, `--effort (low|medium|high)`,
//!   `--sandbox`, `--dangerously-skip-permissions`, and `--conversation
//!   <id>` (resume by conversation id). There is no `--permission-mode`
//!   flag and the board never edits the CLI's `settings.json`;
//! - a fresh TUI session mints its own conversation id (a UUID printed at
//!   startup) — there is **no way to pre-allocate** one, so a Mint carries
//!   no conversation flag and reports `resulting_session_id: None` (the
//!   daemon's signal to persist NULL; the reported id is promoted
//!   atomically after launch from `agent.get.agent_session`);
//! - resume AND retry are `--conversation <id>`: antigravity has **no
//!   fork**, so the board's Fork plan degrades to the resume spelling — a
//!   retry creates a new run and a new pane re-attached to the SAME
//!   conversation;
//! - permission modes are board-facing: `current` (no flag), `sandbox`
//!   (`--sandbox`), `always-proceed`
//!   (`--dangerously-skip-permissions`); any other spelling is rejected up
//!   front by the engine's capability validation;
//! - agy has no system-prompt file equivalent: the managed prompt channels
//!   (`initial_prompt` / `system_prompt`) are the only prompt transport,
//!   and startup argv carries neither task nor system text;
//! - the model is the normalized base id from the live catalog
//!   (`agy_catalog`): `gemini-3.7-flash-high` is listed as model
//!   `gemini-3.7-flash` with efforts `low|medium|high` and runs as
//!   `--model gemini-3.7-flash --effort high`. Fixed-effort models
//!   (`claude-sonnet-4-6`) never send `--effort`.

use board_core::capability::{
    available_harnesses, capabilities_for, default_capabilities, efforts_for, meta_for,
    resume_support_for, HarnessCapabilities, ResumeSupport,
};
use board_core::config::Config;
use board_core::engine::{validate_card_settings, validate_effective_settings, ValidationError};
use board_core::harness::{
    build_invocation, is_builtin_harness, plan_session, resume_invocation, session_argv,
    HarnessError, SessionPlan, BOARD_PROTOCOL_TRAILER, BOARD_RESCUE, BUILTIN_HARNESSES,
};
use board_core::launch::ExecutionSpec;
use board_core::model::{Card, Column};
use board_core::prompt::EffectiveSettings;
use board_core::protocol::Effort;

fn agy_settings() -> EffectiveSettings {
    EffectiveSettings {
        harness: "antigravity".into(),
        model: Some("gemini-3.7-flash".into()),
        effort: Some(Effort::High),
        permission_mode: Some("current".into()),
        system_prompt: Some("PLAN stage".into()),
        fresh_session: false,
        timeout_minutes: None,
    }
}

// ---------------------------------------------------------------------------
// A7 — session policy: Mint mints nothing synthetic; Resume AND Fork re-attach
// ---------------------------------------------------------------------------

#[test]
fn antigravity_session_argv_contract() {
    // Mint: no conversation flag at all; the board never supplies a
    // synthetic id (agy mints its own UUID at startup).
    assert_eq!(
        session_argv("antigravity", &SessionPlan::Mint, None).unwrap(),
        (vec![], None)
    );
    assert_eq!(
        session_argv("antigravity", &SessionPlan::Mint, Some("synthetic-uuid")).unwrap(),
        (vec![], None),
        "agy mints its own conversation id; a board-invented uuid must never surface"
    );
    // Resume: `--conversation <id>` with the real recorded id.
    assert_eq!(
        session_argv("antigravity", &SessionPlan::Resume("conv-1".into()), None).unwrap(),
        (
            vec!["--conversation".to_string(), "conv-1".to_string()],
            Some("conv-1".to_string())
        )
    );
    // Fork (board retry): agy has no fork — the retry re-attaches to the SAME
    // conversation in a NEW pane. The spelling is exactly the resume one, and
    // the recorded source id is what the run persists (the integration's
    // reported id replaces it atomically at promotion if the fallback fired).
    assert_eq!(
        session_argv("antigravity", &SessionPlan::Fork("conv-1".into()), None).unwrap(),
        (
            vec!["--conversation".to_string(), "conv-1".to_string()],
            Some("conv-1".to_string())
        )
    );
    assert_eq!(
        session_argv(
            "antigravity",
            &SessionPlan::Fork("conv-1".into()),
            Some("synthetic-uuid")
        )
        .unwrap(),
        (
            vec!["--conversation".to_string(), "conv-1".to_string()],
            Some("conv-1".to_string())
        )
    );
}

#[test]
fn antigravity_enqueue_policy_never_invents_an_id() {
    // Whatever plan_session decides, an antigravity launch persists exactly
    // the real id — never a board-minted uuid — and a Mint persists NULL.
    let cases = [
        (None, false, false, SessionPlan::Mint),
        (
            Some("conv-1"),
            false,
            false,
            SessionPlan::Resume("conv-1".into()),
        ),
        (
            Some("conv-1"),
            false,
            true,
            SessionPlan::Fork("conv-1".into()),
        ),
        (Some("conv-1"), true, false, SessionPlan::Mint),
    ];
    for (existing, fresh, retry, expected) in cases {
        let plan = plan_session(existing, fresh, retry);
        assert_eq!(plan, expected);
        let inv = build_invocation(
            "antigravity",
            &Config::default(),
            &agy_settings(),
            &plan,
            Some("synthetic-uuid"),
            "task",
        )
        .unwrap();
        match plan {
            SessionPlan::Mint => {
                assert_eq!(
                    inv.resulting_session_id, None,
                    "antigravity Mint must persist NULL, never a board-invented uuid"
                );
                assert!(
                    !inv.argv.iter().any(|a| a.contains("synthetic-uuid")),
                    "the offered minted uuid must not leak into agy argv"
                );
            }
            SessionPlan::Resume(id) | SessionPlan::Fork(id) => {
                assert_eq!(
                    inv.resulting_session_id.as_deref(),
                    Some(id.as_str()),
                    "resume/retry keep the real recorded conversation id"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// A7 — adapter argv: mint/resume/retry, model+effort, permission→argv, prompt
// ---------------------------------------------------------------------------

#[test]
fn antigravity_is_registered_builtin_after_opencode() {
    assert!(is_builtin_harness("antigravity"));
    assert_eq!(
        BUILTIN_HARNESSES.to_vec(),
        vec!["pi", "claude", "codex", "opencode", "antigravity"]
    );
    let list = available_harnesses(&Config::default());
    assert_eq!(
        list.iter().position(|h| h.as_str() == "antigravity"),
        Some(4),
        "antigravity slots into the built-in list right after opencode"
    );
}

#[test]
fn antigravity_mint_argv_exact_spelling() {
    let inv = build_invocation(
        "antigravity",
        &Config::default(),
        &agy_settings(),
        &SessionPlan::Mint,
        None,
        "implement the widget",
    )
    .unwrap();
    assert_eq!(inv.agent_kind.as_deref(), Some("agy"));
    // Model + effort + permission + no conversation flag: the exact launch
    // spelling for a Mint with effort high and default permission.
    assert_eq!(
        inv.argv,
        vec![
            "agy".to_string(),
            "--model".into(),
            "gemini-3.7-flash".into(),
            "--effort".into(),
            "high".into(),
        ],
        "agy mint argv: --model <base> --effort <level>; current permission is no flag"
    );
    // Managed prompt transport: both channels ride outside argv.
    assert_eq!(inv.initial_prompt.as_deref(), Some("implement the widget"));
    let expected_system = format!("PLAN stage\n\n{BOARD_PROTOCOL_TRAILER}");
    assert_eq!(inv.system_prompt.as_deref(), Some(expected_system.as_str()));
    for arg in &inv.argv {
        assert!(
            !arg.contains("implement the widget"),
            "startup argv must not embed the task text: {arg:?}"
        );
        assert!(
            !arg.contains("PLAN stage"),
            "startup argv must not embed the column system prompt: {arg:?}"
        );
    }
}

#[test]
fn antigravity_effort_maps_only_low_medium_high() {
    // agy only knows low|medium|high. Every board level with a spelling maps
    // onto it; levels without one are dropped (the capability ladder filters
    // them before argv anyway — a catalog-down free-form card could still
    // hold one, and it must not reach argv).
    for (effort, spelling) in [
        (Effort::Low, "low"),
        (Effort::Medium, "medium"),
        (Effort::High, "high"),
    ] {
        let mut s = agy_settings();
        s.effort = Some(effort);
        let inv = build_invocation(
            "antigravity",
            &Config::default(),
            &s,
            &SessionPlan::Mint,
            None,
            "t",
        )
        .unwrap();
        assert_eq!(inv.argv[3], "--effort");
        assert_eq!(inv.argv[4], spelling, "effort {effort:?} spelling");
    }
    // Off/minimal/xhigh/max have no agy spelling: no --effort flag at all.
    for effort in [Effort::Off, Effort::Minimal, Effort::Xhigh, Effort::Max] {
        let mut s = agy_settings();
        s.effort = Some(effort);
        let inv = build_invocation(
            "antigravity",
            &Config::default(),
            &s,
            &SessionPlan::Mint,
            None,
            "t",
        )
        .unwrap();
        assert!(
            !inv.argv.iter().any(|a| a == "--effort"),
            "effort {effort:?} has no agy spelling and must not ride argv: {:?}",
            inv.argv
        );
    }
    // No effort → no flag.
    let mut s = agy_settings();
    s.effort = None;
    let inv = build_invocation(
        "antigravity",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert!(!inv.argv.iter().any(|a| a == "--effort"));
}

#[test]
fn antigravity_maps_permission_modes_to_exact_argv() {
    // The board-facing antigravity permission vocabulary is exactly three
    // modes, each mapping to a verified CLI spelling:
    //   current         → (no flag — the CLI keeps the user's toolPermission;
    //                      the board never edits settings.json)
    //   sandbox         → --sandbox
    //   always-proceed  → --dangerously-skip-permissions
    let mut s = agy_settings();
    s.permission_mode = Some("sandbox".into());
    let inv = build_invocation(
        "antigravity",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert!(inv.argv.contains(&"--sandbox".to_string()));
    assert!(!inv.argv.iter().any(|a| a.starts_with("--dangerously")));

    s.permission_mode = Some("always-proceed".into());
    let inv = build_invocation(
        "antigravity",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert!(inv
        .argv
        .contains(&"--dangerously-skip-permissions".to_string()));
    assert!(!inv.argv.contains(&"--sandbox".to_string()));

    s.permission_mode = Some("current".into());
    let inv = build_invocation(
        "antigravity",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert!(
        !inv.argv
            .iter()
            .any(|a| a == "--sandbox" || a == "--dangerously-skip-permissions"),
        "current permission is the absence of every permission flag: {:?}",
        inv.argv
    );

    s.permission_mode = None;
    let inv = build_invocation(
        "antigravity",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert!(!inv.argv.iter().any(|a| a == "--sandbox"));
    assert!(!inv
        .argv
        .iter()
        .any(|a| a == "--dangerously-skip-permissions"));
}

#[test]
fn antigravity_permission_flag_applies_before_conversation_flag() {
    // The permission flag sits with the other launch flags; the conversation
    // syntax remains the LAST argv tokens (the adapter-owned session flags).
    let mut s = agy_settings();
    s.permission_mode = Some("always-proceed".into());
    let inv = build_invocation(
        "antigravity",
        &Config::default(),
        &s,
        &SessionPlan::Fork("conv-7".into()),
        None,
        "t",
    )
    .unwrap();
    let perm_at = inv
        .argv
        .iter()
        .position(|a| a == "--dangerously-skip-permissions")
        .unwrap();
    let session_at = inv.argv.len() - 2;
    assert!(
        perm_at < session_at,
        "the permission flag must precede the conversation flags; got {:?}",
        inv.argv
    );
    assert!(inv
        .argv
        .ends_with(&["--conversation".to_string(), "conv-7".to_string()]));
}

#[test]
fn antigravity_resume_and_retry_argv_are_the_same_conversation_flag() {
    // Resume re-attaches to the recorded conversation id.
    let inv = build_invocation(
        "antigravity",
        &Config::default(),
        &agy_settings(),
        &SessionPlan::Resume("conv-7".into()),
        None,
        "task",
    )
    .unwrap();
    assert_eq!(inv.resulting_session_id.as_deref(), Some("conv-7"));
    assert!(inv
        .argv
        .ends_with(&["--conversation".to_string(), "conv-7".to_string()]));

    // Retry (Fork plan) is the SAME argv: no fork flag exists, the retry
    // re-attaches to the same conversation in a new pane.
    let retry = build_invocation(
        "antigravity",
        &Config::default(),
        &agy_settings(),
        &SessionPlan::Fork("conv-7".into()),
        None,
        "task",
    )
    .unwrap();
    assert_eq!(retry.resulting_session_id.as_deref(), Some("conv-7"));
    assert_eq!(retry.argv, inv.argv, "agy never simulates a fork");
    // Never a board-invented uuid, even when one is offered.
    let inv = build_invocation(
        "antigravity",
        &Config::default(),
        &agy_settings(),
        &SessionPlan::Fork("conv-7".into()),
        Some("synthetic-uuid"),
        "task",
    )
    .unwrap();
    assert_eq!(inv.resulting_session_id.as_deref(), Some("conv-7"));
}

#[test]
fn antigravity_mint_prompt_block_is_a_single_delimited_system_then_task_block() {
    use board_core::harness::agy::{mint_prompt, MINT_SYSTEM_DELIMITER, MINT_TASK_DELIMITER};

    let system = "PLAN stage rules\nsecond line";
    let task = "implement the widget\nwith --flags";
    let block = mint_prompt(system, task);
    // One block, system first, task second, each under its own delimiter.
    let expected = format!("{MINT_SYSTEM_DELIMITER}\n{system}\n\n{MINT_TASK_DELIMITER}\n{task}");
    assert_eq!(block, expected);
    assert!(block.starts_with(MINT_SYSTEM_DELIMITER));
    let task_at = block.find(MINT_TASK_DELIMITER).unwrap();
    assert!(task_at > MINT_SYSTEM_DELIMITER.len());
    assert!(block[..task_at].contains(system));
    assert!(block[task_at..].contains(task));
    // The task text is never quoted or re-delimited inside the system half.
    assert!(!block[..task_at].contains("implement the widget"));
}

// ---------------------------------------------------------------------------
// A7 — capability catalog: live catalog only, free-form when unavailable
// ---------------------------------------------------------------------------

#[test]
fn antigravity_capability_catalog_default_is_the_down_state() {
    // `default_capabilities` (no config) is the catalog-unavailable state:
    // free-form, no models to offer, three permission modes, the agy effort
    // ladder for any model, resume by conversation id.
    let caps = default_capabilities("antigravity");
    assert_eq!(caps.harness, "antigravity");
    assert!(caps.models.is_empty());
    assert!(
        caps.model_freeform,
        "no catalog → free-form (stored models run)"
    );
    assert_eq!(
        caps.default_efforts,
        vec![Effort::Low, Effort::Medium, Effort::High],
        "the agy effort ladder is exactly low|medium|high"
    );
    assert_eq!(
        caps.permission_modes,
        vec!["current", "sandbox", "always-proceed"],
        "the three verified permission modes; nothing else is board-facing"
    );
    assert_eq!(caps.resume, ResumeSupport::ByConversationId);
    // Another CLI's permission vocabulary never leaks in.
    for word in [
        "bypassPermissions",
        "ask-for-approval",
        "auto-approve",
        "--sandbox",
    ] {
        assert!(
            !caps.permission_modes.iter().any(|p| p == word),
            "{word} must not appear as an antigravity permission mode"
        );
    }
}

#[test]
fn antigravity_catalog_up_constrains_models_and_efforts() {
    // The daemon stamps the live probe into the config; capabilities_for then
    // reports the normalized catalog and stops being free-form.
    let config = Config {
        agy_models: Some(vec![
            board_core::capability::ModelInfo {
                id: "gemini-3.7-flash".into(),
                efforts: vec![Effort::Low, Effort::Medium, Effort::High],
            },
            board_core::capability::ModelInfo {
                id: "claude-sonnet-4-6".into(),
                efforts: vec![],
            },
        ]),
        ..Config::default()
    };
    let caps = capabilities_for("antigravity", &config).unwrap();
    assert_eq!(
        caps,
        HarnessCapabilities::from_meta(meta_for("antigravity", &config).unwrap().as_ref())
    );
    assert!(!caps.model_freeform, "live catalog up → authoritative");
    let ids: Vec<&str> = caps.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["gemini-3.7-flash", "claude-sonnet-4-6"]);
    // Known model → its catalog efforts; fixed-effort model → no effort at
    // all; unknown model (should never reach a launch) → the agy ladder.
    assert_eq!(
        efforts_for(&caps, Some("gemini-3.7-flash")),
        vec![Effort::Low, Effort::Medium, Effort::High]
    );
    assert!(efforts_for(&caps, Some("claude-sonnet-4-6")).is_empty());
    assert_eq!(
        efforts_for(&caps, Some("unknown-model")),
        vec![Effort::Low, Effort::Medium, Effort::High]
    );
}

// ---------------------------------------------------------------------------
// A7 — engine validation: catalog-up fail-closed, catalog-down fail-open
// ---------------------------------------------------------------------------

fn card_with(
    harness: &str,
    model: Option<&str>,
    effort: Option<Effort>,
    permission: Option<&str>,
) -> Card {
    Card {
        id: 0,
        board_id: 0,
        column_id: 0,
        position: 0,
        title: String::new(),
        description: String::new(),
        harness: harness.into(),
        model: model.map(str::to_string),
        effort,
        permission_mode: permission.map(str::to_string),
        session: None,
        space_kind: board_core::protocol::SpaceKind::Workspace,
        space_ref: None,
        space_cwd: None,
        status: board_core::protocol::CardStatus::Idle,
        awaiting_reason: None,
        session_id: None,
        created_at: String::new(),
        updated_at: String::new(),
        archived_at: None,
        labels: board_core::protocol::CardLabels::default(),
    }
}

fn plain_column() -> Column {
    Column {
        id: 0,
        board_id: 0,
        name: String::new(),
        position: 0,
        system_prompt: None,
        trigger: board_core::protocol::Trigger::Auto,
        on_success_column_id: None,
        on_fail_column_id: None,
        fresh_session: false,
        harness_override: None,
        model_override: None,
        effort_override: None,
        permission_override: None,
        timeout_minutes: None,
    }
}

#[test]
fn antigravity_catalog_down_accepts_stored_models() {
    // Catalog unavailable (agy_models None): free-form — a stored model that
    // the (unreachable) catalog cannot prove gone must keep running.
    let config = Config::default();
    let card = card_with(
        "antigravity",
        Some("gemini-3.7-flash"),
        Some(Effort::High),
        Some("sandbox"),
    );
    validate_effective_settings(&card, &plain_column(), &config).unwrap();
}

#[test]
fn antigravity_catalog_up_rejects_removed_models() {
    // Catalog available and the stored model is no longer listed: the enqueue
    // must fail closed with an actionable InvalidModel error.
    let config = Config {
        agy_models: Some(vec![board_core::capability::ModelInfo {
            id: "gemini-3.7-flash".into(),
            efforts: vec![Effort::Low, Effort::Medium, Effort::High],
        }]),
        ..Config::default()
    };
    let card = card_with(
        "antigravity",
        Some("model-removed-from-catalog"),
        Some(Effort::High),
        None,
    );
    let err = validate_effective_settings(&card, &plain_column(), &config).unwrap_err();
    assert!(matches!(err, ValidationError::InvalidModel(m) if m == "model-removed-from-catalog"));
}

#[test]
fn antigravity_fixed_effort_model_rejects_an_effort() {
    // Catalog up: a fixed-effort model (claude-sonnet-4-6, efforts []) with a
    // stored effort is invalid — the launch would silently drop it.
    let config = Config {
        agy_models: Some(vec![board_core::capability::ModelInfo {
            id: "claude-sonnet-4-6".into(),
            efforts: vec![],
        }]),
        ..Config::default()
    };
    let card = card_with(
        "antigravity",
        Some("claude-sonnet-4-6"),
        Some(Effort::High),
        None,
    );
    let err = validate_card_settings(&card, &config).unwrap_err();
    assert!(
        matches!(err, ValidationError::InvalidEffort(_)),
        "a fixed-effort model with an effort must fail: {err:?}"
    );
    // Same card without an effort validates.
    let card = card_with("antigravity", Some("claude-sonnet-4-6"), None, None);
    validate_card_settings(&card, &config).unwrap();
}

#[test]
fn antigravity_rejects_out_of_ladder_efforts_even_catalog_down() {
    // agy only knows low|medium|high — even free-form, a stored effort
    // outside the ladder is rejected (a launch would silently drop it).
    let config = Config::default();
    let card = card_with(
        "antigravity",
        Some("gemini-3.7-flash"),
        Some(Effort::Max),
        None,
    );
    let err = validate_effective_settings(&card, &plain_column(), &config).unwrap_err();
    assert!(
        matches!(err, ValidationError::InvalidEffort(_)),
        "effort max has no agy spelling and must fail: {err:?}"
    );
}

#[test]
fn antigravity_accepts_only_the_three_permission_modes() {
    let config = Config::default();
    for mode in ["current", "sandbox", "always-proceed"] {
        let card = card_with("antigravity", None, None, Some(mode));
        validate_effective_settings(&card, &plain_column(), &config)
            .unwrap_or_else(|e| panic!("{mode} must validate: {e:?}"));
    }
    let card = card_with("antigravity", None, None, Some("full-access"));
    let err = validate_effective_settings(&card, &plain_column(), &config).unwrap_err();
    assert!(
        matches!(err, ValidationError::InvalidPermission(ref p) if p == "full-access"),
        "{err:?}"
    );
}

#[test]
fn antigravity_resolves_through_meta_and_resume_support() {
    let cfg = Config::default();
    let meta = meta_for("antigravity", &cfg).unwrap();
    assert_eq!(meta.id(), "antigravity");
    assert!(meta.model_freeform());
    assert_eq!(
        meta.permissions(),
        vec![
            "current".to_string(),
            "sandbox".to_string(),
            "always-proceed".to_string()
        ]
    );
    assert_eq!(meta.resume(), ResumeSupport::ByConversationId);
    assert_eq!(
        resume_support_for("antigravity", &cfg),
        ResumeSupport::ByConversationId
    );
}

// ---------------------------------------------------------------------------
// A7 — rescue re-threading (dead-pane rescue never re-sends the task)
// ---------------------------------------------------------------------------

/// The startup-only argv form an agy mint persists: model + effort, no
/// conversation tokens, no prompt text.
fn persisted_agy_mint() -> ExecutionSpec {
    ExecutionSpec {
        argv: vec![
            "agy".into(),
            "--model".into(),
            "gemini-3.7-flash".into(),
            "--effort".into(),
            "high".into(),
        ],
        env: vec![("RECORDED_ENV".into(), "recorded-value".into())],
        agent_kind: Some("agy".into()),
        initial_prompt: Some("the original task".into()),
        system_prompt: Some("recorded system prompt".into()),
    }
}

#[test]
fn antigravity_resume_invocation_rethreads_mint_argv_to_conversation_flag() {
    let spec = resume_invocation(
        "antigravity",
        ResumeSupport::ByConversationId,
        &persisted_agy_mint(),
        "conv-9",
    )
    .unwrap();
    assert_eq!(
        spec.argv,
        vec![
            "agy",
            "--model",
            "gemini-3.7-flash",
            "--effort",
            "high",
            "--conversation",
            "conv-9"
        ]
    );
    // Rescue never re-sends the task, and never re-runs it.
    assert_eq!(spec.initial_prompt, None);
    assert!(!spec.env.iter().any(|(k, _)| k == "BOARD_PROMPT"));
    assert!(!spec.argv.iter().any(|a| a.contains("the original task")));
    // The rest of the recorded execution environment is preserved verbatim.
    assert!(spec
        .env
        .contains(&("RECORDED_ENV".to_string(), "recorded-value".to_string())));
    assert_eq!(
        spec.system_prompt.as_deref(),
        Some("recorded system prompt")
    );
    assert_eq!(spec.agent_kind.as_deref(), Some("agy"));
    assert!(spec
        .env
        .contains(&(BOARD_RESCUE.to_string(), "1".to_string())));
}

#[test]
fn antigravity_resume_invocation_strips_persisted_old_conversation() {
    let mut spec = persisted_agy_mint();
    spec.argv.extend(["--conversation".into(), "conv-1".into()]);
    let resumed = resume_invocation(
        "antigravity",
        ResumeSupport::ByConversationId,
        &spec,
        "conv-9",
    )
    .unwrap();
    // The persisted old conversation flag is stripped, not duplicated.
    assert!(!resumed.argv.contains(&"conv-1".to_string()));
    assert!(resumed
        .argv
        .ends_with(&["--conversation".to_string(), "conv-9".to_string()]));
}

#[test]
fn antigravity_resume_invocation_refuses_legacy_all_in_one_argv() {
    // A `--` delimiter means the persisted argv embeds the task text, so
    // appending `--conversation <id>` would re-send it. Refuse instead of
    // guessing.
    let mut spec = persisted_agy_mint();
    spec.argv.extend(["--".into(), "the original task".into()]);
    assert!(spec.argv.contains(&"--".to_string()));
    let err = resume_invocation(
        "antigravity",
        ResumeSupport::ByConversationId,
        &spec,
        "conv-9",
    )
    .unwrap_err();
    assert_eq!(err, HarnessError::ResumeLegacyArgv("antigravity".into()));
}
