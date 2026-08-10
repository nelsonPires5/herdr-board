//! OpenCode harness RED contracts (O3 session policy + O6 adapter).
//!
//! These tests pin the opencode built-in contracts before the adapter is
//! fully wired: every `session_argv("opencode", …)`, `build_invocation(
//! "opencode", …)`, capability-catalog and rescue-rethreading assertion below
//! fails with `UnknownHarness` or a behavior mismatch until O7 implements the
//! built-in adapter.
//!
//! Field-verified facts (opencode 1.18.15, `opencode --help` +
//! `opencode run --help` + `opencode models --verbose`):
//! - the TUI is `opencode [project]` with `-m/--model provider/model`,
//!   `-s/--session <id>` (resume by session id), `--fork` (with `--session`),
//!   `--auto` (auto-approve); the root/TUI does **not** accept `--variant` —
//!   that spelling exists only on `opencode run` ("model variant
//!   (provider-specific reasoning effort, e.g., high, max, minimal)");
//! - effort therefore never rides argv: with a board effort set, the adapter
//!   injects a process-local config via the `OPENCODE_CONFIG_CONTENT` env var
//!   defining a stable custom agent `herdr-board` carrying exactly `model` +
//!   `variant`, selected with `--agent herdr-board` (the backend applies the
//!   agent's `variant` when its model matches). An effort without a model is
//!   a typed error; without an effort no config is injected and the model
//!   stays `-m`;
//! - a fresh TUI session mints its own `ses_…` id — there is **no way to
//!   pre-allocate** one, so a Mint carries no session flag and reports
//!   `resulting_session_id: None` (the daemon's signal to persist NULL; the
//!   reported id is promoted atomically after launch from
//!   `agent.get.agent_session`);
//! - resume/fork are `-s <root>` / `-s <root> --fork`, appended last to the
//!   startup argv like every other harness's session flags;
//! - the board calls the effort dimension **effort** everywhere (API/UI/DB);
//!   only the opencode spelling is `variant`, with the lowest board effort
//!   `off` mapped to opencode's own `none` vocabulary inside the agent config;
//! - permission modes are board-facing: `default` (no flag) and
//!   `auto-approve` (`--auto`); any other spelling is rejected up front by the
//!   existing engine validation against the catalog;
//! - opencode has no system-prompt file equivalent: the managed prompt
//!   channels (`initial_prompt` / `system_prompt`) are the only prompt
//!   transport, and startup argv carries neither task nor system text.

use board_core::capability::{
    available_harnesses, capabilities_for, default_capabilities, efforts_for, meta_for,
    resume_support_for, HarnessCapabilities, ResumeSupport,
};
use board_core::config::{Config, HarnessDef};
use board_core::harness::{
    build_invocation, is_builtin_harness, plan_session, resume_invocation, session_argv,
    HarnessError, SessionPlan, BOARD_PROTOCOL_TRAILER, BOARD_RESCUE, BUILTIN_HARNESSES,
};
use board_core::launch::ExecutionSpec;
use board_core::prompt::EffectiveSettings;
use board_core::protocol::Effort;

fn opencode_settings() -> EffectiveSettings {
    EffectiveSettings {
        harness: "opencode".into(),
        model: Some("opencode/deepseek-v4-flash-free".into()),
        effort: Some(Effort::Low),
        permission_mode: Some("auto-approve".into()),
        system_prompt: Some("PLAN stage".into()),
        fresh_session: false,
        timeout_minutes: None,
    }
}

// ---------------------------------------------------------------------------
// O3 — session policy: Mint has no synthetic id; Resume/Fork use the real id
// ---------------------------------------------------------------------------

#[test]
fn opencode_session_argv_contract() {
    // Mint: no session flag at all; the board never supplies a synthetic id.
    assert_eq!(
        session_argv("opencode", &SessionPlan::Mint, None).unwrap(),
        (vec![], None)
    );
    assert_eq!(
        session_argv("opencode", &SessionPlan::Mint, Some("synthetic-uuid")).unwrap(),
        (vec![], None),
        "opencode mints its own ses_ id; a board-invented uuid must never surface"
    );
    // Resume: `-s <root-id>` with the real id persisted.
    assert_eq!(
        session_argv("opencode", &SessionPlan::Resume("ses-1".into()), None).unwrap(),
        (
            vec!["-s".to_string(), "ses-1".to_string()],
            Some("ses-1".to_string())
        )
    );
    // Fork: `-s <root-id> --fork` keeps the real source id at enqueue; the
    // fork's NEW id is only known once the integration reports it, and
    // replaces it atomically at promotion.
    assert_eq!(
        session_argv("opencode", &SessionPlan::Fork("ses-1".into()), None).unwrap(),
        (
            vec!["-s".to_string(), "ses-1".to_string(), "--fork".to_string()],
            Some("ses-1".to_string())
        )
    );
    assert_eq!(
        session_argv(
            "opencode",
            &SessionPlan::Fork("ses-1".into()),
            Some("synthetic-uuid")
        )
        .unwrap(),
        (
            vec!["-s".to_string(), "ses-1".to_string(), "--fork".to_string()],
            Some("ses-1".to_string())
        )
    );
}

#[test]
fn opencode_enqueue_policy_never_invents_an_id() {
    // Whatever plan_session decides, an opencode launch persists exactly the
    // real id — never a board-minted uuid. The daemon's
    // `prepare_enqueue_values` Mint fallback (`target_session`) is suppressed
    // because the adapter side reports None, an unambiguous signal to persist
    // NULL.
    let cases = [
        (None, false, false, SessionPlan::Mint),
        (
            Some("ses-1"),
            false,
            false,
            SessionPlan::Resume("ses-1".into()),
        ),
        (
            Some("ses-1"),
            false,
            true,
            SessionPlan::Fork("ses-1".into()),
        ),
        (Some("ses-1"), true, false, SessionPlan::Mint),
    ];
    for (existing, fresh, retry, expected) in cases {
        let plan = plan_session(existing, fresh, retry);
        assert_eq!(plan, expected);
        let inv = build_invocation(
            "opencode",
            &Config::default(),
            &opencode_settings(),
            &plan,
            Some("synthetic-uuid"),
            "task",
        )
        .unwrap();
        match plan {
            SessionPlan::Mint => {
                assert_eq!(
                    inv.resulting_session_id, None,
                    "opencode Mint must persist NULL, never a board-invented uuid"
                );
                assert!(
                    !inv.argv.iter().any(|a| a.contains("synthetic-uuid")),
                    "the offered minted uuid must not leak into opencode argv"
                );
            }
            SessionPlan::Resume(id) | SessionPlan::Fork(id) => {
                assert_eq!(
                    inv.resulting_session_id.as_deref(),
                    Some(id.as_str()),
                    "resume/fork keep the real root session id"
                );
            }
        }
    }
}

#[test]
fn configured_harnesses_keep_their_synthetic_mint_fallback() {
    // The no-invented-uuid policy is built-in-specific. A config-defined
    // harness keeps the current contract: build_invocation reports no
    // resulting session id, and the daemon's Mint fallback persists the
    // synthetic uuid.
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
        &opencode_settings(),
        &SessionPlan::Mint,
        Some("synthetic-uuid"),
        "p",
    )
    .unwrap();
    assert_eq!(inv.resulting_session_id, None);
    assert_eq!(inv.argv, vec!["run"]);
}

// ---------------------------------------------------------------------------
// O6 — adapter argv: mint/resume/fork, effort→variant, permission→argv, prompt
// ---------------------------------------------------------------------------

#[test]
fn opencode_is_registered_builtin_after_codex() {
    assert!(is_builtin_harness("opencode"));
    assert_eq!(
        BUILTIN_HARNESSES.to_vec(),
        vec!["pi", "claude", "codex", "opencode"]
    );
    let list = available_harnesses(&Config::default());
    assert_eq!(
        list.iter().position(|h| h.as_str() == "opencode"),
        Some(3),
        "opencode slots into the built-in list right after codex"
    );
}

#[test]
fn opencode_mint_argv_exact_spelling() {
    let inv = build_invocation(
        "opencode",
        &Config::default(),
        &opencode_settings(),
        &SessionPlan::Mint,
        None,
        "implement the widget",
    )
    .unwrap();
    assert_eq!(inv.agent_kind.as_deref(), Some("opencode"));
    // Effort is set, so `--variant` must NOT ride argv (the TUI rejects it):
    // a process-local config agent carries model+variant and `--agent`
    // selects it; `-m` is dropped because the agent owns the model.
    assert_eq!(
        inv.argv,
        vec![
            "opencode".to_string(),
            "--agent".into(),
            "herdr-board".into(),
            "--auto".into(),
        ],
        "opencode mint argv with effort: --agent herdr-board, --auto permission, no -m, no --variant, no session flag"
    );
    assert!(
        !inv.argv.iter().any(|a| a == "--variant"),
        "the TUI does not accept --variant; it must never appear in argv"
    );
    assert!(
        !inv.argv.iter().any(|a| a == "-m"),
        "with an effort the model rides the herdr-board agent config, not -m"
    );
    // The effort config is process-local env, not argv.
    assert_eq!(inv.env.len(), 1);
    let (key, raw) = &inv.env[0];
    assert_eq!(key, "OPENCODE_CONFIG_CONTENT");
    let config: serde_json::Value =
        serde_json::from_str(raw).expect("env config must be valid JSON");
    assert_eq!(
        config,
        serde_json::json!({
            "agent": {
                "herdr-board": {
                    "model": "opencode/deepseek-v4-flash-free",
                    "variant": "low",
                }
            }
        }),
        "the agent config carries exactly model + variant"
    );
    // Managed prompt transport: both channels ride outside argv.
    assert_eq!(inv.initial_prompt.as_deref(), Some("implement the widget"));
    let expected_system = format!("PLAN stage\n\n{BOARD_PROTOCOL_TRAILER}");
    assert_eq!(inv.system_prompt.as_deref(), Some(expected_system.as_str()));
    assert!(
        !raw.contains("implement the widget"),
        "the config env carries only model/variant, never prompt text"
    );
}

#[test]
fn opencode_effort_without_model_is_a_typed_error() {
    // Effort is only expressible per-model through the agent config, so an
    // effort with no model fails loudly instead of dropping the setting.
    let mut s = opencode_settings();
    s.model = None;
    let err = build_invocation(
        "opencode",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap_err();
    assert_eq!(err, HarnessError::OpenCodeEffortRequiresModel);
}

#[test]
fn opencode_effort_free_launches_keep_model_on_dash_m_without_config() {
    // Without an effort nothing is injected: no --agent, no config env, and
    // the model stays the plain `-m provider/model` TUI flag.
    let mut s = opencode_settings();
    s.effort = None;
    let inv = build_invocation(
        "opencode",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert_eq!(
        inv.argv,
        vec![
            "opencode".to_string(),
            "-m".into(),
            "opencode/deepseek-v4-flash-free".into(),
            "--auto".into(),
        ]
    );
    assert!(!inv.argv.iter().any(|a| a == "--agent"));
    assert!(
        inv.env.is_empty(),
        "no effort → no OPENCODE_CONFIG_CONTENT env"
    );

    // A model-less, effort-less launch is still fine: nothing to carry.
    s.model = None;
    let inv = build_invocation(
        "opencode",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert_eq!(inv.argv, vec!["opencode".to_string(), "--auto".into()]);
    assert!(inv.env.is_empty());
}

#[test]
fn opencode_agent_config_escapes_special_characters_in_model() {
    // The config JSON is built with serde_json, never string interpolation:
    // quotes, backslashes, tabs and unicode in a model name must round-trip
    // byte-exactly and never corrupt the JSON or leak into argv/env.
    let hostile = "opencode/provider-\"quoted\"\\path\t☃&'$`";
    let mut s = opencode_settings();
    s.model = Some(hostile.into());
    let inv = build_invocation(
        "opencode",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "task with \"quotes\" too",
    )
    .unwrap();
    let (key, raw) = inv
        .env
        .iter()
        .find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
        .expect("effort is set, so the config env must be present");
    assert_eq!(key, "OPENCODE_CONFIG_CONTENT");
    // The raw payload must be valid JSON and parse back to the exact model.
    let config: serde_json::Value = serde_json::from_str(raw).expect("env must carry valid JSON");
    assert_eq!(
        config["agent"]["herdr-board"]["model"],
        serde_json::json!(hostile),
        "hostile model text must survive the JSON round-trip byte-exactly"
    );
    assert_eq!(config["agent"]["herdr-board"]["variant"], "low");
    // Escaping is real: the raw payload quotes the model string, so the
    // embedded quotes must be escaped there — proof this is not interpolation.
    assert!(
        raw.contains("\\\"quoted\\\""),
        "embedded quotes must be JSON-escaped in the raw payload: {raw}"
    );
    // The model text reaches the process only through the config env: it must
    // not appear verbatim in argv, and the prompt must not leak into either.
    assert!(!inv.argv.iter().any(|a| a.contains("provider-")));
    assert!(!raw.contains("task with"));
    assert!(!inv.argv.iter().any(|a| a.contains("task with")));
    assert!(!inv.env.iter().any(|(_, v)| v.contains("task with")));
}

#[test]
fn opencode_resume_and_fork_argv_are_session_flags_with_the_real_id() {
    // Resume re-attaches to the recorded root session id.
    let inv = build_invocation(
        "opencode",
        &Config::default(),
        &opencode_settings(),
        &SessionPlan::Resume("ses-7".into()),
        None,
        "task",
    )
    .unwrap();
    assert_eq!(inv.resulting_session_id.as_deref(), Some("ses-7"));
    assert!(inv.argv.ends_with(&["-s".to_string(), "ses-7".to_string()]));

    // Fork keeps the real source id at enqueue and adds --fork.
    let inv = build_invocation(
        "opencode",
        &Config::default(),
        &opencode_settings(),
        &SessionPlan::Fork("ses-7".into()),
        None,
        "task",
    )
    .unwrap();
    assert_eq!(inv.resulting_session_id.as_deref(), Some("ses-7"));
    assert!(inv
        .argv
        .ends_with(&["-s".to_string(), "ses-7".to_string(), "--fork".to_string()]));
    // Never a board-invented uuid, even when one is offered.
    let inv = build_invocation(
        "opencode",
        &Config::default(),
        &opencode_settings(),
        &SessionPlan::Fork("ses-7".into()),
        Some("synthetic-uuid"),
        "task",
    )
    .unwrap();
    assert_eq!(inv.resulting_session_id.as_deref(), Some("ses-7"));
}

#[test]
fn opencode_maps_off_effort_to_none_and_preserves_other_spellings() {
    let mut s = opencode_settings();
    // The board's lowest effort is `off`; opencode's variant vocabulary spells
    // it `none` (observed in `opencode models --verbose` variant keys). The
    // mapping happens only while building the agent config — the enum stays
    // unchanged, and `--variant` never rides argv (the TUI rejects it).
    s.effort = Some(Effort::Off);
    let inv = build_invocation(
        "opencode",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert!(
        !inv.argv.iter().any(|a| a == "--variant"),
        "the TUI does not accept --variant; it must never appear in argv"
    );
    let (_, raw) = inv
        .env
        .iter()
        .find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
        .unwrap();
    let config: serde_json::Value = serde_json::from_str(raw).unwrap();
    assert_eq!(
        config["agent"]["herdr-board"]["variant"],
        serde_json::json!("none"),
        "board effort off must map to opencode variant none in the agent config"
    );

    // Every other level keeps its canonical spelling.
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
            "opencode",
            &Config::default(),
            &s,
            &SessionPlan::Mint,
            None,
            "t",
        )
        .unwrap();
        let (_, raw) = inv
            .env
            .iter()
            .find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
            .unwrap();
        let config: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            config["agent"]["herdr-board"]["variant"],
            serde_json::json!(spelling),
            "effort {effort:?} must keep its spelling"
        );
        assert!(
            !inv.argv.iter().any(|a| a == "--variant"),
            "effort must ride the agent config, never argv"
        );
    }

    // No effort → no agent config at all, and the model stays `-m`.
    s.effort = None;
    let inv = build_invocation(
        "opencode",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert!(!inv.argv.iter().any(|a| a == "--agent"));
    assert!(
        inv.env.iter().all(|(k, _)| k != "OPENCODE_CONFIG_CONTENT"),
        "no effort → no OPENCODE_CONFIG_CONTENT env"
    );
    assert!(inv.argv.contains(&"-m".to_string()));
}

#[test]
fn opencode_maps_permission_modes_to_exact_argv() {
    // The board-facing opencode permission vocabulary is exactly two modes,
    // each mapping to a verified CLI spelling:
    //   default     → (no flag)
    //   auto-approve → --auto
    let mut s = opencode_settings();
    s.permission_mode = Some("auto-approve".into());
    let inv = build_invocation(
        "opencode",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert!(inv.argv.contains(&"--auto".to_string()));

    s.permission_mode = Some("default".into());
    let inv = build_invocation(
        "opencode",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert!(
        !inv.argv.iter().any(|a| a == "--auto"),
        "default permission is the absence of a flag"
    );

    s.permission_mode = None;
    let inv = build_invocation(
        "opencode",
        &Config::default(),
        &s,
        &SessionPlan::Mint,
        None,
        "t",
    )
    .unwrap();
    assert!(!inv.argv.iter().any(|a| a == "--auto"));
}

#[test]
fn opencode_permission_flag_applies_before_session_flags() {
    // The permission flag sits with the other launch flags; the session
    // syntax remains the LAST argv tokens (the adapter-owned session flags).
    let mut s = opencode_settings();
    s.permission_mode = Some("auto-approve".into());
    let inv = build_invocation(
        "opencode",
        &Config::default(),
        &s,
        &SessionPlan::Fork("ses-7".into()),
        None,
        "t",
    )
    .unwrap();
    let auto_at = inv.argv.iter().position(|a| a == "--auto").unwrap();
    let session_at = inv.argv.len() - 3;
    assert!(
        auto_at < session_at,
        "--auto must precede the session flags; got {:?}",
        inv.argv
    );
    assert!(inv
        .argv
        .ends_with(&["-s".to_string(), "ses-7".to_string(), "--fork".to_string()]));
}

#[test]
fn opencode_startup_argv_contains_no_prompt_or_system_prompt() {
    let prompt = "implement the widget\nand respect --flags";
    let inv = build_invocation(
        "opencode",
        &Config::default(),
        &opencode_settings(),
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
    }
    // The config env carries only the agent's model+variant, never prompt text.
    for (key, value) in &inv.env {
        assert!(
            !value.contains("implement the widget"),
            "env {key} must not embed the task text: {value:?}"
        );
        assert!(
            !value.contains("PLAN stage"),
            "env {key} must not embed the column system prompt: {value:?}"
        );
        assert!(
            !value.contains("herdr-board protocol"),
            "env {key} must not embed the protocol trailer: {value:?}"
        );
    }
    assert!(!inv.argv.iter().any(|a| a == "--"));
    assert!(!inv
        .argv
        .iter()
        .any(|a| a.starts_with("--append-system-prompt")));
    assert!(
        !inv.argv.iter().any(|a| a == "--prompt"),
        "the opencode --prompt flag must never carry the card task in startup argv"
    );
    assert!(
        !inv.argv.iter().any(|a| a == "--variant"),
        "the TUI does not accept --variant; it must never appear in startup argv"
    );
    // Both managed prompt channels stay out of argv and are delivered by the
    // daemon after the agent is interactive (Mint: system_prompt + task).
    assert_eq!(inv.initial_prompt.as_deref(), Some(prompt));
    let expected_system = format!("PLAN stage\n\n{BOARD_PROTOCOL_TRAILER}");
    assert_eq!(inv.system_prompt.as_deref(), Some(expected_system.as_str()));
}

// ---------------------------------------------------------------------------
// O6 — capability catalog (adapter registration, O7 green target)
// ---------------------------------------------------------------------------

#[test]
fn opencode_capability_catalog_shape() {
    let caps = default_capabilities("opencode");
    assert_eq!(caps.harness, "opencode");
    // The static fallback catalog is truthful: `opencode/nemotron-3-ultra-free`
    // declares `variants: {}` for real (verified live), so it is listed with
    // EMPTY efforts — selecting it offers no board effort; the fixture model
    // `opencode/deepseek-v4-flash-free` carries its verified low/high/max
    // variants so model/effort UX stays demonstrable without a live CLI.
    let nemotron = caps
        .models
        .iter()
        .find(|m| m.id == "opencode/nemotron-3-ultra-free")
        .expect("static fallback defines opencode/nemotron-3-ultra-free");
    assert!(
        nemotron.efforts.is_empty(),
        "nemotron-3-ultra-free really has variants {{}} → no board efforts"
    );
    let deepseek = caps
        .models
        .iter()
        .find(|m| m.id == "opencode/deepseek-v4-flash-free")
        .expect("static fallback defines opencode/deepseek-v4-flash-free");
    assert_eq!(
        deepseek.efforts,
        vec![Effort::Low, Effort::High, Effort::Max],
        "the fixture model's verified variants map onto board efforts in canonical order"
    );
    // A known model with empty efforts offers NO effort; an unknown/free-form
    // model keeps the full ladder.
    assert!(efforts_for(&caps, Some("opencode/nemotron-3-ultra-free")).is_empty());
    assert_eq!(
        efforts_for(&caps, Some("opencode/deepseek-v4-flash-free")),
        vec![Effort::Low, Effort::High, Effort::Max]
    );
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
        ],
        "free-form/open-model efforts are the full board ladder"
    );
    assert_eq!(
        caps.permission_modes,
        vec!["default", "auto-approve"],
        "the two verified permission modes; nothing else is board-facing"
    );
    assert_eq!(caps.resume, ResumeSupport::ByConversationId);
    // Another CLI's permission vocabulary never leaks in.
    for word in [
        "acceptEdits",
        "bypassPermissions",
        "ask-for-approval",
        "--auto",
    ] {
        assert!(
            !caps.permission_modes.iter().any(|p| p == word),
            "{word} must not appear as an opencode permission mode"
        );
    }
}

#[test]
fn opencode_resolves_through_meta_and_resume_support() {
    let cfg = Config::default();
    let meta = meta_for("opencode", &cfg).unwrap();
    assert_eq!(meta.id(), "opencode");
    assert!(meta.model_freeform());
    assert_eq!(
        meta.permissions(),
        vec!["default".to_string(), "auto-approve".to_string()]
    );
    assert_eq!(meta.resume(), ResumeSupport::ByConversationId);
    // The trait answer and the wire snapshot never disagree.
    let caps = capabilities_for("opencode", &cfg).unwrap();
    assert_eq!(caps, HarnessCapabilities::from_meta(meta.as_ref()));
    assert_eq!(
        resume_support_for("opencode", &cfg),
        ResumeSupport::ByConversationId
    );
}

// ---------------------------------------------------------------------------
// O6 — rescue re-threading (dead-pane rescue never re-sends the task)
// ---------------------------------------------------------------------------

/// The startup-only argv form an opencode mint persists (after O7): no session
/// tokens, no prompt text; the effort rides the `OPENCODE_CONFIG_CONTENT` env
/// instead of a `--variant` argv flag the TUI rejects.
fn persisted_opencode_mint() -> ExecutionSpec {
    ExecutionSpec {
        argv: vec![
            "opencode".into(),
            "--agent".into(),
            "herdr-board".into(),
            "--auto".into(),
        ],
        env: vec![
            (
                "OPENCODE_CONFIG_CONTENT".into(),
                "{\"agent\":{\"herdr-board\":{\"model\":\"opencode/recorded-model\",\"variant\":\"low\"}}}"
                    .into(),
            ),
            ("RECORDED_ENV".into(), "recorded-value".into()),
        ],
        agent_kind: Some("opencode".into()),
        initial_prompt: Some("the original task".into()),
        system_prompt: Some("recorded system prompt".into()),
    }
}

#[test]
fn opencode_mint_prompt_block_is_a_single_delimited_system_then_task_block() {
    use board_core::harness::opencode::{mint_prompt, MINT_SYSTEM_DELIMITER, MINT_TASK_DELIMITER};

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
fn opencode_resume_invocation_rethreads_mint_argv_to_session_flag() {
    let spec = resume_invocation(
        "opencode",
        ResumeSupport::ByConversationId,
        &persisted_opencode_mint(),
        "ses-9",
    )
    .unwrap();
    assert_eq!(
        spec.argv,
        vec![
            "opencode",
            "--agent",
            "herdr-board",
            "--auto",
            "-s",
            "ses-9",
        ]
    );
    // Rescue never re-sends the task, and never re-runs it.
    assert_eq!(spec.initial_prompt, None);
    assert!(!spec.env.iter().any(|(k, _)| k == "BOARD_PROMPT"));
    assert!(!spec.argv.iter().any(|a| a.contains("the original task")));
    // The rest of the recorded execution environment is preserved verbatim —
    // including the OPENCODE_CONFIG_CONTENT env that carries the effort, so a
    // rescued pane keeps the same model/effort it ran with.
    assert!(spec.env.contains(&(
        "OPENCODE_CONFIG_CONTENT".to_string(),
        "{\"agent\":{\"herdr-board\":{\"model\":\"opencode/recorded-model\",\"variant\":\"low\"}}}"
            .to_string()
    )));
    assert!(spec
        .env
        .contains(&("RECORDED_ENV".to_string(), "recorded-value".to_string())));
    assert_eq!(
        spec.system_prompt.as_deref(),
        Some("recorded system prompt")
    );
    assert_eq!(spec.agent_kind.as_deref(), Some("opencode"));
    assert!(spec
        .env
        .contains(&(BOARD_RESCUE.to_string(), "1".to_string())));
}

#[test]
fn opencode_resume_invocation_strips_persisted_fork_and_old_session() {
    let mut spec = persisted_opencode_mint();
    spec.argv
        .extend(["-s".into(), "ses-1".into(), "--fork".into()]);
    let resumed =
        resume_invocation("opencode", ResumeSupport::ByConversationId, &spec, "ses-9").unwrap();
    // The persisted fork flag and the old session id are stripped, not
    // duplicated.
    assert!(!resumed.argv.contains(&"--fork".to_string()));
    assert!(!resumed.argv.contains(&"ses-1".to_string()));
    assert!(resumed
        .argv
        .ends_with(&["-s".to_string(), "ses-9".to_string()]));
}

#[test]
fn opencode_resume_invocation_refuses_legacy_all_in_one_argv() {
    // A `--` delimiter means the persisted argv embeds the task text, so
    // appending `-s <id>` would re-send it. Refuse instead of guessing.
    let mut spec = persisted_opencode_mint();
    spec.argv.extend(["--".into(), "the original task".into()]);
    assert!(spec.argv.contains(&"--".to_string()));
    let err =
        resume_invocation("opencode", ResumeSupport::ByConversationId, &spec, "ses-9").unwrap_err();
    assert_eq!(err, HarnessError::ResumeLegacyArgv("opencode".into()));
}
