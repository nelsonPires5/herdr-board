//! Config defaults + parsing.

use board_core::config::Config;

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
