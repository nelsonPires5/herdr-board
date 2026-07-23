use super::*;
use board_core::config::RootConfig;

fn env<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |key| {
        values
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| (*value).to_owned())
    }
}

#[test]
fn injected_environment_overrides_typed_config_without_process_env() {
    let root =
        RootConfig::from_toml("[daemon]\nspawner = \"herdr\"\ntimeout_unit_secs = 12\n").unwrap();
    let settings = DaemonSettings::from_root(
        &root,
        &env(&[("BOARD_SPAWNER", "local"), ("BOARD_TICK_MS", "7")]),
    )
    .unwrap();

    assert_eq!(settings.spawner, SpawnerKind::Local);
    assert_eq!(settings.timeout_unit_secs, 12);
    assert_eq!(settings.local_poll_ms, 2000);
    assert_eq!(settings.tick_ms, 7);
}

#[test]
fn missing_daemon_config_uses_runtime_defaults() {
    let settings = DaemonSettings::from_root(&RootConfig::default(), &env(&[])).unwrap();
    assert_eq!(settings, DaemonSettings::default());
}

#[test]
fn invalid_injected_environment_is_a_config_error() {
    let root = RootConfig::default();
    assert!(matches!(
        DaemonSettings::from_root(&root, &env(&[("BOARD_SPAWNER", "bogus")])),
        Err(Error::Config(_))
    ));
}

#[test]
fn malformed_file_is_not_replaced_with_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[daemon\n").unwrap();

    assert!(matches!(
        DaemonSettings::load_with_env(&path, &env(&[])),
        Err(Error::Config(_))
    ));
}
