//! Live Pi model catalog discovery (auth.json + models-store.json).

use std::fs;

use board_core::pi_catalog::load_from_files;
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
      {"id": "glm-4.7", "reasoning": true, "thinkingLevelMap": {"minimal": "low", "xhigh": "xhigh"}}
    ]
  },
  "openai-codex": {
    "models": [
      {"id": "gpt-5.6-sol", "reasoning": true, "thinkingLevelMap": {"low": "low", "high": "high", "max": "max"}}
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
        vec!["openai-codex/gpt-5.6-sol", "zai/glm-4.7", "zai/glm-5.2"]
    );
    assert!(!ids.iter().any(|id| id.starts_with("ghost/")));
}

#[test]
fn effort_levels_come_from_thinking_level_map() {
    let dir = fixture_agent_dir(r#"{"zai": {"type": "api_key"}}"#, STORE);
    let models = load_from_files(dir.path()).unwrap();
    let glm47 = models.iter().find(|m| m.id == "zai/glm-4.7").unwrap();
    // thinkingLevelMap {minimal, xhigh} → canonical order [Minimal, Xhigh].
    assert_eq!(glm47.efforts, vec![Effort::Minimal, Effort::Xhigh]);
}

#[test]
fn model_without_thinking_level_map_gets_default_ladder() {
    let dir = fixture_agent_dir(r#"{"zai": {"type": "api_key"}}"#, STORE);
    let models = load_from_files(dir.path()).unwrap();
    let glm52 = models.iter().find(|m| m.id == "zai/glm-5.2").unwrap();
    // No map → the full Pi thinking ladder (off..max).
    assert_eq!(glm52.efforts.len(), 7);
    assert!(glm52.efforts.contains(&Effort::Off));
    assert!(glm52.efforts.contains(&Effort::Max));
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
