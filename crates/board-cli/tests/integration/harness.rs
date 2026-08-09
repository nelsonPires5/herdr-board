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
    // human: one harness per line, built-ins first (pi, claude, codex) then config.
    let out = td.board(&["harness", "list"]);
    assert!(out.status.success(), "harness list should succeed");
    let text = String::from_utf8_lossy(&out.stdout);
    let names: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(names, vec!["pi", "claude", "codex", "fake"], "got:\n{text}");

    // --json: the same names, default-first, as a JSON array.
    let out = td.board(&["harness", "list", "--json"]);
    let names: Vec<String> = serde_json::from_value(json_output(&out)).unwrap();
    assert_eq!(names, vec!["pi", "claude", "codex", "fake"]);
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
        "board: boardd error 2: not found: unknown harness 'ghost'; known: pi, claude, codex, fake",
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
        "not found: unknown harness 'ghost'; known: pi, claude, codex, fake"
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
