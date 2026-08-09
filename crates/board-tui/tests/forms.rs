use board_core::capability::{
    claude_capabilities, codex_capabilities, pi_capabilities, HarnessCapabilities, ModelInfo,
};
use board_core::engine::{validate_card_space, validate_column_permission_override};
use board_core::protocol::{Effort, SpaceKind};
use board_tui::forms::{FieldId, FieldKind, Form, Submit};
use board_tui::testkit::{choice_labels, field, field_index as idx_of, set_choice};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// KNOWN BUG (documented, not fixed here).
///
/// Every choice change rebuilds the *whole* field list: `on_model_changed` /
/// `on_space_kind_changed` / `apply_options` all funnel into
/// `Form::rebuild_card_fields`, which snapshots each text field as a `String`
/// and hands it to `Field::text` → `new_textarea`, constructing a brand-new
/// `TextArea`. The text survives; the cursor position and the undo/redo
/// history do not. Typing a description, moving the cursor, then cycling the
/// model selector silently teleports the caret back to (0, 0).
///
/// Fixing it means rebuilding *options* without rebuilding *fields* — i.e. the
/// declarative field-spec rewrite `src/forms/` needs anyway (adding one field
/// today touches ~5 places). That is deliberately out of scope for a
/// code-motion pass, so this test is `#[ignore]`d rather than deleted: it is
/// the executable statement of the defect.
#[test]
#[ignore = "known bug: a choice change rebuilds every field, resetting the text \
            editors' cursor and undo history; needs the forms declarative-spec \
            rewrite"]
fn cycling_a_choice_keeps_the_description_cursor_where_the_user_left_it() {
    let mut form = Form::card_create(1);

    let desc = idx_of(&form, FieldId::Description);
    let FieldKind::Text(ta) = &mut form.fields[desc].kind else {
        panic!("Description is a text field");
    };
    for c in "hello world".chars() {
        ta.input(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
    }
    assert_eq!(
        ta.cursor(),
        (0, 11),
        "precondition: caret at end of typed text"
    );

    // A pure selector change. It must not touch the text editors at all.
    let model = idx_of(&form, FieldId::Model);
    form.fields[model].cycle(1);
    form.on_model_changed();

    let desc = idx_of(&form, FieldId::Description);
    let FieldKind::Text(ta) = &form.fields[desc].kind else {
        panic!("Description is a text field");
    };
    assert_eq!(ta.lines().join("\n"), "hello world", "text is preserved");
    assert_eq!(
        ta.cursor(),
        (0, 11),
        "the caret must survive a selector change — a rebuilt TextArea resets it"
    );
}

#[test]
fn card_harness_select_consumes_harness_list() {
    // The card Harness selector draws from the shared `harness.list` source
    // (Form::harnesses) — the same source as the column harness_override
    // selector — so config-defined harnesses appear there too, with pi first
    // (the card default).
    let mut form = Form::card_create(1);
    let before = choice_labels(&form, FieldId::Harness);
    assert_eq!(
        before,
        vec!["pi".to_string(), "claude".to_string(), "codex".to_string()],
        "the built-in catalog includes codex after claude"
    );
    form.apply_options(
        None,
        Some(vec!["pi".into(), "claude".into(), "fake".into()]),
        None,
        None,
    );
    let after = choice_labels(&form, FieldId::Harness);
    assert_eq!(
        after,
        vec!["pi".to_string(), "claude".to_string(), "fake".to_string()]
    );
}

#[test]
fn column_harness_override_is_select_with_builtins() {
    // Before any fetch, harness_override is already a Choice (not free text)
    // seeded with the built-ins + a leading `(none)`.
    let form = Form::column_create(&[]);
    let labels = choice_labels(&form, FieldId::HarnessOverride);
    assert!(labels.first().is_some_and(|l| l == "none"));
    assert!(labels.contains(&"pi".to_string()));
    assert!(labels.contains(&"claude".to_string()));
}

#[test]
fn column_harness_override_select_includes_config_defined() {
    // A harness.list fetch advertising a config-defined harness adds it.
    let mut form = Form::column_create(&[]);
    form.apply_options(
        None,
        Some(vec!["claude".into(), "pi".into(), "fake".into()]),
        None,
        None,
    );
    let labels = choice_labels(&form, FieldId::HarnessOverride);
    assert!(labels.contains(&"fake".to_string()));
}

#[test]
fn column_permission_override_hidden_for_pi_shown_for_claude() {
    // Default (no override) resolves to Pi → permission_override hidden.
    let mut form = Form::column_create(&[]);
    form.apply_options(Some(pi_capabilities()), None, None, None);
    assert!(!form.field_visible(idx_of(&form, FieldId::PermissionOverride)));

    // Switching the override to claude (and loading its caps) shows it.
    form.apply_options(Some(claude_capabilities()), None, None, None);
    assert!(form.field_visible(idx_of(&form, FieldId::PermissionOverride)));
    // And its modes come from the catalog, not a hardcoded list.
    let modes = choice_labels(&form, FieldId::PermissionOverride);
    assert!(modes.contains(&"acceptEdits".to_string()));
    assert!(modes.contains(&"plan".to_string()));
}

#[test]
fn column_override_permission_menu_never_offers_a_value_the_validator_rejects() {
    // The column-override selector is filtered by the SAME validator the
    // daemon applies (`validate_column_permission_override`), so it can never
    // offer a value a `column.create`/`column.update` would be refused for.
    // `bypassPermissions` is exactly that value: a per-card opt-in only.
    let mut form = Form::column_create(&[]);
    form.apply_options(Some(claude_capabilities()), None, None, None);
    let modes = choice_labels(&form, FieldId::PermissionOverride);
    assert!(
        modes.contains(&"acceptEdits".to_string()),
        "catalog modes are still offered: {modes:?}"
    );
    for mode in &modes {
        if mode == "(default)" {
            continue;
        }
        assert!(
            validate_column_permission_override(Some(mode)).is_ok(),
            "column-override selector offers {mode:?}, which the validator rejects"
        );
    }
    assert!(!modes.contains(&"bypassPermissions".to_string()));

    // The card Permission selector is a different context and keeps it.
    let mut card = Form::card_create(1);
    set_choice(&mut card, FieldId::Harness, "claude");
    card.apply_options(Some(claude_capabilities()), None, None, None);
    assert!(choice_labels(&card, FieldId::Permission).contains(&"bypassPermissions".to_string()));
}

/// A2: with no capability catalog fetched yet, the effort/permission menus and
/// the permission field's visibility come from `default_capabilities`, not
/// from a hardcoded `harness != "pi"` comparison.
#[test]
fn card_selectors_fall_back_to_default_capabilities_per_harness() {
    // pi: full effort ladder, no permission modes → field hidden.
    let mut pi = Form::card_create(1);
    assert_eq!(
        choice_labels(&pi, FieldId::Effort),
        vec![
            "(default)",
            "off",
            "minimal",
            "low",
            "medium",
            "high",
            "xhigh",
            "max"
        ]
    );
    assert!(!pi.field_visible(idx_of(&pi, FieldId::Permission)));
    // …and submit sends no permission_mode for a harness that has none.
    if let Some(f) = pi.fields.iter_mut().find(|f| f.id == FieldId::Title) {
        f.set_text("t");
    }
    match pi.submit().unwrap() {
        Submit::CardCreate(p) => assert_eq!(p.permission_mode, None),
        _ => panic!("expected CardCreate"),
    }

    // claude: its own five-rung ladder and its own permission enum.
    let mut claude = Form::card_create(1);
    set_choice(&mut claude, FieldId::Harness, "claude");
    // Rebuild against the new harness without any fetch succeeding.
    claude.apply_options(None, None, None, None);
    assert_eq!(
        choice_labels(&claude, FieldId::Effort),
        vec!["(default)", "low", "medium", "high", "xhigh", "max"]
    );
    assert!(claude.field_visible(idx_of(&claude, FieldId::Permission)));
    assert_eq!(
        choice_labels(&claude, FieldId::Permission),
        claude_capabilities()
            .permission_modes
            .iter()
            .map(String::as_str)
            .fold(vec!["(default)"], |mut acc, m| {
                acc.push(m);
                acc
            })
    );

    // An unknown harness: permissive about efforts (any string is plausible),
    // fail-closed about permission modes — another CLI's enum is never a safe
    // guess, so the selector is hidden until real caps arrive.
    let mut unknown = Form::card_create(1);
    unknown.apply_options(None, Some(vec!["pi".into(), "mystery".into()]), None, None);
    set_choice(&mut unknown, FieldId::Harness, "mystery");
    unknown.apply_options(None, None, None, None);
    assert_eq!(unknown.current_harness(), "mystery");
    assert!(!unknown.field_visible(idx_of(&unknown, FieldId::Permission)));
    assert_eq!(
        choice_labels(&unknown, FieldId::Effort),
        vec![
            "(default)",
            "off",
            "minimal",
            "low",
            "medium",
            "high",
            "xhigh",
            "max"
        ]
    );

    // Real caps from the daemon always win over the built-in default: once
    // `harness.capabilities` answers for the config-defined harness, its
    // permission vocabulary appears.
    unknown.apply_options(
        Some(HarnessCapabilities {
            harness: "mystery".into(),
            models: vec![],
            model_freeform: true,
            default_efforts: vec![Effort::Low],
            permission_modes: vec!["ask".into()],
            resume: Default::default(),
        }),
        None,
        None,
        None,
    );
    assert!(unknown.field_visible(idx_of(&unknown, FieldId::Permission)));
    assert_eq!(
        choice_labels(&unknown, FieldId::Permission),
        vec!["(default)", "ask"]
    );
}

#[test]
fn card_codex_selectors_show_full_ladder_and_approval_modes() {
    // The codex catalog drives the guided selectors: the full effort ladder
    // with `off` first (mapped to codex `none` only at argv time), and the
    // three user-facing approval presets. Sandbox is not a permission mode.
    let mut form = Form::card_create(1);
    set_choice(&mut form, FieldId::Harness, "codex");
    form.apply_options(Some(codex_capabilities()), None, None, None);
    assert_eq!(form.current_harness(), "codex");
    assert_eq!(
        choice_labels(&form, FieldId::Effort),
        vec![
            "(default)".to_string(),
            "off".to_string(),
            "minimal".to_string(),
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
            "xhigh".to_string(),
            "max".to_string(),
        ],
        "the full ladder, off first"
    );
    assert!(
        form.field_visible(idx_of(&form, FieldId::Permission)),
        "codex has approval modes, so the permission field is visible"
    );
    assert_eq!(
        choice_labels(&form, FieldId::Permission),
        vec![
            "(default)".to_string(),
            "Ask for approval".to_string(),
            "Approve for me".to_string(),
            "Full access".to_string(),
        ]
    );

    // Submit carries the exact card overrides.
    form.fields
        .iter_mut()
        .find(|f| f.id == FieldId::Title)
        .unwrap()
        .set_text("codex task");
    set_choice(&mut form, FieldId::Effort, "off");
    set_choice(&mut form, FieldId::Permission, "Full access");
    match form.submit().unwrap() {
        Submit::CardCreate(p) => {
            assert_eq!(p.harness.as_deref(), Some("codex"));
            assert_eq!(p.effort, Some(Effort::Off));
            assert_eq!(p.permission_mode.as_deref(), Some("full-access"));
        }
        _ => panic!("expected CardCreate"),
    }
}

#[test]
fn card_codex_default_capabilities_before_fetch() {
    // Before any catalog fetch, selecting codex answers from board-core's
    // built-in snapshot (`default_capabilities`), not a hardcoded harness
    // comparison — same ladder and approval enum the daemon will confirm.
    let mut form = Form::card_create(1);
    set_choice(&mut form, FieldId::Harness, "codex");
    form.apply_options(None, None, None, None);
    assert_eq!(form.current_harness(), "codex");
    assert_eq!(
        choice_labels(&form, FieldId::Effort),
        vec![
            "(default)".to_string(),
            "off".to_string(),
            "minimal".to_string(),
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
            "xhigh".to_string(),
            "max".to_string(),
        ]
    );
    assert!(form.field_visible(idx_of(&form, FieldId::Permission)));
    assert_eq!(
        choice_labels(&form, FieldId::Permission),
        vec![
            "(default)".to_string(),
            "Ask for approval".to_string(),
            "Approve for me".to_string(),
            "Full access".to_string(),
        ]
    );
}

#[test]
fn column_codex_override_selectors_show_ladder_and_approval() {
    // Column override selectors share the card form's builders, so selecting
    // codex offers the same full ladder and approval enum as the card form.
    let mut form = Form::column_create(&[]);
    set_choice(&mut form, FieldId::HarnessOverride, "codex");
    form.apply_options(Some(codex_capabilities()), None, None, None);
    assert_eq!(form.current_harness(), "codex");
    assert_eq!(
        choice_labels(&form, FieldId::EffortOverride),
        vec![
            "(default)".to_string(),
            "off".to_string(),
            "minimal".to_string(),
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
            "xhigh".to_string(),
            "max".to_string(),
        ]
    );
    assert!(
        form.field_visible(idx_of(&form, FieldId::PermissionOverride)),
        "a codex override harness shows its approval selector"
    );
    assert_eq!(
        choice_labels(&form, FieldId::PermissionOverride),
        vec![
            "(default)".to_string(),
            "Ask for approval".to_string(),
            "Approve for me".to_string(),
            "Full access".to_string(),
        ]
    );

    // Submit carries the exact column overrides.
    form.fields
        .iter_mut()
        .find(|f| f.id == FieldId::Name)
        .unwrap()
        .set_text("Codex stage");
    set_choice(&mut form, FieldId::EffortOverride, "minimal");
    set_choice(&mut form, FieldId::PermissionOverride, "Ask for approval");
    match form.submit().unwrap() {
        Submit::ColumnCreate(p) => {
            assert_eq!(p.harness_override.as_deref(), Some("codex"));
            assert_eq!(p.effort_override.as_deref(), Some("minimal"));
            assert_eq!(p.permission_override.as_deref(), Some("ask-for-approval"));
        }
        _ => panic!("expected ColumnCreate"),
    }
}

/// A9: submit runs the core validators as a pre-flight, so the
/// "new_workspace needs BOTH a label and a cwd" rule is reported in the open
/// form instead of after a failed round-trip.
#[test]
fn new_workspace_submit_requires_both_ref_and_cwd() {
    let complete = |name: &str, cwd: &str| {
        let mut form = Form::card_create(1);
        set_choice(&mut form, FieldId::SpaceKind, "new workspace");
        form.on_space_kind_changed();
        for (id, text) in [
            (FieldId::Title, "t"),
            (FieldId::SpaceRef, name),
            (FieldId::SpaceCwd, cwd),
        ] {
            form.fields
                .iter_mut()
                .find(|f| f.id == id)
                .unwrap()
                .set_text(text);
        }
        form.submit().map(|_| ())
    };

    // Neither / only one of the two → the core message, no RPC attempted.
    let expected = validate_card_space(SpaceKind::NewWorkspace, None, None)
        .unwrap_err()
        .to_string();
    assert_eq!(complete("", ""), Err(expected.clone()));
    assert_eq!(complete("scratch", ""), Err(expected.clone()));
    assert_eq!(complete("", "/tmp/scratch"), Err(expected));
    // Both present → the form submits.
    assert_eq!(complete("scratch", "/tmp/scratch"), Ok(()));

    // A plain `workspace` space is unaffected by the rule.
    let mut ws = Form::card_create(1);
    ws.fields
        .iter_mut()
        .find(|f| f.id == FieldId::Title)
        .unwrap()
        .set_text("t");
    assert!(ws.submit().is_ok());
}

#[test]
fn column_effort_override_follows_catalog() {
    // A catalog exposing only `low` restricts the effort-override menu.
    let caps = HarnessCapabilities {
        harness: "fake".into(),
        models: vec![ModelInfo {
            id: "m".into(),
            efforts: vec![Effort::Low],
        }],
        model_freeform: true,
        default_efforts: vec![Effort::Low],
        permission_modes: vec![],
        resume: Default::default(),
    };
    let mut form = Form::column_create(&[]);
    form.apply_options(Some(caps), None, None, None);
    let labels = choice_labels(&form, FieldId::EffortOverride);
    // `(default)` plus the single declared effort.
    assert_eq!(labels, vec!["(default)".to_string(), "low".to_string()]);
}

#[test]
fn column_cascading_resets_invalid_effort_on_harness_change() {
    // Start on claude; its effort-override menu includes xhigh.
    let mut form = Form::column_create(&[]);
    form.apply_options(Some(claude_capabilities()), None, None, None);
    let before = choice_labels(&form, FieldId::EffortOverride);
    assert!(before.contains(&"xhigh".to_string()));

    // Switch to a harness whose only effort is `low`. After the rebuild the
    // stale `xhigh` is no longer offered (an invalid selection resets to the
    // default option), proving the menu follows the new harness.
    let caps = HarnessCapabilities {
        harness: "fake".into(),
        models: vec![ModelInfo {
            id: "m".into(),
            efforts: vec![Effort::Low],
        }],
        model_freeform: true,
        default_efforts: vec![Effort::Low],
        permission_modes: vec!["auto".into()],
        resume: Default::default(),
    };
    form.apply_options(Some(caps), None, None, None);
    let after = choice_labels(&form, FieldId::EffortOverride);
    assert!(!after.contains(&"xhigh".to_string()));
    assert!(after.contains(&"low".to_string()));
}

#[test]
fn column_options_rebuild_preserves_values_and_focus() {
    let mut form = Form::column_create(&[]);
    form.fields
        .iter_mut()
        .find(|field| field.id == FieldId::Name)
        .unwrap()
        .set_text("stage");
    form.fields
        .iter_mut()
        .find(|field| field.id == FieldId::SystemPrompt)
        .unwrap()
        .set_text("instructions");
    form.fields
        .iter_mut()
        .find(|field| field.id == FieldId::Timeout)
        .unwrap()
        .set_text("15");
    form.focus = idx_of(&form, FieldId::Timeout);

    form.apply_options(Some(pi_capabilities()), None, None, None);

    assert_eq!(form.focus, idx_of(&form, FieldId::Timeout));
    assert_eq!(field(&form, FieldId::Name).get_text(), "stage");
    assert_eq!(
        field(&form, FieldId::SystemPrompt).get_text(),
        "instructions"
    );
    assert_eq!(field(&form, FieldId::Timeout).get_text(), "15");
}

#[test]
fn column_submit_none_harness_override_extracts_none() {
    // `(none)` harness override extracts to `None` (no override).
    let mut form = Form::column_create(&[]);
    form.apply_options(None, None, None, None);
    // Set a name so submit passes the required-field check.
    if let Some(f) = form.fields.iter_mut().find(|f| f.id == FieldId::Name) {
        f.set_text("Col");
    }
    match form.submit().unwrap() {
        Submit::ColumnCreate(p) => assert_eq!(p.harness_override, None),
        _ => panic!("expected ColumnCreate"),
    }
}

// -- column system_prompt conditional on trigger ---------------------------

#[test]
fn column_system_prompt_hidden_for_manual_default() {
    // New column form defaults to trigger=manual → SystemPrompt is hidden, but
    // still present in the field list (so its value survives a submit).
    let form = Form::column_create(&[]);
    assert_eq!(field(&form, FieldId::Trigger).display(), "manual");
    assert!(!form.field_visible(idx_of(&form, FieldId::SystemPrompt)));
    assert!(form.fields.iter().any(|f| f.id == FieldId::SystemPrompt));
}

#[test]
fn column_system_prompt_shown_when_trigger_auto() {
    let mut form = Form::column_create(&[]);
    set_choice(&mut form, FieldId::Trigger, "auto");
    form.on_trigger_changed();
    assert!(form.field_visible(idx_of(&form, FieldId::SystemPrompt)));
}

#[test]
fn column_system_prompt_reappears_when_trigger_toggles() {
    let mut form = Form::column_create(&[]);
    assert!(!form.field_visible(idx_of(&form, FieldId::SystemPrompt)));

    set_choice(&mut form, FieldId::Trigger, "auto");
    form.on_trigger_changed();
    assert!(form.field_visible(idx_of(&form, FieldId::SystemPrompt)));

    set_choice(&mut form, FieldId::Trigger, "manual");
    form.on_trigger_changed();
    assert!(!form.field_visible(idx_of(&form, FieldId::SystemPrompt)));

    set_choice(&mut form, FieldId::Trigger, "auto");
    form.on_trigger_changed();
    assert!(form.field_visible(idx_of(&form, FieldId::SystemPrompt)));
}

#[test]
fn column_system_prompt_focus_moves_off_hidden_field() {
    // Focus the (visible) SystemPrompt under auto, then flip to manual: focus
    // must reconcile off the now-hidden field onto a still-visible one.
    let mut form = Form::column_create(&[]);
    set_choice(&mut form, FieldId::Trigger, "auto");
    form.on_trigger_changed();
    form.focus = idx_of(&form, FieldId::SystemPrompt);
    assert_eq!(form.focused().id, FieldId::SystemPrompt);

    set_choice(&mut form, FieldId::Trigger, "manual");
    form.on_trigger_changed();
    assert_ne!(form.focused().id, FieldId::SystemPrompt);
    assert!(form.field_visible(form.focus));
}

#[test]
fn column_submit_preserves_system_prompt_value_when_trigger_manual() {
    // Crux of "hide UI, preserve DB": a manual column carrying a prompt must
    // still submit Patch::Set(prompt) — NOT Patch::Clear — because the field is
    // hidden (value retained) rather than omitted from the form.
    use board_core::model::Column;
    use board_core::protocol::{Patch, Trigger};
    let col = Column {
        id: 7,
        board_id: 1,
        name: "Human Review".into(),
        position: 3,
        system_prompt: Some("queue for human".into()),
        trigger: Trigger::Manual,
        on_success_column_id: None,
        on_fail_column_id: None,
        fresh_session: false,
        harness_override: None,
        model_override: None,
        effort_override: None,
        permission_override: None,
        timeout_minutes: None,
    };
    let form = Form::column_edit(&col, &[]);
    assert!(!form.field_visible(idx_of(&form, FieldId::SystemPrompt)));
    assert_eq!(
        field(&form, FieldId::SystemPrompt).get_text(),
        "queue for human"
    );
    match form.submit().unwrap() {
        Submit::ColumnUpdate(p) => {
            assert_eq!(p.system_prompt, Patch::Set("queue for human".into()));
            assert_eq!(p.trigger, Some(Trigger::Manual));
        }
        _ => panic!("expected ColumnUpdate"),
    }
}

#[test]
fn column_submit_clears_system_prompt_only_when_emptied_under_auto() {
    // Sanity: an intentionally empty prompt under auto still clears on create,
    // so the preserve path can't silently swallow an explicit clear.
    let mut form = Form::column_create(&[]);
    set_choice(&mut form, FieldId::Trigger, "auto");
    form.on_trigger_changed();
    if let Some(f) = form.fields.iter_mut().find(|f| f.id == FieldId::Name) {
        f.set_text("Col");
    }
    match form.submit().unwrap() {
        Submit::ColumnCreate(p) => assert_eq!(p.system_prompt, None),
        _ => panic!("expected ColumnCreate"),
    }
}
