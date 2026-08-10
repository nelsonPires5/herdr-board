//! Live Codex model catalog discovery (`CODEX_HOME/models_cache.json`).
//!
//! Codex keeps a models cache at `$CODEX_HOME/models_cache.json` (default
//! `~/.codex/models_cache.json`). It lists every model the CLI knows, with a
//! per-model `supported_reasoning_levels` array — the same vocabulary the
//! board needs for `ModelInfo.efforts`. Unlike Pi there is no auth step: the
//! cache is the catalog, and `visibility: "list"` marks the models the CLI
//! actually offers the user.
//!
//! Contracts pinned here (the daemon overlays these onto the codex
//! capabilities exactly like `pi_catalog`):
//! - the modern shape: `{"models": [{"slug", "visibility", ...,
//!   "supported_reasoning_levels": [{"effort", "description"}]}]}`;
//! - visible models only (`visibility` missing or `"list"`; `"hide"` models
//!   are dropped — `codex-auto-review`-style entries never reach the board);
//! - codex's `none` level maps to the board's `off`; every other level keeps
//!   its canonical spelling; levels the board does not know (e.g. `ultra`)
//!   are filtered out, never added to the `Effort` enum;
//! - a model whose levels all get filtered is dropped (it stays reachable
//!   free-form with the default ladder);
//! - a missing/malformed/empty cache yields `None` → the caller keeps the
//!   static free-form catalog (`models: []`), so the board never breaks when
//!   the cache is absent or from a newer CLI;
//! - the legacy map shape (`{slug: {supported_reasoning_levels: [...]}}`)
//!   parses too, so an older cache stays usable.

use std::fs;

use board_core::codex_catalog::{live_models, load_from_files};
use board_core::protocol::Effort;

/// A modern-format cache: top-level `models` array with slugs, visibility and
/// per-model reasoning levels (mirror of the real codex cache shape, verified
/// against an installed CLI).
const MODERN_CACHE: &str = r#"{
  "fetched_at": "2026-01-01T00:00:00Z",
  "etag": "abc",
  "client_version": "0.59.0",
  "models": [
    {
      "slug": "gpt-5.6-sol",
      "visibility": "list",
      "supported_reasoning_levels": [
        {"effort": "low", "description": "fast"},
        {"effort": "medium", "description": "balanced"},
        {"effort": "high", "description": "deep"},
        {"effort": "xhigh", "description": "deeper"},
        {"effort": "max", "description": "deepest"},
        {"effort": "ultra", "description": "auto-delegating"}
      ]
    },
    {
      "slug": "gpt-5.4",
      "visibility": "list",
      "supported_reasoning_levels": [
        {"effort": "none", "description": "no reasoning"},
        {"effort": "low", "description": "fast"},
        {"effort": "medium", "description": "balanced"}
      ]
    },
    {
      "slug": "gpt-5.6-sol-wm",
      "visibility": "hide",
      "supported_reasoning_levels": [
        {"effort": "low", "description": "hidden"}
      ]
    },
    {
      "slug": "codex-auto-review",
      "visibility": "hide",
      "supported_reasoning_levels": [
        {"effort": "low", "description": "internal"}
      ]
    },
    {
      "slug": "ultra-only",
      "visibility": "list",
      "supported_reasoning_levels": [
        {"effort": "ultra", "description": "only ultra"}
      ]
    }
  ]
}"#;

fn fixture_cache(contents: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("models_cache.json"), contents).unwrap();
    dir
}

#[test]
fn maps_visible_slugs_and_reasoning_levels_into_model_info() {
    let dir = fixture_cache(MODERN_CACHE);
    let models = load_from_files(dir.path()).unwrap();

    // `hide` models are dropped; `ultra-only` is dropped because every level
    // got filtered. The two visible models remain, sorted by slug.
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["gpt-5.4", "gpt-5.6-sol"]);

    // Codex `none` maps to board `off`; `ultra` is filtered out; the rest
    // keep their canonical spellings in ascending order.
    let sol = models.iter().find(|m| m.id == "gpt-5.6-sol").unwrap();
    assert_eq!(
        sol.efforts,
        vec![
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max,
        ],
        "ultra must be filtered, never added to the Effort enum"
    );
    let gpt54 = models.iter().find(|m| m.id == "gpt-5.4").unwrap();
    assert_eq!(
        gpt54.efforts,
        vec![Effort::Off, Effort::Low, Effort::Medium],
        "codex `none` maps to the board's lowest effort `off`"
    );
    let mut deduped = gpt54.efforts.clone();
    deduped.dedup();
    assert_eq!(gpt54.efforts, deduped, "no duplicate efforts");
}

#[test]
fn legacy_map_shape_with_plain_string_levels_parses() {
    // Older caches map slug -> model object (some with plain-string levels).
    let dir = fixture_cache(
        r#"{
          "o3": {"supported_reasoning_levels": ["none", "low", "high"]},
          "gpt-5-mini": {"supported_reasoning_levels": ["minimal", "max", "ultra"]}
        }"#,
    );
    let models = load_from_files(dir.path()).unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["gpt-5-mini", "o3"]);
    let o3 = models.iter().find(|m| m.id == "o3").unwrap();
    assert_eq!(o3.efforts, vec![Effort::Off, Effort::Low, Effort::High]);
    let mini = models.iter().find(|m| m.id == "gpt-5-mini").unwrap();
    assert_eq!(mini.efforts, vec![Effort::Minimal, Effort::Max]);
}

#[test]
fn missing_cache_yields_none() {
    let dir = tempfile::tempdir().unwrap();
    assert!(load_from_files(dir.path()).is_none());
}

#[test]
fn malformed_cache_yields_none() {
    let dir = fixture_cache("not json at all {");
    assert!(load_from_files(dir.path()).is_none());
}

#[test]
fn empty_or_shapeless_cache_yields_none() {
    // Valid JSON but no models list → nothing to offer.
    let dir = fixture_cache(r#"{"fetched_at": "x"}"#);
    assert!(load_from_files(dir.path()).is_none());
}

#[test]
fn no_home_disables_live_discovery() {
    // `live_models(None)` is the daemon's no-configured-home path: empty,
    // so the caller keeps the static free-form catalog.
    assert!(live_models(None).is_empty());
}

#[test]
fn live_models_falls_back_to_static_on_missing_cache() {
    let dir = tempfile::tempdir().unwrap();
    assert!(live_models(Some(dir.path())).is_empty());
}
