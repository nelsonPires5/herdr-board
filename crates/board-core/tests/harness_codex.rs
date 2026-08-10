//! Codex harness RED contracts (C3 session policy + C6 adapter).
//!
//! These tests pin the codex built-in contracts *before* the adapter exists.
//! They are intentionally RED today: every `session_argv("codex", …)`,
//! `build_invocation("codex", …)`, capability-catalog and rescue-rethreading
//! assertion below fails with `UnknownHarness` or a behavior mismatch until C7
//! implements the built-in adapter.
//!
//! Verified facts (the Codex harness plan + `docs/research.md`):
//! - codex mints its own thread/session UUID; there is **no public
//!   `--session-id` for Mint**, so the board must never invent one;
//! - resume and fork are **subcommands**: `codex resume <id>` / `codex fork
//!   <id>`, appended last to the startup argv like every other harness's
//!   session flags;
//! - reasoning effort rides `-c model_reasoning_effort=<value>`, and codex
//!   spells the lowest level `none` where the board says `off`;
//! - approval rides user-facing presets: `ask-for-approval` →
//!   `--sandbox workspace-write --ask-for-approval on-request`,
//!   `approve-for-me` → `--approve-for-me`, `full-access` →
//!   `--dangerously-bypass-approvals-and-sandbox`;
//! - sandbox is a separate dimension and is only spelled explicitly for the
//!   `ask-for-approval` preset (the approve-for-me preset already routes
//!   through the workspace-write sandbox per the CLI's own help);
//! - codex has no system-prompt file equivalent: the managed prompt channels
//!   (`initial_prompt` / `system_prompt`) are the only prompt transport, and
//!   startup argv carries neither task nor system text.
//!
//! What cannot be pinned here (daemon-owned, C4/C5 REDs live in
//! `board-daemon`):
//! - `prepare_enqueue_values` must persist **NULL** for a codex Mint instead
//!   of its synthetic `target_session` fallback — the adapter side (this
//!   file) reports `resulting_session_id: None` so the daemon has a signal;
//! - the Mint prompt submission (`system_prompt + task` delimited block) and
//!   the post-launch `agent.get.agent_session` thread-id capture/promotion.

use board_core::capability::{
    available_harnesses, capabilities_for, default_capabilities, meta_for, resume_support_for,
    HarnessCapabilities, ResumeSupport,
};
use board_core::config::{Config, HarnessDef};
use board_core::harness::{
    build_invocation, is_builtin_harness, plan_session, resume_invocation, session_argv,
    HarnessError, SessionPlan, BOARD_PROTOCOL_TRAILER, BOARD_RESCUE, BUILTIN_HARNESSES,
};
use board_core::launch::ExecutionSpec;
use board_core::prompt::EffectiveSettings;
use board_core::protocol::Effort;

fn codex_settings() -> EffectiveSettings {
    EffectiveSettings {
        harness: "codex".into(),
        model: Some("gpt-5.6".into()),
        effort: Some(Effort::Low),
        permission_mode: Some("ask-for-approval".into()),
        system_prompt: Some("PLAN stage".into()),
        fresh_session: false,
        timeout_minutes: None,
    }
}

// ---------------------------------------------------------------------------
// C3 — session policy: Mint has no synthetic id; Resume/Fork use the real id
// ---------------------------------------------------------------------------

#[test]
fn codex_session_argv_contract() {
    // Mint: no session flag at all; the board never supplies a synthetic id.
    assert_eq!(
        session_argv("codex", &SessionPlan::Mint, None).unwrap(),
        (vec![], None)
    );
    assert_eq!(
        session_argv("codex", &SessionPlan::Mint, Some("synthetic-uuid")).unwrap(),
        (vec![], None),
        "codex mints its own thread id; a board-invented uuid must never surface"
    );
    // Resume: `codex resume <id>` with the real id persisted.
    assert_eq!(
        session_argv("codex", &SessionPlan::Resume("thread-1".into()), None).unwrap(),
        (
            vec!["resume".to_string(), "thread-1".to_string()],
            Some("thread-1".to_string())
        )
    );
    // Fork: `codex fork <id>` keeps the real source id at enqueue; the fork's
    // NEW thread id is only known once the integration reports it, and
    // replaces it atomically at promotion (C4).
    assert_eq!(
        session_argv("codex", &SessionPlan::Fork("thread-1".into()), None).unwrap(),
        (
            vec!["fork".to_string(), "thread-1".to_string()],
            Some("thread-1".to_string())
        )
    );
    assert_eq!(
        session_argv(
            "codex",
            &SessionPlan::Fork("thread-1".into()),
            Some("synthetic-uuid")
        )
        .unwrap(),
        (
            vec!["fork".to_string(), "thread-1".to_string()],
            Some("thread-1".to_string())
        )
    );
}

#[test]
fn codex_enqueue_policy_never_invents_an_id() {
    // The full policy chain: whatever plan_session decides, a codex launch
    // persists exactly the real id — never a board-minted uuid. The daemon's
    // `prepare_enqueue_values` Mint fallback (`target_session`) must be
    // suppressed for codex; the adapter side pinned here reports None so the
    // daemon has an unambiguous signal to persist NULL.
    let cases = [
        (None, false, false, SessionPlan::Mint),
        (
            Some("thread-1"),
            false,
            false,
            SessionPlan::Resume("thread-1".into()),
        ),
        (
            Some("thread-1"),
            false,
            true,
            SessionPlan::Fork("thread-1".into()),
        ),
        (Some("thread-1"), true, false, SessionPlan::Mint),
    ];
    for (existing, fresh, retry, expected) in cases {
        let plan = plan_session(existing, fresh, retry);
        assert_eq!(plan, expected);
        let inv = build_invocation(
            "codex",
            &Config::default(),
            &codex_settings(),
            &plan,
            Some("synthetic-uuid"),
            "task",
        )
        .unwrap();
        match plan {
            SessionPlan::Mint => {
                assert_eq!(
                    inv.resulting_session_id, None,
                    "codex Mint must persist NULL, never a board-invented uuid"
                );
                assert!(
                    !inv.argv.iter().any(|a| a.contains("synthetic-uuid")),
                    "the offered minted uuid must not leak into codex argv"
                );
            }
            SessionPlan::Resume(id) | SessionPlan::Fork(id) => {
                assert_eq!(
                    inv.resulting_session_id.as_deref(),
                    Some(id.as_str()),
                    "resume/fork keep the real thread id"
                );
            }
        }
    }
}

#[test]
fn configured_harnesses_keep_their_synthetic_mint_fallback() {
    // The no-invented-uuid policy is codex-specific. A config-defined harness
    // keeps the current contract: build_invocation reports no resulting
    // session id, and the daemon's Mint fallback persists the synthetic uuid.
    let mut config = Config::default();
    config.harness.insert(
        "fake".into(),
        HarnessDef {
            argv: vec!["run".into()],
            ..Default::default()
        },
    );
    let inv = build_invocation(
        "fake",
        &config,
        &codex_settings(),
        &SessionPlan::Mint,
        Some("synthetic-uuid"),
        "p",
    )
    .unwrap();
    assert_eq!(inv.resulting_session_id, None);
    assert_eq!(inv.argv, vec!["run"]);
}

// ---------------------------------------------------------------------------
// C6 — adapter argv: mint/resume/fork, effort + approval mapping, prompt
// ---------------------------------------------------------------------------

#[test]
fn codex_is_registered_builtin_after_claude() {
    assert!(is_builtin_harness("codex"));
    assert_eq!(
        BUILTIN_HARNESSES.to_vec(),
        vec!["pi", "claude", "codex", "opencode"]
    );
    let list = available_harnesses(&Config::default());
    assert_eq!(
        list.iter().position(|h| h.as_str() == "codex"),
        Some(2),
        "codex slots into the built-in list right after claude"
    );
    assert_eq!(
        list.iter().position(|h| h.as_str() == "opencode"),
        Some(3),
        "opencode slots in right after codex"
    );
}

#[test]
fn codex_mint_argv_exact_spelling() {
    let inv = build_invocation(
        "codex",
        &Config::default(),
        &codex_settings(),
        &SessionPlan::Mint,
        None,
        "implement the widget",
    )
    .unwrap();
    assert_eq!(inv.agent_kind.as_deref(), Some("codex"));
    assert_eq!(
        inv.argv,
        vec![
            "codex".to_string(),
            "--model".into(),
            "gpt-5.6".into(),
            "-c".into(),
            "model_reasoning_effort=low".into(),
            "--sandbox".into(),
            "workspace-write".into(),
            "--ask-for-approval".into(),
            "on-request".into(),
        ],
        "codex mint argv: free-form --model, effort via -c, the ask-for-approval preset, no session flag"
    );
    // Managed prompt transport: both channels ride outside argv.
    assert_eq!(inv.initial_prompt.as_deref(), Some("implement the widget"));
    let expected_system = format!("PLAN stage\n\n{BOARD_PROTOCOL_TRAILER}");
    assert_eq!(inv.system_prompt.as_deref(), Some(expected_system.as_str()));
    assert!(inv.env.is_empty());
}

#[test]
fn codex_resume_and_fork_argv_are_subcommands_with_the_real_id() {
    // Resume re-attaches to the recorded thread id.
    let inv = build_invocation(
        "codex",
        &Config::default(),
        &codex_settings(),
        &SessionPlan::Resume("thread-7".into()),
        None,
        "task",
    )
    .unwrap();
    assert_eq!(inv.resulting_session_id.as_deref(), Some("thread-7"));
    assert!(inv
        .argv
        .ends_with(&["resume".to_string(), "thread-7".to_string()]));

    // Fork keeps the real source id at enqueue (the NEW thread id replaces it
    // at promotion once the integration reports it).
    let inv = build_invocation(
        "codex",
        &Config::default(),
        &codex_settings(),
        &SessionPlan::Fork("thread-7".into()),
        None,
        "task",
    )
    .unwrap();
    assert_eq!(inv.resulting_session_id.as_deref(), Some("thread-7"));
    assert!(inv
        .argv
        .ends_with(&["fork".to_string(), "thread-7".to_string()]));
    // Never a board-invented uuid, even when one is offered.
    let inv = build_invocation(
        "codex",
        &Config::default(),
        &codex_settings(),
        &SessionPlan::Fork("thread-7".into()),
        Some("synthetic-uuid"),
        "task",
    )
    .unwrap();
    assert_eq!(inv.resulting_session_id.as_deref(), Some("thread-7"));
}

#[test]
fn codex_maps_off_effort_to_none_and_preserves_other_spellings() {
    let mut s = codex_settings();
    // The board's lowest effort is `off`; codex calls it `none`. The mapping
    // happens only while building argv — the enum stays unchanged.
    s.effort = Some(Effort::Off);
    let inv = build_invocation(
        "codex",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert!(inv
        .argv
        .windows(2)
        .any(|w| w[0] == "-c" && w[1] == "model_reasoning_effort=none"));

    // Every other level keeps its canonical spelling (no `ultra` in this
    // version).
    for (effort, spelling) in [
        (Effort::Minimal, "minimal"),
        (Effort::Low, "low"),
        (Effort::Medium, "medium"),
        (Effort::High, "high"),
        (Effort::Xhigh, "xhigh"),
        (Effort::Max, "max"),
    ] {
        s.effort = Some(effort);
        let inv = build_invocation(
            "codex",
            &Config::default(),
            &s,
            &SessionPlan::Mint,
            None,
            "t",
        )
        .unwrap();
        assert!(
            inv.argv
                .windows(2)
                .any(|w| { w[0] == "-c" && w[1] == format!("model_reasoning_effort={spelling}") }),
            "effort {effort:?} must keep its spelling"
        );
    }

    // No effort → no `-c` at all.
    s.effort = None;
    let inv = build_invocation(
        "codex",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert!(!inv.argv.iter().any(|a| a == "-c"));
}

#[test]
fn codex_maps_permission_presets_to_exact_argv() {
    // The board-facing codex approval vocabulary is three user-facing presets;
    // each maps to an exact verified codex CLI spelling (checked against the
    // installed CLI's help):
    //   ask-for-approval → --sandbox workspace-write --ask-for-approval on-request
    //   approve-for-me   → --approve-for-me          (routes through the
    //                       workspace-write sandbox per the CLI's own help)
    //   full-access      → --dangerously-bypass-approvals-and-sandbox
    let mut s = codex_settings();
    let cases = [
        (
            "ask-for-approval",
            vec![
                "--sandbox",
                "workspace-write",
                "--ask-for-approval",
                "on-request",
            ],
        ),
        ("approve-for-me", vec!["--approve-for-me"]),
        (
            "full-access",
            vec!["--dangerously-bypass-approvals-and-sandbox"],
        ),
    ];
    for (preset, expected) in cases {
        s.permission_mode = Some(preset.into());
        let inv = build_invocation(
            "codex",
            &Config::default(),
            &s,
            &SessionPlan::Mint,
            None,
            "t",
        )
        .unwrap();
        assert!(
            inv.argv.windows(expected.len()).any(|w| w == expected),
            "preset {preset} must map to {expected:?}; got {:?}",
            inv.argv
        );
    }
    s.permission_mode = None;
    let inv = build_invocation(
        "codex",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert!(!inv.argv.iter().any(|a| a == "--ask-for-approval"));
    assert!(!inv.argv.iter().any(|a| a == "--sandbox"));
    assert!(!inv.argv.iter().any(|a| a == "--approve-for-me"));
}

#[test]
fn codex_permission_preset_applies_before_resume_fork_subcommand() {
    // The preset flags sit with the other launch flags; resume/fork remain
    // the LAST two argv entries (the adapter-owned session syntax).
    let mut s = codex_settings();
    s.permission_mode = Some("full-access".into());
    let inv = build_invocation(
        "codex",
        &Config::default(),
        &s,
        &SessionPlan::Resume("thread-7".into()),
        None,
        "t",
    )
    .unwrap();
    assert!(inv
        .argv
        .contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
    assert!(inv
        .argv
        .ends_with(&["resume".to_string(), "thread-7".to_string()]));
}

#[test]
fn codex_startup_argv_contains_no_prompt_or_system_prompt() {
    let prompt = "implement the widget\nand respect --flags";
    let inv = build_invocation(
        "codex",
        &Config::default(),
        &codex_settings(),
        &SessionPlan::Mint,
        None,
        prompt,
    )
    .unwrap();
    for arg in &inv.argv {
        assert!(
            !arg.contains("implement the widget"),
            "startup argv must not embed the task text: {arg:?}"
        );
        assert!(
            !arg.contains("PLAN stage"),
            "startup argv must not embed the column system prompt: {arg:?}"
        );
        assert!(
            !arg.contains("herdr-board protocol"),
            "startup argv must not embed the protocol trailer: {arg:?}"
        );
        assert!(
            !arg.contains("instructions="),
            "the prompt must never ride -c instructions: {arg:?}"
        );
    }
    assert!(!inv.argv.iter().any(|a| a == "--"));
    assert!(!inv
        .argv
        .iter()
        .any(|a| a.starts_with("--append-system-prompt")));
    // Both managed prompt channels stay out of argv and are delivered by the
    // daemon after the agent is interactive (Mint: system_prompt + task).
    assert_eq!(inv.initial_prompt.as_deref(), Some(prompt));
    let expected_system = format!("PLAN stage\n\n{BOARD_PROTOCOL_TRAILER}");
    assert_eq!(inv.system_prompt.as_deref(), Some(expected_system.as_str()));
    assert!(inv.env.is_empty());
}

// ---------------------------------------------------------------------------
// C6 — capability catalog (adapter registration, C7 green target)
// ---------------------------------------------------------------------------

#[test]
fn codex_capability_catalog_shape() {
    let caps = default_capabilities("codex");
    assert_eq!(caps.harness, "codex");
    assert!(caps.models.is_empty());
    assert!(caps.model_freeform);
    assert_eq!(
        caps.default_efforts,
        vec![
            Effort::Off,
            Effort::Minimal,
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max,
        ]
    );
    assert_eq!(
        caps.permission_modes,
        vec!["ask-for-approval", "approve-for-me", "full-access"]
    );
    assert_eq!(caps.resume, ResumeSupport::ByConversationId);
    // The old approval-mode ids are gone: they were codex-internal spellings,
    // not board-facing presets. Sandbox vocabulary never hides inside the
    // permission field either.
    for word in [
        "untrusted",
        "on-request",
        "never",
        "read-only",
        "workspace-write",
    ] {
        assert!(
            !caps.permission_modes.iter().any(|p| p == word),
            "{word} must not appear as a board-facing preset"
        );
    }
}

#[test]
fn codex_resolves_through_meta_and_resume_support() {
    let cfg = Config::default();
    let meta = meta_for("codex", &cfg).unwrap();
    assert_eq!(meta.id(), "codex");
    assert!(meta.models().is_empty());
    assert!(meta.model_freeform());
    assert_eq!(
        meta.permissions(),
        vec!["ask-for-approval", "approve-for-me", "full-access"]
    );
    assert_eq!(meta.resume(), ResumeSupport::ByConversationId);
    // The trait answer and the wire snapshot never disagree.
    let caps = capabilities_for("codex", &cfg).unwrap();
    assert_eq!(caps, HarnessCapabilities::from_meta(meta.as_ref()));
    assert_eq!(
        resume_support_for("codex", &cfg),
        ResumeSupport::ByConversationId
    );
}

// ---------------------------------------------------------------------------
// C6 — rescue re-threading (dead-pane rescue never re-sends the task)
// ---------------------------------------------------------------------------

/// The startup-only argv form a codex mint persists (after C7): no session
/// tokens, no prompt text.
fn persisted_codex_mint() -> ExecutionSpec {
    ExecutionSpec {
        argv: vec![
            "codex".into(),
            "--model".into(),
            "recorded-model".into(),
            "-c".into(),
            "model_reasoning_effort=low".into(),
            "--sandbox".into(),
            "workspace-write".into(),
            "--ask-for-approval".into(),
            "on-request".into(),
        ],
        env: vec![("RECORDED_ENV".into(), "recorded-value".into())],
        agent_kind: Some("codex".into()),
        initial_prompt: Some("the original task".into()),
        system_prompt: Some("recorded system prompt".into()),
    }
}

#[test]
fn codex_mint_prompt_block_is_a_single_delimited_system_then_task_block() {
    use board_core::harness::codex::{mint_prompt, MINT_SYSTEM_DELIMITER, MINT_TASK_DELIMITER};

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

#[test]
fn codex_resume_invocation_rethreads_mint_argv_to_resume_subcommand() {
    let spec = resume_invocation(
        "codex",
        ResumeSupport::ByConversationId,
        &persisted_codex_mint(),
        "thread-9",
    )
    .unwrap();
    assert_eq!(
        spec.argv,
        vec![
            "codex",
            "--model",
            "recorded-model",
            "-c",
            "model_reasoning_effort=low",
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "on-request",
            "resume",
            "thread-9",
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
    assert_eq!(spec.agent_kind.as_deref(), Some("codex"));
    assert!(spec
        .env
        .contains(&(BOARD_RESCUE.to_string(), "1".to_string())));
}

#[test]
fn codex_resume_invocation_preserves_freeform_model_named_resume_or_fork() {
    for model in ["resume", "fork"] {
        let mut spec = persisted_codex_mint();
        spec.argv[2] = model.to_string();
        spec.argv.extend(["fork".into(), "thread-1".into()]);

        let resumed =
            resume_invocation("codex", ResumeSupport::ByConversationId, &spec, "thread-9").unwrap();

        assert_eq!(resumed.argv[1..3], ["--model", model]);
        assert!(resumed.argv.contains(&"-c".to_string()));
        assert!(resumed
            .argv
            .ends_with(&["resume".to_string(), "thread-9".to_string()]));
    }
}

#[test]
fn codex_resume_invocation_rethreads_fork_argv_to_plain_resume() {
    let mut spec = persisted_codex_mint();
    spec.argv.extend(["fork".into(), "thread-1".into()]);
    let resumed =
        resume_invocation("codex", ResumeSupport::ByConversationId, &spec, "thread-9").unwrap();
    // The persisted fork subcommand is stripped, not duplicated.
    assert!(!resumed.argv.contains(&"thread-1".to_string()));
    assert!(resumed
        .argv
        .ends_with(&["resume".to_string(), "thread-9".to_string()]));
}

#[test]
fn codex_resume_invocation_refuses_legacy_all_in_one_argv() {
    // A `--` delimiter means the persisted argv embeds the task text, so
    // appending `resume <id>` would re-send it. Refuse instead of guessing.
    let mut spec = persisted_codex_mint();
    spec.argv.extend(["--".into(), "the original task".into()]);
    assert!(spec.argv.contains(&"--".to_string()));
    let err =
        resume_invocation("codex", ResumeSupport::ByConversationId, &spec, "thread-9").unwrap_err();
    assert_eq!(err, HarnessError::ResumeLegacyArgv("codex".into()));
}
