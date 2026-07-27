//! Harness argv/env building + session planning tests.

use board_core::capability::ResumeSupport;
use board_core::config::{Config, HarnessDef};
use board_core::harness::{
    build_invocation, claude_argv, is_builtin_harness, pi_argv, plan_session, resume_invocation,
    session_argv, HarnessError, SessionPlan, BOARD_PROTOCOL_TRAILER, DEFAULT_HARNESS,
};
use board_core::launch::ExecutionSpec;
use board_core::prompt::EffectiveSettings;
use board_core::protocol::Effort;

fn settings() -> EffectiveSettings {
    EffectiveSettings {
        harness: "claude".into(),
        model: Some("sonnet".into()),
        effort: Some(Effort::High),
        permission_mode: Some("acceptEdits".into()),
        system_prompt: Some("PLAN stage".into()),
        fresh_session: false,
        timeout_minutes: None,
    }
}

fn pi_settings() -> EffectiveSettings {
    EffectiveSettings {
        harness: "pi".into(),
        model: Some("openai-codex/example".into()),
        effort: Some(Effort::Low),
        permission_mode: None,
        system_prompt: Some("EXECUTE stage".into()),
        fresh_session: false,
        timeout_minutes: None,
    }
}

#[test]
fn builtin_registry_is_pi_first() {
    assert_eq!(DEFAULT_HARNESS, "pi");
    assert!(is_builtin_harness("pi"));
    assert!(is_builtin_harness("claude"));
    assert!(!is_builtin_harness("fake"));
}

#[test]
fn session_planning() {
    // No prior session → mint.
    assert_eq!(plan_session(None, false, false), SessionPlan::Mint);
    // Normal continuation → resume.
    assert_eq!(
        plan_session(Some("s1"), false, false),
        SessionPlan::Resume("s1".into())
    );
    // Retry → fork.
    assert_eq!(
        plan_session(Some("s1"), false, true),
        SessionPlan::Fork("s1".into())
    );
    // Forced fresh column → mint even with a session.
    assert_eq!(plan_session(Some("s1"), true, false), SessionPlan::Mint);
}

#[test]
fn claude_fresh_session_mints_uuid() {
    let uuid = "11111111-1111-4111-8111-111111111111";
    let argv = claude_argv(&settings(), &SessionPlan::Mint, Some(uuid), "prompt text").unwrap();
    assert_eq!(
        argv,
        vec![
            "claude",
            "--model",
            "sonnet",
            "--effort",
            "high",
            "--permission-mode",
            "acceptEdits",
            "--append-system-prompt",
            &format!("PLAN stage\n\n{BOARD_PROTOCOL_TRAILER}"),
            "--allowedTools",
            "Bash(board:*)",
            "--session-id",
            uuid,
            "--",
            "prompt text",
        ]
    );
}

#[test]
fn claude_mint_without_uuid_errors() {
    let err = claude_argv(&settings(), &SessionPlan::Mint, None, "p").unwrap_err();
    assert_eq!(err, HarnessError::MissingMintedSession);
}

#[test]
fn claude_resume() {
    let argv = claude_argv(&settings(), &SessionPlan::Resume("abc".into()), None, "p").unwrap();
    assert!(argv.windows(2).any(|w| w == ["--resume", "abc"]));
    assert!(!argv.iter().any(|a| a == "--fork-session"));
    assert!(!argv.iter().any(|a| a == "--session-id"));
}

#[test]
fn claude_fork_on_retry() {
    let argv = claude_argv(&settings(), &SessionPlan::Fork("abc".into()), None, "p").unwrap();
    assert!(argv.windows(2).any(|w| w == ["--resume", "abc"]));
    assert!(argv.iter().any(|a| a == "--fork-session"));
}

#[test]
fn claude_omits_unset_overrides() {
    let s = EffectiveSettings {
        harness: "claude".into(),
        model: None,
        effort: None,
        permission_mode: None,
        system_prompt: None,
        fresh_session: false,
        timeout_minutes: None,
    };
    let argv = claude_argv(&s, &SessionPlan::Resume("x".into()), None, "p").unwrap();
    // No column prompt → the system prompt is exactly the protocol trailer.
    assert_eq!(
        argv,
        vec![
            "claude",
            "--append-system-prompt",
            BOARD_PROTOCOL_TRAILER,
            "--allowedTools",
            "Bash(board:*)",
            "--resume",
            "x",
            "--",
            "p",
        ]
    );
}

#[test]
fn claude_bypass_permission_is_allowed_when_card_set() {
    // The refusal is enforced at settings resolution; if permission_mode is set
    // (by the card), argv building carries it through verbatim.
    let mut s = settings();
    s.permission_mode = Some("bypassPermissions".into());
    let argv = claude_argv(&s, &SessionPlan::Mint, Some("u"), "p").unwrap();
    assert!(argv
        .windows(2)
        .any(|w| w == ["--permission-mode", "bypassPermissions"]));
}

#[test]
fn pi_mint_argv_uses_exact_session_id() {
    let target = "11111111-1111-4111-8111-111111111111";
    let inv = pi_argv(&pi_settings(), &SessionPlan::Mint, Some(target), "write it").unwrap();
    assert!(inv.argv.windows(2).any(|w| w == ["--session-id", target]));
    assert_eq!(inv.resulting_session_id.as_deref(), Some(target));
}

#[test]
fn pi_resume_argv_uses_exact_session_id() {
    let inv = pi_argv(
        &pi_settings(),
        &SessionPlan::Resume("existing-id".into()),
        None,
        "continue",
    )
    .unwrap();
    assert!(inv
        .argv
        .windows(2)
        .any(|w| w == ["--session-id", "existing-id"]));
    assert!(!inv.argv.iter().any(|a| a == "--fork"));
    assert_eq!(inv.resulting_session_id.as_deref(), Some("existing-id"));
}

#[test]
fn pi_retry_forks_to_new_session_id() {
    let target = "22222222-2222-4222-8222-222222222222";
    let inv = pi_argv(
        &pi_settings(),
        &SessionPlan::Fork("source-id".into()),
        Some(target),
        "retry",
    )
    .unwrap();
    assert!(inv.argv.windows(2).any(|w| w == ["--fork", "source-id"]));
    assert!(inv.argv.windows(2).any(|w| w == ["--session-id", target]));
    assert_eq!(inv.resulting_session_id.as_deref(), Some(target));
}

#[test]
fn pi_maps_effort_to_thinking_and_model() {
    let inv = pi_argv(
        &pi_settings(),
        &SessionPlan::Resume("s".into()),
        None,
        "task",
    )
    .unwrap();
    assert!(inv
        .argv
        .windows(2)
        .any(|w| w == ["--model", "openai-codex/example"]));
    assert!(inv.argv.windows(2).any(|w| w == ["--thinking", "low"]));
}

#[test]
fn pi_omits_unset_model_and_thinking() {
    let mut s = pi_settings();
    s.model = None;
    s.effort = None;
    let inv = pi_argv(&s, &SessionPlan::Resume("s".into()), None, "task").unwrap();
    assert!(!inv.argv.iter().any(|a| a == "--model"));
    assert!(!inv.argv.iter().any(|a| a == "--thinking"));
}

#[test]
fn pi_appends_board_protocol_trailer() {
    let inv = pi_argv(
        &pi_settings(),
        &SessionPlan::Resume("s".into()),
        None,
        "task",
    )
    .unwrap();
    let system = inv
        .argv
        .windows(2)
        .find(|w| w[0] == "--append-system-prompt")
        .map(|w| w[1].as_str())
        .unwrap();
    assert_eq!(system, format!("EXECUTE stage\n\n{BOARD_PROTOCOL_TRAILER}"));
}

#[test]
fn pi_prompt_cannot_be_parsed_as_a_flag() {
    let inv = pi_argv(
        &pi_settings(),
        &SessionPlan::Resume("s".into()),
        None,
        "--version",
    )
    .unwrap();
    assert!(!inv.argv.iter().any(|a| a == "--"));
    assert_eq!(inv.argv.last().unwrap(), "Card task:\n--version");
}

#[test]
fn pi_rejects_explicit_permission_mode() {
    let mut s = pi_settings();
    s.permission_mode = Some("acceptEdits".into());
    let err = pi_argv(&s, &SessionPlan::Mint, Some("target"), "task").unwrap_err();
    assert_eq!(err.to_string(), "pi does not support permission modes");
}

#[test]
fn build_invocation_routes_pi_without_config() {
    let inv = build_invocation(
        "pi",
        &Config::default(),
        &pi_settings(),
        &SessionPlan::Mint,
        Some("target"),
        "task",
    )
    .unwrap();
    assert_eq!(inv.argv.first().map(String::as_str), Some("pi"));
    assert!(inv.env.is_empty());
    assert_eq!(inv.resulting_session_id.as_deref(), Some("target"));
}

#[test]
fn custom_harness_uses_template_and_env() {
    let mut config = Config::default();
    config.harness.insert(
        "fake".into(),
        HarnessDef {
            argv: vec![
                "bash".into(),
                "/tmp/fake.sh".into(),
                "{model}".into(),
                "{effort}".into(),
            ],
            ..Default::default()
        },
    );
    let inv = build_invocation(
        "fake",
        &config,
        &settings(),
        &SessionPlan::Mint,
        None,
        "the prompt",
    )
    .unwrap();
    assert_eq!(inv.argv, vec!["bash", "/tmp/fake.sh", "sonnet", "high"]);
    assert!(inv
        .env
        .contains(&("BOARD_PROMPT".into(), "the prompt".into())));
    // The protocol trailer rides BOARD_SYSTEM_PROMPT even for custom harnesses.
    assert!(inv.env.contains(&(
        "BOARD_SYSTEM_PROMPT".into(),
        format!("PLAN stage\n\n{BOARD_PROTOCOL_TRAILER}")
    )));
}

#[test]
fn custom_harness_without_column_prompt_still_gets_the_protocol_trailer() {
    let mut config = Config::default();
    config.harness.insert(
        "fake".into(),
        HarnessDef {
            argv: vec!["run".into()],
            ..Default::default()
        },
    );
    let no_prompt = EffectiveSettings {
        system_prompt: None,
        ..settings()
    };
    let inv = build_invocation("fake", &config, &no_prompt, &SessionPlan::Mint, None, "p").unwrap();
    assert!(inv.env.contains(&(
        "BOARD_SYSTEM_PROMPT".into(),
        BOARD_PROTOCOL_TRAILER.to_string()
    )));
}

#[test]
fn custom_harness_drops_unset_placeholders() {
    let mut config = Config::default();
    config.harness.insert(
        "fake".into(),
        HarnessDef {
            argv: vec!["run".into(), "{model}".into(), "{permission_mode}".into()],
            ..Default::default()
        },
    );
    let mut s = settings();
    s.permission_mode = None; // unset → its element is dropped
    let inv = build_invocation("fake", &config, &s, &SessionPlan::Mint, None, "p").unwrap();
    assert_eq!(inv.argv, vec!["run", "sonnet"]);
}

#[test]
fn unknown_harness_errors() {
    let config = Config::default();
    let err = build_invocation(
        "nope",
        &config,
        &settings(),
        &SessionPlan::Mint,
        Some("u"),
        "p",
    )
    .unwrap_err();
    assert_eq!(err, HarnessError::UnknownHarness("nope".into()));
}

// ---------------------------------------------------------------------------
// Resume launches (the dead-pane rescue)
// ---------------------------------------------------------------------------

fn persisted(harness: &str, session_flag: &str) -> ExecutionSpec {
    ExecutionSpec {
        argv: vec![
            harness.into(),
            "--model".into(),
            "recorded-model".into(),
            session_flag.into(),
            "old-conversation".into(),
        ],
        env: vec![
            ("BOARD_PROMPT".into(), "the original task".into()),
            ("RECORDED_ENV".into(), "recorded-value".into()),
        ],
        agent_kind: Some(harness.into()),
        initial_prompt: Some("the original task".into()),
        system_prompt: Some("recorded system prompt".into()),
    }
}

#[test]
fn session_argv_owns_each_harness_resume_syntax() {
    // Resume is spelled differently per harness and there is exactly one place
    // that knows how.
    assert_eq!(
        session_argv("claude", &SessionPlan::Resume("c1".into()), None).unwrap(),
        (
            vec!["--resume".to_string(), "c1".to_string()],
            Some("c1".to_string())
        )
    );
    assert_eq!(
        session_argv("pi", &SessionPlan::Resume("c1".into()), None).unwrap(),
        (
            vec!["--session-id".to_string(), "c1".to_string()],
            Some("c1".to_string())
        )
    );
    assert_eq!(
        session_argv("nope", &SessionPlan::Resume("c1".into()), None).unwrap_err(),
        HarnessError::UnknownHarness("nope".into())
    );
}

#[test]
fn resume_invocation_rethreads_the_persisted_argv_per_harness() {
    // claude: the persisted `--resume <old>` is replaced, not duplicated.
    let spec = resume_invocation(
        "claude",
        ResumeSupport::ByConversationId,
        &persisted("claude", "--resume"),
        "conv-9",
    )
    .unwrap();
    assert_eq!(
        spec.argv,
        vec!["claude", "--model", "recorded-model", "--resume", "conv-9"]
    );
    assert!(!spec.argv.contains(&"old-conversation".to_string()));

    // pi re-uses `--session-id` for resuming, and the persisted one is replaced.
    let spec = resume_invocation(
        "pi",
        ResumeSupport::ByConversationId,
        &persisted("pi", "--session-id"),
        "conv-9",
    )
    .unwrap();
    assert_eq!(
        spec.argv,
        vec!["pi", "--model", "recorded-model", "--session-id", "conv-9"]
    );

    // A persisted fork is also re-threaded to a plain resume.
    let mut forked = persisted("claude", "--resume");
    forked.argv.push("--fork-session".into());
    let spec =
        resume_invocation("claude", ResumeSupport::ByConversationId, &forked, "conv-9").unwrap();
    assert_eq!(
        spec.argv,
        vec!["claude", "--model", "recorded-model", "--resume", "conv-9"]
    );
}

#[test]
fn resume_invocation_never_re_sends_the_original_prompt() {
    let spec = resume_invocation(
        "claude",
        ResumeSupport::ByConversationId,
        &persisted("claude", "--resume"),
        "conv-9",
    )
    .unwrap();
    // Both prompt channels are silenced: the managed prompt submission and the
    // configured `BOARD_PROMPT` env. Resuming must continue the conversation,
    // never re-run the task.
    assert_eq!(spec.initial_prompt, None);
    assert!(!spec.env.iter().any(|(k, _)| k == "BOARD_PROMPT"));
    assert!(!spec.argv.iter().any(|a| a.contains("the original task")));
    // The rest of the recorded execution environment is preserved verbatim, so
    // a rescue cannot silently switch model/effort/env after a config edit.
    assert!(spec
        .env
        .contains(&("RECORDED_ENV".to_string(), "recorded-value".to_string())));
    assert_eq!(
        spec.system_prompt.as_deref(),
        Some("recorded system prompt")
    );
    assert_eq!(spec.agent_kind.as_deref(), Some("claude"));
    // The pane is marked as a rescue and carries the conversation id for
    // configured harnesses that opted in.
    assert!(spec
        .env
        .contains(&("BOARD_RESCUE".to_string(), "1".to_string())));
    assert!(spec
        .env
        .contains(&("BOARD_RESUME_SESSION_ID".to_string(), "conv-9".to_string())));
}

#[test]
fn resume_invocation_runs_a_configured_harness_argv_unchanged() {
    // A configured argv is persisted fully materialized, so there is nothing to
    // re-thread: the id travels in the environment instead.
    let mut spec = persisted("custom", "--whatever");
    spec.agent_kind = None;
    let resumed =
        resume_invocation("custom", ResumeSupport::ByConversationId, &spec, "conv-9").unwrap();
    assert_eq!(resumed.argv, spec.argv);
    assert_eq!(resumed.agent_kind, None);
    assert!(resumed
        .env
        .contains(&("BOARD_RESUME_SESSION_ID".to_string(), "conv-9".to_string())));
}

#[test]
fn resume_invocation_refuses_a_legacy_all_in_one_command_line() {
    // The legacy helpers end in `-- "<prompt>"` (claude) or a bare positional
    // (pi). Appending resume flags to that would emit
    // `… -- "<prompt>" --resume <id>` — re-running the task AND feeding
    // `--resume` to the harness as prompt text. Refuse instead of rewriting.
    let legacy = ExecutionSpec {
        argv: claude_argv(
            &settings(),
            &SessionPlan::Resume("old".into()),
            None,
            "the original task",
        )
        .unwrap(),
        env: vec![],
        agent_kind: Some("claude".into()),
        initial_prompt: Some("the original task".into()),
        system_prompt: Some("s".into()),
    };
    assert!(legacy.argv.contains(&"--".to_string()));
    assert_eq!(
        resume_invocation("claude", ResumeSupport::ByConversationId, &legacy, "conv-9")
            .unwrap_err(),
        HarnessError::ResumeLegacyArgv("claude".into())
    );
    // The protocol-17 form `build_invocation` actually persists is accepted.
    let managed = build_invocation(
        "claude",
        &Config::default(),
        &settings(),
        &SessionPlan::Mint,
        Some("minted"),
        "the original task",
    )
    .unwrap();
    let spec = ExecutionSpec {
        argv: managed.argv,
        env: managed.env,
        agent_kind: managed.agent_kind,
        initial_prompt: managed.initial_prompt,
        system_prompt: managed.system_prompt,
    };
    let resumed =
        resume_invocation("claude", ResumeSupport::ByConversationId, &spec, "conv-9").unwrap();
    assert!(resumed
        .argv
        .ends_with(&["--resume".to_string(), "conv-9".to_string()]));
    assert!(!resumed.argv.contains(&"minted".to_string()));
}

#[test]
fn resume_invocation_refuses_without_capability_or_conversation_id() {
    let spec = persisted("custom", "--whatever");
    assert_eq!(
        resume_invocation("custom", ResumeSupport::Unsupported, &spec, "conv-9").unwrap_err(),
        HarnessError::ResumeUnsupported("custom".into())
    );
    for blank in ["", "   "] {
        assert_eq!(
            resume_invocation("claude", ResumeSupport::ByConversationId, &spec, blank).unwrap_err(),
            HarnessError::MissingResumeSession
        );
    }
}
