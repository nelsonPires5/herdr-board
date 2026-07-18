//! Harness argv/env building + session planning tests.

use board_core::config::{Config, HarnessDef};
use board_core::harness::{
    build_invocation, claude_argv, is_builtin_harness, pi_argv, plan_session, HarnessError,
    SessionPlan, BOARD_PROTOCOL_TRAILER, DEFAULT_HARNESS,
};
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
    assert!(inv
        .env
        .contains(&("BOARD_SYSTEM_PROMPT".into(), "PLAN stage".into())));
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
