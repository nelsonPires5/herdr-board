//! Harness capability catalog + run-pane naming.

use board_core::capability::{
    anchor_label_for_tab, available_harnesses, capabilities_for, card_anchor_label,
    card_short_name, card_tab_label, claude_capabilities, default_capabilities, efforts_for,
    meta_for, pi_capabilities, resume_support_for, run_pane_name, run_pane_name_unique,
    HarnessCapabilities, ResumeSupport,
};
use board_core::config::Config;
use board_core::protocol::Effort;

#[test]
fn efforts_for_uses_model_policy_and_freeform_defaults() {
    let caps = HarnessCapabilities {
        harness: "test".into(),
        models: vec![board_core::capability::ModelInfo {
            id: "known".into(),
            efforts: vec![Effort::High],
        }],
        model_freeform: true,
        default_efforts: vec![Effort::Low],
        permission_modes: vec![],
        resume: Default::default(),
        default_effort_label: String::new(),
        default_permission_label: String::new(),
        default_model_label: String::new(),
    };
    assert_eq!(efforts_for(&caps, Some("known")), vec![Effort::High]);
    assert_eq!(
        efforts_for(&caps, Some("provider/custom")),
        vec![Effort::Low]
    );
    assert_eq!(efforts_for(&caps, None), vec![Effort::Low]);

    let mut legacy = caps.clone();
    legacy.default_efforts.clear();
    assert_eq!(
        efforts_for(&legacy, Some("provider/custom")),
        vec![Effort::High]
    );

    let pi = pi_capabilities();
    assert!(efforts_for(&pi, None).contains(&Effort::Minimal));
}

#[test]
fn claude_catalog_shape() {
    let caps = claude_capabilities();
    assert_eq!(caps.harness, "claude");
    assert!(caps.model_freeform);

    let ids: Vec<&str> = caps.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["fable", "opus", "sonnet", "haiku"]);

    // Every model carries all five efforts, ascending.
    for m in &caps.models {
        assert_eq!(
            m.efforts,
            vec![
                Effort::Low,
                Effort::Medium,
                Effort::High,
                Effort::Xhigh,
                Effort::Max
            ]
        );
    }

    assert_eq!(
        caps.permission_modes,
        vec![
            "acceptEdits",
            "auto",
            "bypassPermissions",
            "manual",
            "dontAsk",
            "plan"
        ]
    );
    assert_eq!(
        caps.default_efforts,
        vec![
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max
        ]
    );
}

#[test]
fn pi_capabilities_are_freeform_without_permissions() {
    let caps = pi_capabilities();
    assert_eq!(caps.harness, "pi");
    assert!(caps.models.is_empty());
    assert!(caps.model_freeform);
    assert!(caps.permission_modes.is_empty());
}

#[test]
fn pi_capabilities_expose_default_thinking_levels() {
    let caps = pi_capabilities();
    assert_eq!(
        caps.default_efforts,
        vec![
            Effort::Off,
            Effort::Minimal,
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max,
        ]
    );
}

#[test]
fn harness_efforts_pi_freeform_model_includes_low() {
    // A model Pi has never heard of is still accepted, and its effort ladder
    // falls back to Pi's defaults rather than to nothing.
    let caps = pi_capabilities();
    assert!(!caps
        .models
        .iter()
        .any(|model| model.id == "openai-codex/example"));
    assert!(efforts_for(&caps, Some("openai-codex/example")).contains(&Effort::Low));
}

#[test]
fn capabilities_for_builtin_and_unknown() {
    let cfg = Config::default();
    assert_eq!(
        capabilities_for("claude", &cfg),
        Some(claude_capabilities())
    );
    assert_eq!(capabilities_for("pi", &cfg), Some(pi_capabilities()));
    assert!(capabilities_for("nope", &cfg).is_none());
}

#[test]
fn capabilities_for_config_harness() {
    let toml = r#"
[harness.fake]
argv = ["bash", "/x.sh"]
models = ["big", "small"]
efforts = ["low", "high", "bogus"]
permission_modes = ["auto", "manual"]
"#;
    let cfg = Config::from_toml(toml).unwrap();
    let caps = capabilities_for("fake", &cfg).unwrap();
    assert_eq!(caps.harness, "fake");
    assert!(caps.model_freeform);
    assert_eq!(caps.models.len(), 2);
    // Unparseable efforts are dropped; the rest apply to every model.
    for m in &caps.models {
        assert_eq!(m.efforts, vec![Effort::Low, Effort::High]);
    }
    assert_eq!(caps.permission_modes, vec!["auto", "manual"]);
    assert_eq!(caps.default_efforts, vec![Effort::Low, Effort::High]);
}

#[test]
fn config_harness_without_capabilities_is_empty() {
    // A bare `[harness.x] argv=[…]` (pre-existing config) still resolves.
    let cfg = Config::from_toml("[harness.bare]\nargv = [\"x\"]\n").unwrap();
    let caps = capabilities_for("bare", &cfg).unwrap();
    assert!(caps.models.is_empty());
    assert!(caps.permission_modes.is_empty());
    assert!(caps.default_efforts.is_empty());
    assert!(caps.model_freeform);
}

#[test]
fn pane_name_basic_slug() {
    assert_eq!(run_pane_name(14, "Execute"), "card-14-execute");
    assert_eq!(run_pane_name(1, "In Progress"), "card-1-in-progress");
    assert_eq!(run_pane_name(7, "Code Review!!"), "card-7-code-review");
}

#[test]
fn pane_name_empty_slug_omits_part() {
    assert_eq!(run_pane_name(3, ""), "card-3");
    assert_eq!(run_pane_name(3, "   "), "card-3");
    assert_eq!(run_pane_name(3, "***"), "card-3");
}

#[test]
fn pane_name_truncates_to_24() {
    let name = run_pane_name(9, "abcdefghijklmnopqrstuv wxyz");
    let slug = name.strip_prefix("card-9-").unwrap();
    assert!(slug.len() <= 24, "slug too long: {slug}");
    assert!(!slug.ends_with('-'));
    // "...v" (22) + "-" + "w" fills exactly 24 chars.
    assert_eq!(slug, "abcdefghijklmnopqrstuv-w");
}

#[test]
fn pane_name_truncation_never_ends_on_dash() {
    // 23 alnum chars then a separator that would land at index 24 as a dash.
    let name = run_pane_name(9, "abcdefghijklmnopqrstuvw xyz");
    let slug = name.strip_prefix("card-9-").unwrap();
    assert!(!slug.ends_with('-'));
    assert_eq!(slug, "abcdefghijklmnopqrstuvw");
}

#[test]
fn pane_name_unique_adds_run_suffix() {
    assert_eq!(run_pane_name_unique(14, "Execute", 5), "card-14-execute-r5");
    assert_eq!(run_pane_name_unique(3, "", 2), "card-3-r2");
}

#[test]
fn card_short_name_kebab_cases_first_four_words() {
    assert_eq!(card_short_name("Fix login redirect"), "fix-login-redirect");
    assert_eq!(
        card_short_name("Ship the new onboarding flow today please"),
        "ship-the-new-onboarding"
    );
}

#[test]
fn card_short_name_tolerates_noise_and_caps_length() {
    assert_eq!(card_short_name("  Refactor   Auth!! "), "refactor-auth");
    assert_eq!(card_short_name(""), "");
    assert_eq!(card_short_name("***"), "");
    let long = card_short_name("abcdefghijklmnopqrstuvwxyz abcdefghijklmnopqrstuvwxyz");
    assert!(long.chars().count() <= 32, "slug too long: {long}");
    assert!(!long.ends_with('-'));
    assert_eq!(long, "abcdefghijklmnopqrstuvwxyz-abcde");
}

#[test]
fn card_short_name_handles_mixed_non_ascii_titles() {
    // Non-ASCII runs collapse to `-` like any other separator; ASCII words survive.
    assert_eq!(card_short_name("Fix caf\u{e9} bug"), "fix-caf-bug");
    assert_eq!(card_short_name("caf\u{e9} d\u{e9}j\u{e0} vu"), "caf-d-j-vu");
    assert_eq!(card_tab_label(7, "Fix caf\u{e9} bug"), "card-7 fix-caf-bug");
}

#[test]
fn card_short_name_entirely_non_ascii_yields_bare_tab_label() {
    // Entirely non-ASCII titles have no slug words: the tab stays bare `card-<id>`.
    assert_eq!(
        card_short_name("\u{65e5}\u{672c}\u{8a9e} \u{30c6}\u{30b9}\u{30c8}"),
        ""
    );
    assert_eq!(card_short_name("\u{1f680}\u{2728}"), "");
    assert_eq!(
        card_tab_label(9, "\u{65e5}\u{672c}\u{8a9e} \u{30c6}\u{30b9}\u{30c8}"),
        "card-9"
    );
    assert_eq!(card_tab_label(9, "\u{1f680}\u{2728}"), "card-9");
}

#[test]
fn anchor_label_stays_card_id_anchor_despite_tab_suffix() {
    // Regression: anchors must stay `card-<id>-anchor` independent of the
    // human-readable tab suffix (`card-7 fix-login-redirect` → `card-7-anchor`,
    // never `card-7 fix-login-redirect-anchor`).
    assert_eq!(card_anchor_label(7), "card-7-anchor");
    assert_eq!(
        anchor_label_for_tab(&card_tab_label(7, "Fix login redirect")),
        "card-7-anchor"
    );
    assert_eq!(anchor_label_for_tab("card-42"), "card-42-anchor");
    assert_eq!(
        anchor_label_for_tab("card-7 fix-login-redirect"),
        "card-7-anchor"
    );
    assert_eq!(anchor_label_for_tab("card-9"), "card-9-anchor");
}

#[test]
fn card_tab_label_shows_short_name_after_id() {
    assert_eq!(
        card_tab_label(7, "Fix login redirect"),
        "card-7 fix-login-redirect"
    );
    assert_eq!(card_tab_label(7, ""), "card-7");
    assert_eq!(card_tab_label(7, "***"), "card-7");
    // Ownership still keys on the `card-` prefix, never the label suffix.
    assert!(card_tab_label(7, "Fix login").starts_with("card-"));
}

// -- HarnessMeta trait -----------------------------------------------------

#[test]
fn trait_pi_has_no_models_or_permissions() {
    let m = meta_for("pi", &Config::default()).unwrap();
    assert_eq!(m.id(), "pi");
    assert!(m.models().is_empty());
    assert!(m.permissions().is_empty());
    assert!(m.model_freeform());
    // Default (None) efforts = the full Pi thinking ladder incl. off/minimal.
    let eff = m.efforts(None);
    assert!(eff.contains(&Effort::Off) && eff.contains(&Effort::Minimal));
}

#[test]
fn trait_claude_model_efforts_authoritative() {
    let m = meta_for("claude", &Config::default()).unwrap();
    assert_eq!(m.id(), "claude");
    // A known model carries its own efforts.
    let known = m.efforts(Some("sonnet"));
    assert_eq!(
        known,
        vec![
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max
        ]
    );
    // An unknown/free-form model still gets the default ladder.
    let unknown = m.efforts(Some("whatever"));
    assert!(!unknown.is_empty());
    // Permissions are non-empty → the column permission_override stays visible.
    assert!(!m.permissions().is_empty());
}

#[test]
fn trait_config_harness_resolves_and_is_freeform() {
    let toml = r#"
[harness.fake]
argv = ["bash", "/x.sh"]
models = ["big", "small"]
efforts = ["low", "high"]
permission_modes = ["auto"]
"#;
    let cfg = Config::from_toml(toml).unwrap();
    let m = meta_for("fake", &cfg).unwrap();
    assert_eq!(m.id(), "fake");
    assert!(m.model_freeform());
    assert_eq!(m.permissions(), vec!["auto".to_string()]);
    // A declared model's efforts come back exactly as declared.
    assert_eq!(m.efforts(Some("big")), vec![Effort::Low, Effort::High]);
    // Default efforts (None) are the parsed declared set.
    assert_eq!(m.efforts(None), vec![Effort::Low, Effort::High]);
}

#[test]
fn trait_meta_for_unknown_is_none() {
    assert!(meta_for("ghost", &Config::default()).is_none());
}

#[test]
fn available_harnesses_lists_builtins_and_config() {
    let toml = r#"
[harness.zeta]
argv = ["z"]
[harness.alpha]
argv = ["a"]
"#;
    let cfg = Config::from_toml(toml).unwrap();
    // Built-ins first in their default order (pi before claude, codex slotting
    // in right after claude, opencode after codex, antigravity last), then
    // config keys sorted and de-duplicated.
    assert_eq!(
        available_harnesses(&cfg),
        vec![
            "pi",
            "claude",
            "codex",
            "opencode",
            "antigravity",
            "alpha",
            "zeta"
        ]
    );
}

#[test]
fn builtin_capability_snapshots_match_pinned_expectations() {
    // Pinned expectations for every built-in, stated independently on both
    // the wire snapshot (`capabilities_for`) and the trait adapter
    // (`meta_for`) so a drift in either is caught. This replaces the old
    // self-compare (`capabilities_for` vs `from_meta`), which passed as long
    // as the two shared one implementation.
    let cfg = Config::default();
    let full_ladder = vec![
        Effort::Off,
        Effort::Minimal,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];
    let claude_ladder = vec![
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];
    let agy_ladder = vec![Effort::Low, Effort::Medium, Effort::High];

    // pi: free-form, no catalog, no permissions, full ladder, resumable.
    let pi = capabilities_for("pi", &cfg).unwrap();
    assert_eq!(pi.harness, "pi");
    assert!(pi.models.is_empty());
    assert!(pi.model_freeform);
    assert_eq!(pi.default_efforts, full_ladder);
    assert!(pi.permission_modes.is_empty());
    assert_eq!(pi.resume, ResumeSupport::ByConversationId);
    assert_eq!(pi.default_effort_label, "default effort");
    assert_eq!(pi.default_permission_label, "default permission");
    assert_eq!(pi.default_model_label, "default model");
    let m = meta_for("pi", &cfg).unwrap();
    assert_eq!(m.id(), "pi");
    assert!(m.models().is_empty());
    assert!(m.model_freeform());
    assert_eq!(m.efforts(None), full_ladder);
    assert!(m.permissions().is_empty());
    assert_eq!(m.resume(), ResumeSupport::ByConversationId);

    // claude: four aliases sharing the five-level ladder, six permission modes.
    let claude = capabilities_for("claude", &cfg).unwrap();
    assert_eq!(claude.harness, "claude");
    assert!(claude.model_freeform);
    let ids: Vec<&str> = claude.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["fable", "opus", "sonnet", "haiku"]);
    for model in &claude.models {
        assert_eq!(model.efforts, claude_ladder);
    }
    assert_eq!(claude.default_efforts, claude_ladder);
    assert_eq!(
        claude.permission_modes,
        vec![
            "acceptEdits",
            "auto",
            "bypassPermissions",
            "manual",
            "dontAsk",
            "plan"
        ]
    );
    assert_eq!(claude.resume, ResumeSupport::ByConversationId);
    let m = meta_for("claude", &cfg).unwrap();
    assert_eq!(m.id(), "claude");
    assert!(m.model_freeform());
    assert_eq!(m.efforts(Some("sonnet")), claude_ladder);
    assert_eq!(m.efforts(None), claude_ladder);
    assert_eq!(
        m.permissions(),
        vec![
            "acceptEdits".to_string(),
            "auto".to_string(),
            "bypassPermissions".to_string(),
            "manual".to_string(),
            "dontAsk".to_string(),
            "plan".to_string()
        ]
    );
    assert_eq!(m.resume(), ResumeSupport::ByConversationId);

    // codex: free-form, no catalog, three approval presets, full ladder.
    let codex = capabilities_for("codex", &cfg).unwrap();
    assert_eq!(codex.harness, "codex");
    assert!(codex.models.is_empty());
    assert!(codex.model_freeform);
    assert_eq!(codex.default_efforts, full_ladder);
    assert_eq!(
        codex.permission_modes,
        vec!["ask-for-approval", "approve-for-me", "full-access"]
    );
    assert_eq!(codex.resume, ResumeSupport::ByConversationId);
    let m = meta_for("codex", &cfg).unwrap();
    assert_eq!(m.id(), "codex");
    assert!(m.model_freeform());
    assert_eq!(m.efforts(None), full_ladder);
    assert_eq!(
        m.permissions(),
        vec![
            "ask-for-approval".to_string(),
            "approve-for-me".to_string(),
            "full-access".to_string()
        ]
    );
    assert_eq!(m.resume(), ResumeSupport::ByConversationId);

    // opencode: free-form plus the two-entry static fallback catalog.
    let opencode = capabilities_for("opencode", &cfg).unwrap();
    assert_eq!(opencode.harness, "opencode");
    assert!(opencode.model_freeform);
    assert_eq!(opencode.models.len(), 2);
    let nemotron = opencode
        .models
        .iter()
        .find(|m| m.id == "opencode/nemotron-3-ultra-free")
        .expect("fallback defines nemotron");
    assert!(nemotron.efforts.is_empty());
    let deepseek = opencode
        .models
        .iter()
        .find(|m| m.id == "opencode/deepseek-v4-flash-free")
        .expect("fallback defines deepseek");
    assert_eq!(
        deepseek.efforts,
        vec![Effort::Low, Effort::High, Effort::Max]
    );
    assert_eq!(opencode.default_efforts, full_ladder);
    assert_eq!(opencode.permission_modes, vec!["default", "auto-approve"]);
    assert_eq!(opencode.resume, ResumeSupport::ByConversationId);
    let m = meta_for("opencode", &cfg).unwrap();
    assert_eq!(m.id(), "opencode");
    assert!(m.model_freeform());
    assert_eq!(m.efforts(None), full_ladder);
    assert!(m.efforts(Some("opencode/nemotron-3-ultra-free")).is_empty());
    assert_eq!(
        m.efforts(Some("opencode/deepseek-v4-flash-free")),
        vec![Effort::Low, Effort::High, Effort::Max]
    );
    assert_eq!(
        m.permissions(),
        vec!["default".to_string(), "auto-approve".to_string()]
    );
    assert_eq!(m.resume(), ResumeSupport::ByConversationId);

    // antigravity with no live catalog: no models, free-form, three agy
    // levels, two permission modes, resumable.
    let agy = capabilities_for("antigravity", &cfg).unwrap();
    assert_eq!(agy.harness, "antigravity");
    assert!(agy.models.is_empty());
    assert!(agy.model_freeform);
    assert_eq!(agy.default_efforts, agy_ladder);
    assert_eq!(agy.permission_modes, vec!["sandbox", "always-proceed"]);
    assert_eq!(agy.resume, ResumeSupport::ByConversationId);
    let m = meta_for("antigravity", &cfg).unwrap();
    assert_eq!(m.id(), "antigravity");
    assert!(m.models().is_empty());
    assert!(m.model_freeform());
    assert_eq!(m.efforts(None), agy_ladder);
    assert_eq!(
        m.permissions(),
        vec!["sandbox".to_string(), "always-proceed".to_string()]
    );
    assert_eq!(m.resume(), ResumeSupport::ByConversationId);
}

// ---------------------------------------------------------------------------
// Resume capability (the dead-pane rescue asks before it tries)
// ---------------------------------------------------------------------------

#[test]
fn builtins_declare_resume_by_conversation_id() {
    let cfg = Config::default();
    for harness in ["pi", "claude"] {
        let meta = meta_for(harness, &cfg).unwrap();
        assert_eq!(meta.resume(), ResumeSupport::ByConversationId, "{harness}");
        // The trait answer and the wire snapshot never disagree.
        assert_eq!(
            capabilities_for(harness, &cfg).unwrap().resume,
            ResumeSupport::ByConversationId
        );
        assert!(resume_support_for(harness, &cfg).is_supported());
    }
}

#[test]
fn config_harness_defaults_to_no_resume_support() {
    // A `[harness.NAME]` table that says nothing about resuming is treated as
    // unable to resume: there is no universal syntax to guess at.
    let cfg = Config::from_toml("[harness.custom]\nargv = [\"c\"]\n").unwrap();
    assert_eq!(
        meta_for("custom", &cfg).unwrap().resume(),
        ResumeSupport::Unsupported
    );
    assert_eq!(
        capabilities_for("custom", &cfg).unwrap().resume,
        ResumeSupport::Unsupported
    );
    assert!(!resume_support_for("custom", &cfg).is_supported());
}

#[test]
fn config_harness_opts_into_resume_explicitly() {
    let cfg = Config::from_toml("[harness.custom]\nargv = [\"c\"]\nresume = true\n").unwrap();
    assert_eq!(
        meta_for("custom", &cfg).unwrap().resume(),
        ResumeSupport::ByConversationId
    );
    assert!(resume_support_for("custom", &cfg).is_supported());
    // `resume = false` is the same as omitting it.
    let off = Config::from_toml("[harness.custom]\nargv = [\"c\"]\nresume = false\n").unwrap();
    assert!(!resume_support_for("custom", &off).is_supported());
}

#[test]
fn unknown_harness_and_legacy_payloads_fail_closed_on_resume() {
    // An unknown harness answers "unsupported", not "unknown": for the rescue
    // both mean refuse, and there must be no separate unknown-case fallback.
    assert!(!resume_support_for("ghost", &Config::default()).is_supported());
    // A capability payload serialized before the field existed reads as
    // unsupported rather than defaulting to "sure, try it".
    let legacy: HarnessCapabilities = serde_json::from_value(serde_json::json!({
        "harness": "old", "models": [], "model_freeform": true, "permission_modes": []
    }))
    .unwrap();
    assert_eq!(legacy.resume, ResumeSupport::Unsupported);
    assert!(!legacy.resume.is_supported());
}

#[test]
fn default_capabilities_match_builtins_and_fail_closed_for_unknown() {
    assert_eq!(default_capabilities("pi"), pi_capabilities());
    assert_eq!(default_capabilities("claude"), claude_capabilities());

    // An unknown harness: permissive about models/efforts we cannot validate,
    // silent about permission modes and resuming, which we must not invent.
    let unknown = default_capabilities("mystery");
    assert_eq!(unknown.harness, "mystery");
    assert!(unknown.models.is_empty());
    assert!(unknown.model_freeform);
    assert_eq!(unknown.default_efforts.len(), 7);
    assert_eq!(
        efforts_for(&unknown, Some("anything")),
        unknown.default_efforts
    );
    assert!(unknown.permission_modes.is_empty());
    assert_eq!(unknown.resume, ResumeSupport::Unsupported);
    assert_eq!(
        unknown.resume,
        resume_support_for("mystery", &Config::default())
    );
}

#[test]
fn filtered_harnesses_show_only_installed_builtins() {
    use board_core::capability::filtered_available_harnesses;
    let cfg = Config::default();
    // Herdr reports only pi and codex as available → only those builtins survive.
    let installed = vec!["pi".to_string(), "codex".to_string()];
    assert_eq!(
        filtered_available_harnesses(&cfg, Some(&installed)),
        vec!["pi", "codex"]
    );
    // Antigravity is discovered via the `antigravity_cli` target.
    let agy = vec!["antigravity_cli".to_string()];
    assert_eq!(
        filtered_available_harnesses(&cfg, Some(&agy)),
        vec!["antigravity"]
    );
    // No Herdr reachability → graceful fallback to all builtins.
    assert_eq!(
        filtered_available_harnesses(&cfg, None),
        vec!["pi", "claude", "codex", "opencode", "antigravity"]
    );
    // Config-defined harnesses are always appended, sorted, and never filtered by Herdr.
    let toml = "[harness.zeta]\nargv = [\"z\"]\n[harness.alpha]\nargv = [\"a\"]\n";
    let cfg2 = Config::from_toml(toml).unwrap();
    assert_eq!(
        filtered_available_harnesses(&cfg2, Some(&installed)),
        vec!["pi", "codex", "alpha", "zeta"]
    );
    assert_eq!(
        filtered_available_harnesses(&cfg2, None),
        vec![
            "pi",
            "claude",
            "codex",
            "opencode",
            "antigravity",
            "alpha",
            "zeta"
        ]
    );
}

#[test]
fn config_section_under_a_builtin_name_never_readmits_it() {
    use board_core::capability::{available_harnesses, filtered_available_harnesses};
    // `[harness.claude]` is unreachable (`meta_for` resolves the builtin first), so
    // it must never re-add `claude` to a filtered list, and never appear twice in
    // the unfiltered fallback. A genuinely custom name is still appended.
    let toml = "[harness.claude]\nargv = [\"x\"]\n[harness.mine]\nargv = [\"m\"]\n";
    let cfg = Config::from_toml(toml).unwrap();
    let only_pi = vec!["pi".to_string()];
    assert_eq!(
        filtered_available_harnesses(&cfg, Some(&only_pi)),
        vec!["pi", "mine"],
        "the uninstalled builtin must stay out even with a colliding config section"
    );
    let pi_and_claude = vec!["pi".to_string(), "claude".to_string()];
    assert_eq!(
        filtered_available_harnesses(&cfg, Some(&pi_and_claude)),
        vec!["pi", "claude", "mine"],
        "the installed builtin appears exactly once"
    );
    assert_eq!(
        available_harnesses(&cfg),
        vec!["pi", "claude", "codex", "opencode", "antigravity", "mine"],
        "the unfiltered fallback also keeps one claude entry"
    );
}

#[test]
fn default_harness_picks_pi_or_first_installed() {
    use board_core::capability::default_harness_for;
    // pi present → pi is the default even though other harnesses exist.
    assert_eq!(
        default_harness_for(&["pi".to_string(), "codex".to_string()]),
        "pi"
    );
    // pi absent → first installed in canonical order.
    assert_eq!(
        default_harness_for(&["codex".to_string(), "opencode".to_string()]),
        "codex"
    );
    // Empty list (no builtin installed and no config) → last-resort pi.
    assert_eq!(default_harness_for(&[]), "pi");
    // Config-only list → first config harness.
    assert_eq!(
        default_harness_for(&["alpha".to_string(), "zeta".to_string()]),
        "alpha"
    );
}
