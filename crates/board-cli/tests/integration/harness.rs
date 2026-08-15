use board_core::client::BoardClient;
use serde_json::Value;

use super::{fake_card, json_output, todo_id, TestDaemon};

// -- harness / space CLI verbs -----------------------------------------------

#[test]
fn harness_models_claude_json_and_human() {
    let td = TestDaemon::start(&[]);

    // --json: full HarnessCapabilities — 4 models, 5 efforts each, freeform.
    let out = td.board(&["harness", "models", "claude", "--json"]);
    let caps: board_core::capability::HarnessCapabilities =
        serde_json::from_value(json_output(&out)).expect("parse HarnessCapabilities");
    assert_eq!(caps.harness, "claude");
    assert!(caps.model_freeform);
    assert_eq!(caps.models.len(), 4, "claude has 4 known models");
    let ids: Vec<&str> = caps.models.iter().map(|m| m.id.as_str()).collect();
    for expected in ["fable", "opus", "sonnet", "haiku"] {
        assert!(ids.contains(&expected), "missing model {expected}");
    }
    for m in &caps.models {
        assert_eq!(m.efforts.len(), 5, "{} should list 5 efforts", m.id);
    }

    // human: one line per model with its efforts, plus the freeform note.
    let out = td.board(&["harness", "models", "claude"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.lines()
            .any(|l| l.starts_with("fable") && l.contains("low medium high xhigh max")),
        "human output lists model efforts; got:\n{text}"
    );
    assert!(
        text.contains("any model string accepted"),
        "human output notes model_freeform; got:\n{text}"
    );
}

#[test]
fn harness_list_builtins_and_config_defined() {
    let td = TestDaemon::start(&[]);
    // human: one harness per line, built-ins first (pi, claude, codex,
    // opencode) then config.
    let out = td.board(&["harness", "list"]);
    assert!(out.status.success(), "harness list should succeed");
    let text = String::from_utf8_lossy(&out.stdout);
    let names: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        names,
        vec!["pi", "claude", "codex", "opencode", "antigravity", "fake"],
        "got:\n{text}"
    );

    // --json: the same names, default-first, as a JSON array.
    let out = td.board(&["harness", "list", "--json"]);
    let names: Vec<String> = serde_json::from_value(json_output(&out)).unwrap();
    assert_eq!(
        names,
        vec!["pi", "claude", "codex", "opencode", "antigravity", "fake"]
    );
}

#[test]
fn harness_models_default_is_pi() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&["harness", "models", "--json"]);
    let caps: board_core::capability::HarnessCapabilities =
        serde_json::from_value(json_output(&out)).unwrap();
    assert_eq!(caps.harness, "pi");
    assert!(caps.models.is_empty());
    assert!(caps.model_freeform);
    assert!(caps
        .default_efforts
        .iter()
        .any(|effort| effort.as_str() == "low"));
}

#[test]
fn harness_models_unknown_harness_errors() {
    let td = TestDaemon::start(&[]);
    // The daemon answers with protocol code 2 (not found), which the CLI passes
    // through as the exit status (see `exit_codes.rs`).
    let out = td.board(&["harness", "models", "ghost"]);
    assert_eq!(out.status.code(), Some(2), "not found is protocol code 2");
    assert!(out.stdout.is_empty(), "no capabilities printed on error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        err.trim_end(),
        "board: boardd error 2: not found: unknown harness 'ghost'; known: pi, claude, codex, opencode, antigravity, fake",
        "unknown harness names the harness and the known set"
    );

    // --json fails identically, with the code kept in the error envelope.
    let out = td.board(&["harness", "models", "ghost", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let error: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("JSON error on stderr");
    assert_eq!(error["error"]["code"], 2);
    assert_eq!(
        error["error"]["message"],
        "not found: unknown harness 'ghost'; known: pi, claude, codex, opencode, antigravity, fake"
    );
}

#[test]
fn harness_codex_models_efforts_and_permissions() {
    let td = TestDaemon::start(&[]);

    // --json: full HarnessCapabilities — no model aliases (models are
    // free-form), the full effort ladder with `off` first (mapped to codex
    // `none` only at argv time), and the three user-facing approval presets.
    let out = td.board(&["harness", "models", "codex", "--json"]);
    let caps: board_core::capability::HarnessCapabilities =
        serde_json::from_value(json_output(&out)).expect("parse HarnessCapabilities");
    assert_eq!(caps.harness, "codex");
    assert!(
        caps.models.is_empty(),
        "codex models are free-form, not aliased"
    );
    assert!(caps.model_freeform);
    let efforts: Vec<&str> = caps.default_efforts.iter().map(|e| e.as_str()).collect();
    assert_eq!(
        efforts,
        vec!["off", "minimal", "low", "medium", "high", "xhigh", "max"],
        "the full board ladder; off maps to codex `none` only while building argv"
    );
    assert_eq!(
        caps.permission_modes,
        vec!["ask-for-approval", "approve-for-me", "full-access"],
        "the three user-facing approval presets; sandbox stays a separate dimension"
    );

    // human: notes the free-form catalog (there is no model table to render).
    let out = td.board(&["harness", "models", "codex"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("any model string accepted"),
        "human output notes model_freeform; got:\n{text}"
    );

    // efforts: every model string is accepted (known:false) with all 7 levels.
    let out = td.board(&[
        "harness",
        "efforts",
        "codex",
        "--model",
        "gpt-5.2-codex",
        "--json",
    ]);
    let v = json_output(&out);
    assert_eq!(v["model"], "gpt-5.2-codex");
    assert_eq!(v["known"], false);
    let efforts: Vec<&str> = v["efforts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    assert_eq!(
        efforts,
        vec!["off", "minimal", "low", "medium", "high", "xhigh", "max"]
    );

    // permissions: exactly the three approval presets, JSON and one-per-line.
    let out = td.board(&["harness", "permissions", "codex", "--json"]);
    let modes: Vec<String> = serde_json::from_value(json_output(&out)).unwrap();
    assert_eq!(
        modes,
        vec!["ask-for-approval", "approve-for-me", "full-access"]
    );
    let out = td.board(&["harness", "permissions", "codex"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for mode in ["ask-for-approval", "approve-for-me", "full-access"] {
        assert!(
            text.lines().any(|l| l == mode),
            "missing permission line {mode}; got:\n{text}"
        );
    }
}

#[test]
fn harness_codex_models_overlay_live_catalog_from_codex_home() {
    // A CODEX_HOME with models_cache.json → the daemon overlays the visible
    // model slugs and their per-model supported_reasoning_levels (efforts
    // filtered to the board ladder: `none`→off, `ultra` dropped).
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models_cache.json"),
        r#"{"models": [
          {"slug": "gpt-5.6-sol", "visibility": "list",
           "supported_reasoning_levels": [
             {"effort": "none"}, {"effort": "low"}, {"effort": "high"},
             {"effort": "ultra"}]},
          {"slug": "gpt-5.6-sol-wm", "visibility": "hide",
           "supported_reasoning_levels": [{"effort": "low"}]}
        ]}"#,
    )
    .unwrap();
    let td = TestDaemon::start(&[("CODEX_HOME", dir.path().to_str().unwrap())]);

    let out = td.board(&["harness", "models", "codex", "--json"]);
    let caps: board_core::capability::HarnessCapabilities =
        serde_json::from_value(json_output(&out)).expect("parse HarnessCapabilities");
    let ids: Vec<&str> = caps.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["gpt-5.6-sol"], "visible slugs only");
    assert_eq!(caps.models[0].id, "gpt-5.6-sol");
    let efforts: Vec<&str> = caps.models[0].efforts.iter().map(|e| e.as_str()).collect();
    assert_eq!(
        efforts,
        vec!["off", "low", "high"],
        "none maps to off; ultra is filtered out of the wire protocol"
    );
    assert!(caps.model_freeform);

    // The static catalog is the fallback when CODEX_HOME has no cache.
    let empty = tempfile::tempdir().unwrap();
    let td = TestDaemon::start(&[("CODEX_HOME", empty.path().to_str().unwrap())]);
    let out = td.board(&["harness", "models", "codex", "--json"]);
    let caps: board_core::capability::HarnessCapabilities =
        serde_json::from_value(json_output(&out)).unwrap();
    assert!(
        caps.models.is_empty(),
        "missing cache keeps the static catalog"
    );
}

#[test]
fn harness_opencode_models_efforts_and_permissions() {
    // A failing OPENCODE_BIN pins the daemon to the deterministic static
    // fallback catalog (the live CLI catalog is credential-dependent and is
    // exercised separately by the overlay test).
    let td = TestDaemon::start(&[("OPENCODE_BIN", "/nonexistent/opencode-binary")]);

    // --json: the static fallback catalog truthfully lists
    // opencode/nemotron-3-ultra-free (EMPTY efforts — the real model declares
    // `variants: {}`) and the fixture model opencode/deepseek-v4-flash-free
    // (low/high/max); models are free-form beyond that; the effort vocabulary
    // is the full board ladder (the board calls the opencode CLI's "variant"
    // dimension effort); permissions are exactly the two verified modes.
    let out = td.board(&["harness", "models", "opencode", "--json"]);
    let caps: board_core::capability::HarnessCapabilities =
        serde_json::from_value(json_output(&out)).expect("parse HarnessCapabilities");
    assert_eq!(caps.harness, "opencode");
    assert!(caps.model_freeform);
    let nemotron = caps
        .models
        .iter()
        .find(|m| m.id == "opencode/nemotron-3-ultra-free")
        .expect("static fallback defines nemotron");
    assert!(
        nemotron.efforts.is_empty(),
        "nemotron really has variants {{}} → offers no board effort"
    );
    let deepseek = caps
        .models
        .iter()
        .find(|m| m.id == "opencode/deepseek-v4-flash-free")
        .expect("static fallback defines deepseek");
    let efforts: Vec<&str> = deepseek.efforts.iter().map(|e| e.as_str()).collect();
    assert_eq!(efforts, vec!["low", "high", "max"]);
    let defaults: Vec<&str> = caps.default_efforts.iter().map(|e| e.as_str()).collect();
    assert_eq!(
        defaults,
        vec!["off", "minimal", "low", "medium", "high", "xhigh", "max"],
        "the full board ladder; off maps to opencode variant `none` only while building argv"
    );
    assert_eq!(
        caps.permission_modes,
        vec!["default", "auto-approve"],
        "the two verified board-facing modes"
    );

    // human: notes the free-form catalog and lists the fallback model.
    let out = td.board(&["harness", "models", "opencode"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("opencode/nemotron-3-ultra-free"),
        "human output lists the fallback model; got:\n{text}"
    );
    assert!(
        text.contains("any model string accepted"),
        "human output notes model_freeform; got:\n{text}"
    );

    // efforts: every model string is accepted (known:false) with all 7 levels.
    let out = td.board(&[
        "harness",
        "efforts",
        "opencode",
        "--model",
        "opencode/whatever-model",
        "--json",
    ]);
    let v = json_output(&out);
    assert_eq!(v["model"], "opencode/whatever-model");
    assert_eq!(v["known"], false);
    let efforts: Vec<&str> = v["efforts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    assert_eq!(
        efforts,
        vec!["off", "minimal", "low", "medium", "high", "xhigh", "max"]
    );

    // permissions: exactly the two modes, JSON and one-per-line.
    let out = td.board(&["harness", "permissions", "opencode", "--json"]);
    let modes: Vec<String> = serde_json::from_value(json_output(&out)).unwrap();
    assert_eq!(modes, vec!["default", "auto-approve"]);
    let out = td.board(&["harness", "permissions", "opencode"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for mode in ["default", "auto-approve"] {
        assert!(
            text.lines().any(|l| l == mode),
            "missing permission line {mode}; got:\n{text}"
        );
    }
}

#[test]
fn harness_opencode_models_overlay_live_catalog_from_cli() {
    // An OPENCODE_BIN resolving to a working CLI → the daemon overlays the
    // live model catalog (per-model variant efforts) onto the opencode
    // capabilities. Variants the board does not know (`thinking`) never reach
    // the wire; a valid model with no variants (nemotron) stays listed with
    // EMPTY efforts.
    let dir = tempfile::tempdir().unwrap();
    let script = r#"#!/bin/sh
cat <<'HBEOF'
opencode/nemotron-3-ultra-free
{
  "id": "nemotron-3-ultra-free",
  "variants": {}
}
opencode/deepseek-v4-flash-free
{
  "id": "deepseek-v4-flash-free",
  "variants": {
    "low": {"reasoningEffort": "low"},
    "high": {"reasoningEffort": "high"},
    "max": {"reasoningEffort": "max"}
  }
}
HBEOF
"#;
    let bin = dir.path().join("opencode-fixture");
    std::fs::write(&bin, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let td = TestDaemon::start(&[("OPENCODE_BIN", bin.to_str().unwrap())]);

    let out = td.board(&["harness", "models", "opencode", "--json"]);
    let caps: board_core::capability::HarnessCapabilities =
        serde_json::from_value(json_output(&out)).expect("parse HarnessCapabilities");
    let ids: Vec<&str> = caps.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "opencode/deepseek-v4-flash-free",
            "opencode/nemotron-3-ultra-free",
        ],
        "live catalog preferred over the static fallback"
    );
    let deepseek = caps
        .models
        .iter()
        .find(|m| m.id == "opencode/deepseek-v4-flash-free")
        .unwrap();
    let efforts: Vec<&str> = deepseek.efforts.iter().map(|e| e.as_str()).collect();
    assert_eq!(
        efforts,
        vec!["low", "high", "max"],
        "verified live variant keys map onto board efforts in canonical order"
    );
    let nemotron = caps
        .models
        .iter()
        .find(|m| m.id == "opencode/nemotron-3-ultra-free")
        .unwrap();
    assert!(
        nemotron.efforts.is_empty(),
        "a valid model with variants {{}} stays listed with no board efforts"
    );
    assert!(caps.model_freeform);

    // The static fallback is kept when the configured bin fails.
    let td = TestDaemon::start(&[("OPENCODE_BIN", "/nonexistent/opencode-binary")]);
    let out = td.board(&["harness", "models", "opencode", "--json"]);
    let caps: board_core::capability::HarnessCapabilities =
        serde_json::from_value(json_output(&out)).unwrap();
    assert_eq!(
        caps.models[0].id, "opencode/nemotron-3-ultra-free",
        "a failing CLI keeps the static fallback catalog"
    );
}

#[test]
fn card_new_opencode_effort_and_permission_persist() {
    // A failing OPENCODE_BIN keeps the daemon on the deterministic static
    // fallback catalog; the free-form model string gets the full ladder.
    let td = TestDaemon::start(&[("OPENCODE_BIN", "/nonexistent/opencode-binary")]);

    // An opencode card: free-form model, the `off` effort (spelled variant
    // `none` for the opencode CLI only at launch time), and the auto-approve
    // permission mode.
    let out = td.board(&[
        "card",
        "new",
        "--title",
        "opencode task",
        "--harness",
        "opencode",
        "--model",
        "opencode/custom-model",
        "--effort",
        "off",
        "--permission",
        "auto-approve",
        "--json",
    ]);
    let card = json_output(&out);
    assert_eq!(card["harness"], "opencode");
    assert_eq!(card["model"], "opencode/custom-model");
    assert_eq!(card["effort"], "off");
    assert_eq!(card["permission_mode"], "auto-approve");
    let id = card["id"].as_i64().expect("card id");

    // Edit flips to a different effort and the default permission mode.
    let edited = json_output(&td.board(&[
        "card",
        "edit",
        &id.to_string(),
        "--effort",
        "max",
        "--permission",
        "default",
        "--json",
    ]));
    assert_eq!(edited["effort"], "max");
    assert_eq!(edited["permission_mode"], "default");
    assert_eq!(edited["harness"], "opencode");
}

#[test]
fn card_new_rejects_opencode_out_of_catalog_values() {
    let td = TestDaemon::start(&[]);

    // `ultra` is not in the board ladder — rejected up front for opencode too.
    let out = td.board(&[
        "card",
        "new",
        "--title",
        "bad effort",
        "--harness",
        "opencode",
        "--effort",
        "ultra",
    ]);
    assert!(!out.status.success(), "ultra must be rejected for opencode");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        err.trim_end(),
        "board: invalid effort 'ultra' (expected: off, minimal, low, medium, high, xhigh, max)",
        "the CLI's effort vocabulary is the full board ladder, off first; no ultra"
    );

    // A Claude-only permission mode is not an opencode permission mode.
    let out = td.board(&[
        "card",
        "new",
        "--title",
        "bad perm",
        "--harness",
        "opencode",
        "--permission",
        "acceptEdits",
    ]);
    assert!(
        !out.status.success(),
        "claude modes must be rejected for opencode"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("permission mode 'acceptEdits' is not accepted"),
        "names the rejected permission; got: {err}"
    );
}

#[test]
fn column_create_opencode_overrides_persist_and_clear() {
    // A failing OPENCODE_BIN keeps the daemon on the deterministic static
    // fallback catalog; the free-form model string gets the full ladder.
    let td = TestDaemon::start(&[("OPENCODE_BIN", "/nonexistent/opencode-binary")]);

    let created = json_output(&td.board(&[
        "column",
        "create",
        "--name",
        "OpenCode stage",
        "--harness",
        "opencode",
        "--model",
        "opencode/custom-model",
        "--effort",
        "minimal",
        "--permission",
        "default",
        "--json",
    ]));
    let id = created["id"].as_i64().expect("created column id");
    assert_eq!(created["harness_override"], "opencode");
    assert_eq!(created["model_override"], "opencode/custom-model");
    assert_eq!(created["effort_override"], "minimal");
    assert_eq!(created["permission_override"], "default");

    let edited = json_output(&td.board(&[
        "column",
        "edit",
        &id.to_string(),
        "--effort",
        "high",
        "--permission",
        "auto-approve",
        "--json",
    ]));
    assert_eq!(edited["effort_override"], "high");
    assert_eq!(edited["permission_override"], "auto-approve");
    assert_eq!(edited["harness_override"], "opencode");

    let cleared = json_output(&td.board(&[
        "column",
        "edit",
        &id.to_string(),
        "--clear-harness",
        "--clear-model",
        "--clear-effort",
        "--clear-permission",
        "--json",
    ]));
    assert_eq!(cleared["harness_override"], Value::Null);
    assert_eq!(cleared["model_override"], Value::Null);
    assert_eq!(cleared["effort_override"], Value::Null);
    assert_eq!(cleared["permission_override"], Value::Null);
}

#[test]
fn harness_efforts_known_and_unknown_model() {
    let td = TestDaemon::start(&[]);

    // Known model: efforts from the catalog, known:true.
    let out = td.board(&[
        "harness", "efforts", "claude", "--model", "sonnet", "--json",
    ]);
    let v = json_output(&out);
    assert_eq!(v["model"], "sonnet");
    assert_eq!(v["known"], true);
    assert_eq!(v["efforts"].as_array().unwrap().len(), 5);

    // Unknown-but-freeform model: all efforts, known:false.
    let out = td.board(&["harness", "efforts", "claude", "--model", "gpt-x", "--json"]);
    let v = json_output(&out);
    assert_eq!(v["model"], "gpt-x");
    assert_eq!(v["known"], false);
    assert_eq!(v["efforts"].as_array().unwrap().len(), 5);

    // Human output notes the unknown-but-accepted model.
    let out = td.board(&["harness", "efforts", "claude", "--model", "gpt-x"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("unknown"),
        "notes unknown model; got:\n{text}"
    );
}

#[test]
fn harness_permissions_matches_claude_modes() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&["harness", "permissions", "claude", "--json"]);
    let modes: Vec<String> = serde_json::from_value(json_output(&out)).unwrap();
    assert_eq!(
        modes,
        vec![
            "acceptEdits",
            "auto",
            "bypassPermissions",
            "manual",
            "dontAsk",
            "plan"
        ]
    );

    // Human output: one mode per line.
    let out = td.board(&["harness", "permissions", "claude"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for mode in [
        "acceptEdits",
        "auto",
        "bypassPermissions",
        "manual",
        "dontAsk",
        "plan",
    ] {
        assert!(
            text.lines().any(|l| l == mode),
            "missing permission line {mode}; got:\n{text}"
        );
    }
}

#[test]
fn space_list_without_herdr_surfaces_error() {
    // The test daemon has no herdr, so space.list yields the herdr-unavailable
    // error (code 4); the CLI must surface it cleanly (non-zero exit + message).
    let td = TestDaemon::start(&[]);
    let out = td.board(&["space", "list"]);
    assert!(!out.status.success(), "space list should exit non-zero");
    assert!(out.stdout.is_empty(), "no rows printed on error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("herdr") && err.contains("error 4"),
        "error surfaces herdr-unavailable; got: {err}"
    );

    // --json path fails the same way (error before any JSON is written).
    let out = td.board(&["space", "list", "--json"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
}

#[test]
fn session_list_without_herdr_surfaces_error() {
    // The test daemon runs the local spawner (no session registry), so
    // session.list yields the herdr-unavailable error (code 4); the CLI surfaces
    // it cleanly (non-zero exit + message, no rows printed).
    let td = TestDaemon::start(&[]);
    let out = td.board(&["session", "list"]);
    assert!(!out.status.success(), "session list should exit non-zero");
    assert!(out.stdout.is_empty(), "no rows printed on error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("herdr") && err.contains("error 4"),
        "error surfaces herdr-unavailable; got: {err}"
    );
}

#[test]
fn card_new_new_workspace_missing_cwd_is_validation_error() {
    // `new-workspace` requires both --space-ref and --space-cwd; omitting cwd
    // must surface the daemon's validation error (code 1).
    let td = TestDaemon::start(&[]);
    let out = td.board(&[
        "card",
        "new",
        "--title",
        "needs cwd",
        "--harness",
        "fake",
        "--space-kind",
        "new-workspace",
        "--space-ref",
        "my-feature",
    ]);
    assert!(
        !out.status.success(),
        "missing space-cwd should exit non-zero"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("error 1"),
        "error surfaces the validation code; got: {err}"
    );
}

#[test]
fn card_new_defaults_to_pi_and_claude_remains_explicit() {
    let td = TestDaemon::start(&[]);
    let pi = json_output(&td.board(&["card", "new", "--title", "default", "--json"]));
    assert_eq!(pi["harness"], "pi");

    let claude = json_output(&td.board(&[
        "card",
        "new",
        "--title",
        "explicit",
        "--harness",
        "claude",
        "--json",
    ]));
    assert_eq!(claude["harness"], "claude");
}

#[test]
fn card_new_rejects_pi_permission_mode() {
    let td = TestDaemon::start(&[]);
    let out = td.board(&[
        "card",
        "new",
        "--title",
        "bad",
        "--permission",
        "acceptEdits",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("pi does not support permission modes"));
}

#[test]
fn card_new_codex_effort_and_approval_persist() {
    let td = TestDaemon::start(&[]);

    // A codex card: free-form model, the `off` effort (spelled `none` for the
    // codex CLI only at launch time), and an explicit approval preset.
    let out = td.board(&[
        "card",
        "new",
        "--title",
        "codex task",
        "--harness",
        "codex",
        "--model",
        "gpt-5.2-codex",
        "--effort",
        "off",
        "--permission",
        "full-access",
        "--json",
    ]);
    let card = json_output(&out);
    assert_eq!(card["harness"], "codex");
    assert_eq!(card["model"], "gpt-5.2-codex");
    assert_eq!(card["effort"], "off");
    assert_eq!(card["permission_mode"], "full-access");
    let id = card["id"].as_i64().expect("card id");

    // Edit flips to the other end of the ladder and a different approval preset.
    let edited = json_output(&td.board(&[
        "card",
        "edit",
        &id.to_string(),
        "--effort",
        "max",
        "--permission",
        "ask-for-approval",
        "--json",
    ]));
    assert_eq!(edited["effort"], "max");
    assert_eq!(edited["permission_mode"], "ask-for-approval");
    assert_eq!(edited["harness"], "codex");
}

#[test]
fn card_new_rejects_codex_out_of_catalog_values() {
    let td = TestDaemon::start(&[]);

    // `ultra` is deliberately not in this version's codex ladder — the CLI
    // rejects it up front, listing the exact canonical board efforts.
    let out = td.board(&[
        "card",
        "new",
        "--title",
        "bad effort",
        "--harness",
        "codex",
        "--effort",
        "ultra",
    ]);
    assert!(!out.status.success(), "ultra must be rejected for codex");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        err.trim_end(),
        "board: invalid effort 'ultra' (expected: off, minimal, low, medium, high, xhigh, max)",
        "the CLI's effort vocabulary is the full board ladder, off first; no ultra"
    );

    // A Claude-only permission mode is not a codex approval preset.
    let out = td.board(&[
        "card",
        "new",
        "--title",
        "bad perm",
        "--harness",
        "codex",
        "--permission",
        "acceptEdits",
    ]);
    assert!(
        !out.status.success(),
        "claude modes must be rejected for codex"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("permission mode 'acceptEdits' is not accepted"),
        "names the rejected permission; got: {err}"
    );

    // The old codex-internal approval ids are gone: the board-facing
    // vocabulary is the three presets, so `untrusted` is rejected up front.
    let out = td.board(&[
        "card",
        "new",
        "--title",
        "legacy perm",
        "--harness",
        "codex",
        "--permission",
        "untrusted",
    ]);
    assert!(
        !out.status.success(),
        "untrusted is a codex-internal id, not a board-facing preset"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("permission mode 'untrusted' is not accepted"),
        "names the rejected permission; got: {err}"
    );
}

#[test]
fn column_create_codex_overrides_persist_and_clear() {
    let td = TestDaemon::start(&[]);

    let created = json_output(&td.board(&[
        "column",
        "create",
        "--name",
        "Codex stage",
        "--harness",
        "codex",
        "--model",
        "gpt-5.2-codex",
        "--effort",
        "minimal",
        "--permission",
        "ask-for-approval",
        "--json",
    ]));
    let id = created["id"].as_i64().expect("created column id");
    assert_eq!(created["harness_override"], "codex");
    assert_eq!(created["model_override"], "gpt-5.2-codex");
    assert_eq!(created["effort_override"], "minimal");
    assert_eq!(created["permission_override"], "ask-for-approval");

    let edited = json_output(&td.board(&[
        "column",
        "edit",
        &id.to_string(),
        "--effort",
        "off",
        "--permission",
        "full-access",
        "--json",
    ]));
    assert_eq!(edited["effort_override"], "off");
    assert_eq!(edited["permission_override"], "full-access");
    assert_eq!(edited["harness_override"], "codex");

    let cleared = json_output(&td.board(&[
        "column",
        "edit",
        &id.to_string(),
        "--clear-harness",
        "--clear-model",
        "--clear-effort",
        "--clear-permission",
        "--json",
    ]));
    assert_eq!(cleared["harness_override"], Value::Null);
    assert_eq!(cleared["model_override"], Value::Null);
    assert_eq!(cleared["effort_override"], Value::Null);
    assert_eq!(cleared["permission_override"], Value::Null);

    // The per-card-only opt-in stays rejected at the column boundary for
    // codex too (the catalog is never consulted for it).
    let out = td.board(&[
        "column",
        "create",
        "--name",
        "Bad",
        "--harness",
        "codex",
        "--permission",
        "bypassPermissions",
    ]);
    assert!(
        !out.status.success(),
        "bypassPermissions is never a column override"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("bypassPermissions"), "got: {err}");
}

#[test]
fn card_archive_and_restore_cli_roundtrip() {
    let td = TestDaemon::start(&[]);
    let card = json_output(&td.board(&[
        "card",
        "new",
        "--title",
        "archive me",
        "--harness",
        "fake",
        "--json",
    ]));
    let id = card["id"].as_i64().unwrap().to_string();

    let archived = json_output(&td.board(&["card", "archive", &id, "--json"]));
    assert!(archived["archived_at"].is_string());

    let restored = json_output(&td.board(&["card", "restore", &id, "--json"]));
    assert!(restored["archived_at"].is_null());
}

#[test]
fn card_new_with_session_persists_and_shows() {
    let td = TestDaemon::start(&[]);
    // Create a card with an explicit --session (into the manual Todo column, so
    // no dispatch / herdr is needed).
    let out = td.board(&[
        "card",
        "new",
        "--title",
        "sessioned",
        "--harness",
        "fake",
        "--session",
        "my-sess",
        "--json",
    ]);
    let card = json_output(&out);
    assert_eq!(
        card["session"].as_str(),
        Some("my-sess"),
        "session persisted on the created card"
    );
    let id = card["id"].as_i64().expect("card id");

    // `card show` (human) surfaces the session.
    let out = td.board(&["card", "show", &id.to_string()]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("session: my-sess"),
        "card show renders the session; got:\n{text}"
    );
}

#[test]
fn template_apply_on_empty_board() {
    let td = TestDaemon::start(&[]);
    let mut c = td.client();
    let cols = c.template_apply("pipeline").unwrap();
    let names: Vec<&str> = cols.iter().map(|x| x.name.as_str()).collect();
    for expected in ["Todo", "Plan", "Execute", "Review", "Human Review", "Done"] {
        assert!(names.contains(&expected), "missing column {expected}");
    }
    let find = |n: &str| cols.iter().find(|x| x.name == n).unwrap();
    assert_eq!(find("Plan").on_success_column_id, Some(find("Execute").id));
    assert_eq!(find("Plan").on_fail_column_id, Some(find("Todo").id));
    assert_eq!(
        find("Review").on_success_column_id,
        Some(find("Human Review").id)
    );
    assert_eq!(find("Review").on_fail_column_id, Some(find("Execute").id));
    assert_eq!(find("Review").model_override.as_deref(), Some("opus"));
}

#[test]
fn template_refused_on_non_empty_board() {
    let td = TestDaemon::start(&[]);
    let mut c = td.client();
    let todo = todo_id(&mut c);
    c.card_create(&fake_card(todo)).unwrap();
    let err = c.template_apply("pipeline").unwrap_err();
    assert!(
        err.to_string().contains("error 3"),
        "expected invalid-state error, got: {err}"
    );
}

#[test]
fn harness_antigravity_models_lists_efforts_and_permissions() {
    let td = TestDaemon::start(&[]);

    // The daemon answers the antigravity down-state: no models (free-form —
    // there is deliberately no static fallback), the agy effort ladder, and
    // exactly the three permission modes.
    let out = td.board(&["harness", "models", "antigravity", "--json"]);
    let caps: board_core::capability::HarnessCapabilities =
        serde_json::from_value(json_output(&out)).unwrap();
    assert_eq!(caps.harness, "antigravity");
    assert!(caps.models.is_empty(), "catalog down → no models to offer");
    assert!(
        caps.model_freeform,
        "catalog down → free-form (stored models run)"
    );
    let efforts: Vec<&str> = caps.default_efforts.iter().map(|e| e.as_str()).collect();
    assert_eq!(
        efforts,
        vec!["low", "medium", "high"],
        "the agy effort ladder is exactly low|medium|high"
    );
    assert_eq!(
        caps.permission_modes,
        vec!["current", "sandbox", "always-proceed"]
    );

    // Human output mirrors the free-form down state.
    let out = td.board(&["harness", "models", "antigravity"]);
    assert!(out.status.success(), "human harness models should succeed");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("any model string accepted"),
        "human output states the free-form catalog: {text}"
    );

    // The permission vocabulary is its own verb.
    let out = td.board(&["harness", "permissions", "antigravity"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("current") && text.contains("sandbox") && text.contains("always-proceed"),
        "permissions output lists the three modes: {text}"
    );
}

#[test]
fn harness_antigravity_models_lists_live_catalog_when_agy_bin_resolves() {
    // With AGY_BIN pointing at a fixture CLI, the daemon overlays the live
    // normalized catalog: variant ids merge onto base models, fixed-effort
    // models carry no efforts, and the harness stops being free-form.
    use std::os::unix::fs::PermissionsExt;

    let fixture = r#"{
  "conversation_id": "",
  "status": "SUCCESS",
  "response": "",
  "command": {
    "name": "models",
    "data": {
      "models": [
        {"id": "gemini-3.7-flash-high", "label": "Gemini 3.7 Flash (High)"},
        {"id": "gemini-3.7-flash-medium", "label": "Gemini 3.7 Flash (Medium)"},
        {"id": "gemini-3.7-flash-low", "label": "Gemini 3.7 Flash (Low)"},
        {"id": "claude-sonnet-4-6", "label": "Claude Sonnet 4.6 (Thinking)"}
      ]
    }
  }
}
"#;
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("agy-fixture");
    std::fs::write(
        &bin,
        format!("#!/bin/sh\ncat <<'HBEOF'\n{fixture}\nHBEOF\n"),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let td = TestDaemon::start(&[("AGY_BIN", bin.to_str().unwrap())]);

    let out = td.board(&["harness", "models", "antigravity", "--json"]);
    let caps: board_core::capability::HarnessCapabilities =
        serde_json::from_value(json_output(&out)).unwrap();
    let ids: Vec<&str> = caps.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["claude-sonnet-4-6", "gemini-3.7-flash"],
        "normalized base models, sorted"
    );
    let gemini = caps
        .models
        .iter()
        .find(|m| m.id == "gemini-3.7-flash")
        .unwrap();
    let efforts: Vec<&str> = gemini.efforts.iter().map(|e| e.as_str()).collect();
    assert_eq!(efforts, vec!["low", "medium", "high"]);
    let sonnet = caps
        .models
        .iter()
        .find(|m| m.id == "claude-sonnet-4-6")
        .unwrap();
    assert!(sonnet.efforts.is_empty(), "fixed-effort model → no efforts");
    assert!(
        !caps.model_freeform,
        "catalog up → authoritative model list"
    );
}
