//! Protocol-17 launch metadata contracts.
//!
//! Managed agents expose the authoritative system instructions separately from
//! the card task. The daemon materializes those instructions as a startup-only
//! file; only the card task is later submitted through `agent.prompt`.
//! Configured commands remain unmanaged even when their executable happens to
//! be named like a built-in harness.

use board_core::config::{Config, HarnessDef};
use board_core::harness::{
    build_invocation, protocol_system_prompt, SessionPlan, BOARD_PROTOCOL_TRAILER,
};
use board_core::prompt::EffectiveSettings;
use board_core::protocol::Effort;

fn settings(harness: &str) -> EffectiveSettings {
    EffectiveSettings {
        harness: harness.into(),
        model: Some("provider/model with space".into()),
        effort: Some(Effort::Low),
        permission_mode: None,
        system_prompt: Some("Review carefully".into()),
        fresh_session: false,
        timeout_minutes: None,
    }
}

#[test]
fn pi_invocation_preserves_authoritative_system_prompt_separately_from_card_prompt() {
    let prompt = "first task line\nsecond task line with spaces";
    let inv = build_invocation(
        "pi",
        &Config::default(),
        &settings("pi"),
        &SessionPlan::Resume("pi-session".into()),
        None,
        prompt,
    )
    .unwrap();

    assert_eq!(inv.agent_kind.as_deref(), Some("pi"));
    assert_eq!(inv.initial_prompt.as_deref(), Some(prompt));
    let expected_system = protocol_system_prompt(Some("Review carefully"));
    assert_eq!(inv.system_prompt.as_deref(), Some(expected_system.as_str()));
    assert_eq!(
        inv.argv,
        vec![
            "pi".to_string(),
            "--model".into(),
            "provider/model with space".into(),
            "--thinking".into(),
            "low".into(),
            "--session-id".into(),
            "pi-session".into(),
        ],
        "the daemon adds --append-system-prompt plus its temporary file path; neither authoritative text nor card text belongs in the base startup argv",
    );
}

#[test]
fn claude_invocation_preserves_authoritative_system_prompt_and_exact_startup_tail() {
    let prompt = "inspect --all\nthen report";
    let mut claude_settings = settings("claude");
    claude_settings.permission_mode = Some("acceptEdits".into());
    let inv = build_invocation(
        "claude",
        &Config::default(),
        &claude_settings,
        &SessionPlan::Fork("claude-source".into()),
        None,
        prompt,
    )
    .unwrap();

    assert_eq!(inv.agent_kind.as_deref(), Some("claude"));
    assert_eq!(inv.initial_prompt.as_deref(), Some(prompt));
    let expected_system = protocol_system_prompt(Some("Review carefully"));
    assert_eq!(inv.system_prompt.as_deref(), Some(expected_system.as_str()));
    assert_eq!(
        inv.argv,
        vec![
            "claude".to_string(),
            "--model".into(),
            "provider/model with space".into(),
            "--effort".into(),
            "low".into(),
            "--permission-mode".into(),
            "acceptEdits".into(),
            "--allowedTools".into(),
            "Bash(board:*)".into(),
            "--resume".into(),
            "claude-source".into(),
            "--fork-session".into(),
        ],
        "the daemon preserves this argv[1..] tail, then adds --append-system-prompt-file plus its temporary path; only the card task is submitted later",
    );
}

#[test]
fn configured_invocation_is_explicitly_unmanaged_even_when_argv_starts_with_pi() {
    let mut config = Config::default();
    config.harness.insert(
        "custom".into(),
        HarnessDef {
            argv: vec![
                "pi".into(),
                "--literal=space value".into(),
                "single'quote".into(),
                "tail\nline".into(),
            ],
            ..Default::default()
        },
    );
    let prompt = "custom task line one\ncustom task line two";
    let inv = build_invocation(
        "custom",
        &config,
        &settings("custom"),
        &SessionPlan::Mint,
        None,
        prompt,
    )
    .unwrap();

    assert_eq!(
        inv.agent_kind, None,
        "managed kind must never be inferred from argv[0]"
    );
    assert_eq!(
        inv.initial_prompt, None,
        "configured harnesses consume BOARD_PROMPT"
    );
    assert_eq!(
        inv.system_prompt, None,
        "configured harnesses consume BOARD_SYSTEM_PROMPT"
    );
    assert_eq!(
        inv.argv,
        vec!["pi", "--literal=space value", "single'quote", "tail\nline"],
    );
    let expected_system = format!("Review carefully\n\n{BOARD_PROTOCOL_TRAILER}");
    assert_eq!(
        inv.env,
        vec![
            ("BOARD_PROMPT".to_string(), prompt.to_string()),
            ("BOARD_SYSTEM_PROMPT".to_string(), expected_system),
        ],
        "configured harnesses receive the exact multiline card task and authoritative system text via distinct environment values",
    );
}
