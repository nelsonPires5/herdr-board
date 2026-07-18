//! Harness capability catalog + run-pane naming.

use board_core::capability::{
    available_harnesses, capabilities_for, claude_capabilities, meta_for, pi_capabilities,
    run_pane_name, run_pane_name_unique,
};
use board_core::config::Config;
use board_core::protocol::Effort;

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
    // Built-ins + config keys, sorted and de-duplicated.
    assert_eq!(
        available_harnesses(&cfg),
        vec!["alpha", "claude", "pi", "zeta"]
    );
}

#[test]
fn capabilities_match_trait_snapshot() {
    // The wire snapshot and the trait agree for every built-in.
    for h in ["pi", "claude"] {
        let cfg = Config::default();
        let via_fn = capabilities_for(h, &cfg).unwrap();
        let via_trait = {
            let m = meta_for(h, &cfg).unwrap();
            let snap = board_core::capability::HarnessCapabilities::from_meta(m.as_ref());
            snap
        };
        assert_eq!(via_fn, via_trait);
    }
}
