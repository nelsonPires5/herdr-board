//! Unit tests for the launch-plan argv fork detection.

use super::*;

fn argv(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|s| s.to_string()).collect()
}

#[test]
fn opencode_fork_is_the_exact_trailing_session_shape() {
    // `-s <id> --fork` closes the argv: the fork spelling is the trailing
    // three-token session shape, exactly what the adapter appends last.
    assert!(argv_is_fork(
        "opencode",
        &argv(&[
            "opencode",
            "--agent",
            "herdr-board",
            "--auto",
            "-s",
            "ses-1",
            "--fork"
        ])
    ));
    assert!(argv_is_fork(
        "opencode",
        &argv(&["opencode", "-m", "a/b", "--auto", "-s", "ses-1", "--fork"])
    ));
}

#[test]
fn opencode_resume_and_mint_are_not_forks() {
    // Resume closes the argv at `-s <id>` — no `--fork`.
    assert!(!argv_is_fork(
        "opencode",
        &argv(&[
            "opencode",
            "--agent",
            "herdr-board",
            "--auto",
            "-s",
            "ses-1"
        ])
    ));
    // A Mint carries no session flags at all.
    assert!(!argv_is_fork(
        "opencode",
        &argv(&["opencode", "--agent", "herdr-board", "--auto"])
    ));
    assert!(!argv_is_fork("opencode", &argv(&["opencode"])));
}

#[test]
fn opencode_model_literally_spelled_fork_is_not_misclassified() {
    // OpenCode models are free-form, so a no-effort `-m` value could literally
    // be `--fork` (or `-s`) on a Mint; the trailing-window check must not treat
    // it as a fork hop. After the model value a board argv only ever appends
    // `--auto`, never a `-s <id> --fork` tail.
    assert!(!argv_is_fork(
        "opencode",
        &argv(&["opencode", "-m", "--fork"])
    ));
    assert!(!argv_is_fork(
        "opencode",
        &argv(&["opencode", "-m", "--fork", "--auto"])
    ));
    assert!(!argv_is_fork(
        "opencode",
        &argv(&["opencode", "-m", "-s", "--auto"])
    ));
    // A `--fork` that does not close the argv is not the fork spelling.
    assert!(!argv_is_fork(
        "opencode",
        &argv(&["opencode", "-m", "a", "--fork", "--auto"])
    ));
}

#[test]
fn other_harness_fork_spellings_keep_their_shapes() {
    assert!(argv_is_fork(
        "pi",
        &argv(&["pi", "--fork", "source", "--session-id", "t"])
    ));
    assert!(!argv_is_fork("pi", &argv(&["pi", "--session-id", "t"])));
    assert!(argv_is_fork(
        "claude",
        &argv(&["claude", "--resume", "s", "--fork-session"])
    ));
    assert!(!argv_is_fork("claude", &argv(&["claude", "--resume", "s"])));
    assert!(argv_is_fork("codex", &argv(&["codex", "fork", "t"])));
    assert!(!argv_is_fork("codex", &argv(&["codex", "resume", "t"])));
    assert!(!argv_is_fork("unknown", &argv(&["any", "--fork"])));
}

#[test]
fn column_override_resolves_the_exact_configured_argv() {
    // Live e2e/14-column-config.sh keeps only the actual-runner argv
    // recording (the spawned process logging what it really received) plus
    // the stored-vs-actual comparison; the resolution half of that contract
    // lives here: a column harness_override with effort/permission overrides
    // must produce the exact template argv, with the unset `{model}` element
    // dropped and no placeholder left unsubstituted.
    let mut config = Config::default();
    config.harness.insert(
        "fake-ov".into(),
        board_core::config::HarnessDef {
            argv: vec![
                "env".into(),
                "BOARD_BIN=/bin/board".into(),
                "bash".into(),
                "/fake-agent".into(),
                "{model}".into(),
                "{effort}".into(),
                "{permission_mode}".into(),
            ],
            efforts: vec!["low".into()],
            permission_modes: vec!["auto".into()],
            ..Default::default()
        },
    );
    let (d, _, _) = test_daemon_with_config(Arc::new(MissingPiSpawner), config);
    let (card_id, column_id) = {
        let db = d.store.lock();
        let column = db
            .create_column(&ColumnCreateParams {
                name: "Override Execute".into(),
                trigger: Some(Trigger::Auto),
                system_prompt: Some("COLCFG E2E SYSTEM".into()),
                harness_override: Some("fake-ov".into()),
                effort_override: Some("low".into()),
                permission_override: Some("auto".into()),
                ..Default::default()
            })
            .unwrap();
        // The card carries no harness of its own: the column override alone
        // must drive the run, exactly as the live scenario dispatches it.
        let card = db
            .create_card(&CardCreateParams {
                column_id: Some(column.id),
                title: "Override Column Card".into(),
                description: Some("run via the column harness_override".into()),
                ..Default::default()
            })
            .unwrap();
        (card.id, column.id)
    };

    let run = enqueue_run(&d, card_id, column_id, false).unwrap();
    assert_eq!(run.harness, "fake-ov");
    let argv: Vec<String> = serde_json::from_str(&run.argv_json).unwrap();
    assert_eq!(
        argv,
        vec![
            "env".to_string(),
            "BOARD_BIN=/bin/board".to_string(),
            "bash".to_string(),
            "/fake-agent".to_string(),
            "low".to_string(),
            "auto".to_string(),
        ],
        "unset {{model}} drops its element; effort/permission resolve exactly"
    );
    assert!(
        !argv.iter().any(|a| a.contains('{') || a.contains('}')),
        "no placeholder may survive substitution: {argv:?}"
    );
    // The persisted launch spec carries the identical argv the spawner uses.
    let spec = run.launch_spec.as_ref().expect("v11 launch spec");
    assert_eq!(
        &serde_json::from_str::<Vec<String>>(&run.argv_json).unwrap(),
        &spec.execution().argv
    );
}
