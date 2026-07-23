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
