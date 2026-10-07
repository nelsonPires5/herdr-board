//! Config defaults + parsing.

use board_core::config::{Config, DaemonConfig, RootConfig, SpawnerKind};
use board_core::Error;

#[test]
fn defaults_when_empty() {
    let c = Config::default();
    assert_eq!(c.max_concurrent, 3);
    assert_eq!(c.idle_grace_seconds, 90);
    assert!(c.harness.is_empty());
}

#[test]
fn missing_file_is_defaults() {
    let path = std::path::Path::new("/nonexistent/herdr-board/config.toml");
    let c = Config::load_from(path).unwrap();
    assert_eq!(c, Config::default());
}

#[test]
fn parse_full_config() {
    let toml = r#"
max_concurrent = 5
idle_grace_seconds = 120

[harness.fake]
argv = ["bash", "/path/to/fake-agent.sh"]
"#;
    let c = Config::from_toml(toml).unwrap();
    assert_eq!(c.max_concurrent, 5);
    assert_eq!(c.idle_grace_seconds, 120);
    let fake = c.harness.get("fake").unwrap();
    assert_eq!(fake.argv, vec!["bash", "/path/to/fake-agent.sh"]);
    // Capability fields default empty when the pre-existing `argv`-only form is used.
    assert!(fake.models.is_empty());
    assert!(fake.efforts.is_empty());
    assert!(fake.permission_modes.is_empty());
}

#[test]
fn harness_capability_fields_parse() {
    let toml = r#"
[harness.custom]
argv = ["run", "{model}"]
models = ["big", "small"]
efforts = ["low", "high"]
permission_modes = ["auto"]
"#;
    let c = Config::from_toml(toml).unwrap();
    let h = c.harness.get("custom").unwrap();
    assert_eq!(h.models, vec!["big", "small"]);
    assert_eq!(h.efforts, vec!["low", "high"]);
    assert_eq!(h.permission_modes, vec!["auto"]);
}

#[test]
fn partial_config_keeps_defaults() {
    let c = Config::from_toml("max_concurrent = 7\n").unwrap();
    assert_eq!(c.max_concurrent, 7);
    assert_eq!(c.idle_grace_seconds, 90);
}

#[test]
fn root_config_parses_board_harness_and_daemon_in_one_pass() {
    let root = RootConfig::from_toml(
        r#"
max_concurrent = 5

[daemon]
spawner = "local"
timeout_unit_secs = 2

[harness.fake]
argv = ["fake"]
"#,
    )
    .unwrap();

    assert_eq!(root.board.max_concurrent, 5);
    assert_eq!(root.board.harness["fake"].argv, vec!["fake"]);
    assert_eq!(root.daemon.spawner, SpawnerKind::Local);
    assert_eq!(root.daemon.timeout_unit_secs, 2);
}

#[test]
fn root_config_defaults_missing_sections() {
    let root = RootConfig::from_toml("").unwrap();
    assert_eq!(root, RootConfig::default());
    assert_eq!(root.daemon, DaemonConfig::default());
    assert_eq!(root.daemon.spawner, SpawnerKind::Herdr);
    assert_eq!(root.daemon.timeout_unit_secs, 60);
    assert_eq!(root.daemon.local_poll_ms, 2000);
    assert_eq!(root.daemon.tick_ms, 1000);
}

#[test]
fn root_config_rejects_bad_values_and_malformed_toml() {
    for source in [
        "[daemon]\nspawner = \"unknown\"\n",
        "max_concurrent = \"three\"\n",
        "max_concurrent = [\n",
    ] {
        assert!(matches!(
            RootConfig::from_toml(source),
            Err(Error::Config(_))
        ));
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[daemon]\nspawner = \"not-a-spawner\"\n").unwrap();
    assert!(matches!(
        RootConfig::load_from(&path),
        Err(Error::Config(_))
    ));
}

#[test]
fn card_actions_default_to_none() {
    assert!(Config::default().card_actions.is_empty());
    assert!(Config::from_toml("").unwrap().card_actions.is_empty());
    assert!(Config::default().card_actions_for("Review").is_empty());
}

#[test]
fn card_actions_parse_and_filter_by_column() {
    let c = Config::from_toml(
        r#"
[[card_action]]
label = "Review"
columns = ["Code review"]
argv = ["my-review-tool", "--base", "origin/main"]

[[card_action]]
label = "Open shell"
argv = ["sh", "-c", "exec $SHELL"]
"#,
    )
    .unwrap();
    assert_eq!(c.card_actions.len(), 2);
    assert_eq!(c.card_actions[0].label, "Review");
    assert_eq!(c.card_actions[0].columns, vec!["Code review".to_string()]);

    // Column names match trimmed and case-insensitively; an action without
    // `columns` applies everywhere.
    let in_review = c.card_actions_for(" code REVIEW ");
    let labels: Vec<_> = in_review.iter().map(|a| a.label.as_str()).collect();
    assert_eq!(labels, vec!["Review", "Open shell"]);
    assert_eq!(
        in_review[0].argv,
        vec!["my-review-tool", "--base", "origin/main"]
    );

    let elsewhere = c.card_actions_for("Todo");
    let labels: Vec<_> = elsewhere.iter().map(|a| a.label.as_str()).collect();
    assert_eq!(labels, vec!["Open shell"]);
}

#[test]
fn card_actions_are_capped_at_nine() {
    let mut toml = String::new();
    for i in 0..12 {
        toml.push_str(&format!(
            "[[card_action]]\nlabel = \"a{i}\"\nargv = [\"true\"]\n"
        ));
    }
    let c = Config::from_toml(&toml).unwrap();
    assert_eq!(c.card_actions.len(), 12);
    let actions = c.card_actions_for("Todo");
    assert_eq!(actions.len(), 9);
    assert_eq!(actions[8].label, "a8");
}

#[test]
fn card_actions_reject_empty_label_or_argv() {
    for source in [
        "[[card_action]]\nlabel = \"\"\nargv = [\"true\"]\n",
        "[[card_action]]\nlabel = \"  \"\nargv = [\"true\"]\n",
        "[[card_action]]\nlabel = \"x\"\nargv = []\n",
        "[[card_action]]\nlabel = \"x\"\nargv = [\"\"]\n",
        "[[card_action]]\nlabel = \"x\"\n",
    ] {
        assert!(
            matches!(RootConfig::from_toml(source), Err(Error::Config(_))),
            "accepted: {source}"
        );
    }
}
