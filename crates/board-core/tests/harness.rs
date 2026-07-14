//! Harness argv/env building + session planning tests.

use board_core::config::{Config, HarnessDef};
use board_core::harness::{
    build_invocation, claude_argv, plan_session, HarnessError, SessionPlan, BOARD_PROTOCOL_TRAILER,
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
