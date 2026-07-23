use super::*;

#[test]
fn local_materializer_preserves_pi_historic_prompt_flag_and_card_argument() {
    let argv = materialize_local_argv(&managed_req("pi")).unwrap();
    assert_eq!(
        argv,
        vec![
            "pi",
            "--model",
            "m",
            "--append-system-prompt",
            "old system\nsecond line",
            "--session-id",
            "s",
            "Card task:\nexact task",
        ]
    );
}

#[test]
fn local_materializer_preserves_claude_flag_order_and_final_prompt() {
    let argv = materialize_local_argv(&managed_req("claude")).unwrap();
    assert_eq!(
        argv,
        vec![
            "claude",
            "--model",
            "m",
            "--append-system-prompt",
            "old system\nsecond line",
            "--allowedTools",
            "Bash(*)",
            "--",
            "exact task",
        ]
    );
}

#[test]
fn local_materializer_leaves_configured_argv_untouched() {
    let mut req = managed_req("custom");
    req.agent_kind = None;
    req.initial_prompt = None;
    req.system_prompt = None;
    req.argv = vec!["configured".into(), "literal\nargument".into()];
    assert_eq!(materialize_local_argv(&req).unwrap(), req.argv);
}

#[test]
fn local_materializer_rejects_incomplete_or_unknown_managed_metadata() {
    let mut missing = managed_req("pi");
    missing.system_prompt = None;
    let err = materialize_local_argv(&missing).unwrap_err();
    assert!(err.to_string().contains("system_prompt"));

    let err = materialize_local_argv(&managed_req("other")).unwrap_err();
    assert!(err.to_string().contains("managed") || err.to_string().contains("harness"));
}
