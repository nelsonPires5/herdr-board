//! Live Pi model catalog discovery (auth.json + models-store.json).

use std::fs;
use std::process::Command;
use std::time::{Duration, Instant};

use board_core::pi_catalog::{load_from_cli, load_from_cli_bounded, load_from_files};
use board_core::protocol::Effort;

/// Write `auth.json` + `models-store.json` into a temp agent dir.
fn fixture_agent_dir(auth: &str, store: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("auth.json"), auth).unwrap();
    fs::write(dir.path().join("models-store.json"), store).unwrap();
    dir
}

const STORE: &str = r#"{
  "zai": {
    "models": [
      {"id": "glm-5.2", "reasoning": true},
      {"id": "glm-sparse", "reasoning": true, "thinkingLevelMap": {"minimal": "low", "xhigh": "xhigh", "max": "max"}},
      {"id": "glm-no-off", "reasoning": true, "thinkingLevelMap": {"off": null, "minimal": "low", "xhigh": "xhigh"}},
      {"id": "glm-holes", "reasoning": true, "thinkingLevelMap": {"low": null, "medium": null, "xhigh": null, "max": "max"}}
    ]
  },
  "openai-codex": {
    "models": [
      {"id": "gpt-5.6-sol", "reasoning": true, "thinkingLevelMap": {"minimal": "low", "xhigh": "xhigh", "max": "max"}}
    ]
  },
  "ghost": {
    "models": [{"id": "nope", "reasoning": true}]
  }
}"#;

#[test]
fn filters_to_authenticated_providers_and_prefixes_ids() {
    // auth has zai + openai-codex; the store also has `ghost` (no auth) → dropped.
    let dir = fixture_agent_dir(
        r#"{"zai": {"type": "api_key"}, "openai-codex": {"type": "oauth"}}"#,
        STORE,
    );
    let models = load_from_files(dir.path()).unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "openai-codex/gpt-5.6-sol",
            "zai/glm-5.2",
            "zai/glm-holes",
            "zai/glm-no-off",
            "zai/glm-sparse",
        ]
    );
    assert!(!ids.iter().any(|id| id.starts_with("ghost/")));
}

#[test]
fn thinking_level_map_uses_pi_tristate_semantics_in_canonical_order() {
    let dir = fixture_agent_dir(r#"{"zai": {"type": "api_key"}}"#, STORE);
    let models = load_from_files(dir.path()).unwrap();
    let cases = [
        (
            "zai/glm-sparse",
            vec![
                Effort::Off,
                Effort::Minimal,
                Effort::Low,
                Effort::Medium,
                Effort::High,
                Effort::Xhigh,
                Effort::Max,
            ],
        ),
        (
            "zai/glm-no-off",
            vec![
                Effort::Minimal,
                Effort::Low,
                Effort::Medium,
                Effort::High,
                Effort::Xhigh,
            ],
        ),
        (
            "zai/glm-holes",
            vec![Effort::Off, Effort::Minimal, Effort::High, Effort::Max],
        ),
        (
            "zai/glm-5.2",
            vec![
                Effort::Off,
                Effort::Minimal,
                Effort::Low,
                Effort::Medium,
                Effort::High,
            ],
        ),
    ];

    for (id, expected) in cases {
        let model = models.iter().find(|model| model.id == id).unwrap();
        assert_eq!(model.efforts, expected, "efforts for {id}");
        let mut deduped = model.efforts.clone();
        deduped.dedup();
        assert_eq!(model.efforts, deduped, "duplicate efforts for {id}");
    }
}

#[cfg(unix)]
#[test]
fn cli_fallback_keeps_provider_prefixed_models_and_existing_default_ladder() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let pi = dir.path().join("pi-list-models");
    fs::write(
        &pi,
        "#!/bin/sh\nprintf 'provider model context input reasoning\\nopenai-codex gpt-test 1 text yes\\nzai glm-test 1 text yes\\n'\n",
    )
    .unwrap();
    fs::set_permissions(&pi, fs::Permissions::from_mode(0o700)).unwrap();

    let models = load_from_cli(pi.to_str().unwrap()).unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["openai-codex/gpt-test", "zai/glm-test"]
    );
    assert!(models.iter().all(|model| model.efforts
        == vec![
            Effort::Off,
            Effort::Minimal,
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max,
        ]));
}

/// Windows twin of the unix fixture above: npm installs `pi` as a `pi.cmd`
/// shim, which only resolves through `PATHEXT` (the caller passes the bare,
/// extension-less name exactly as the daemon passes `pi`).
#[cfg(windows)]
#[test]
fn cli_fallback_resolves_a_cmd_shim() {
    let dir = tempfile::tempdir().unwrap();
    let table = dir.path().join("table.txt");
    fs::write(
        &table,
        "provider model context input reasoning\r\nopenai-codex gpt-test 1 text yes\r\nzai glm-test 1 text yes\r\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("pi-list-models.cmd"),
        format!("@type \"{}\"\r\n", table.display()),
    )
    .unwrap();

    let bin = dir.path().join("pi-list-models");
    let models = load_from_cli(bin.to_str().unwrap()).unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["openai-codex/gpt-test", "zai/glm-test"]
    );
}

/// A `pi` that never answers: before the bounded runner, `pi --list-models`
/// ran through `Command::output()` with no deadline at all.
#[cfg(unix)]
fn hanging_pi(dir: &tempfile::TempDir) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.path().join("pi-hang");
    fs::write(&bin, "#!/bin/sh\nexec sleep 30\n").unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    bin
}

#[cfg(windows)]
fn hanging_pi(dir: &tempfile::TempDir) -> std::path::PathBuf {
    fs::write(dir.path().join("pi-hang.cmd"), "@ping -n 30 127.0.0.1\r\n").unwrap();
    dir.path().join("pi-hang")
}

#[test]
fn cli_fallback_is_bounded_by_a_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let bin = hanging_pi(&dir);
    let started = Instant::now();
    let models = load_from_cli_bounded(bin.to_str().unwrap(), Duration::from_secs(2));
    let elapsed = started.elapsed();
    assert!(models.is_none(), "a hung pi must yield None");
    assert!(
        elapsed < Duration::from_secs(15),
        "a hung pi must be abandoned at the deadline (took {elapsed:?})"
    );
}

#[test]
fn missing_cli_yields_none() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-pi");
    assert!(load_from_cli(missing.to_str().unwrap()).is_none());
}

/// `default_agent_dir()` reads the process environment, so it runs in a
/// child test process with a controlled env (the parent never mutates its
/// own). Unix honors `HOME` verbatim — fixture homes keep working — while
/// Windows uses the profile folder, as pi's own `os.homedir()` does.
#[test]
fn default_agent_dir_follows_the_platform_home() {
    const CHILD_ENV: &str = "BOARD_CORE_PI_HOME_CHILD";
    if std::env::var_os(CHILD_ENV).is_some() {
        let dir = board_core::pi_catalog::default_agent_dir();
        println!(
            "AGENT_DIR={}",
            dir.map(|d| d.display().to_string()).unwrap_or_default()
        );
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "default_agent_dir_follows_the_platform_home",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .env_remove("PI_CODING_AGENT_DIR")
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let line = stdout
        .lines()
        .find_map(|line| line.strip_prefix("AGENT_DIR="))
        .expect("child printed its agent dir");
    let dir = std::path::PathBuf::from(line);
    assert!(
        dir.ends_with(std::path::Path::new(".pi").join("agent")),
        "{dir:?}"
    );
    if cfg!(unix) {
        assert_eq!(dir, home.path().join(".pi").join("agent"));
    } else {
        assert!(
            !dir.starts_with(home.path()),
            "Windows must not treat a stray HOME as the profile: {dir:?}"
        );
    }
}

#[test]
fn missing_auth_file_yields_none() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("models-store.json"), STORE).unwrap();
    // No auth.json → None (caller falls back).
    assert!(load_from_files(dir.path()).is_none());
}

#[test]
fn no_authenticated_models_yields_none() {
    // auth only has `ghost`, whose catalog block has one model, but auth for
    // a provider not in the store → nothing to offer.
    let dir = fixture_agent_dir(r#"{"other": {"type": "api_key"}}"#, STORE);
    assert!(load_from_files(dir.path()).is_none());
}

#[test]
fn malformed_store_json_yields_none() {
    let dir = fixture_agent_dir(r#"{"zai": {"type": "api_key"}}"#, "not json");
    assert!(load_from_files(dir.path()).is_none());
}
