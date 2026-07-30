//! Live Pi model catalog discovery (auth.json + models-store.json).

use std::fs;

use board_core::pi_catalog::{load_from_cli, load_from_files};
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
