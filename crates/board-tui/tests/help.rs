//! B9: keep `view::HELP_KEYS` honest against the real key handlers.
//!
//! The table used to be hand-maintained with nothing tying it to
//! `src/app/*.rs`, and it had silently drifted (`y`/`n` on the confirm sheet,
//! `q` on card detail, `R` on the board were all handled but undocumented).
//! This test reads the handler sources and fails when a handled character key
//! has no row in the table.
//!
//! Matching is on the *characters* of a row's key column, case-insensitively:
//! rows spell keys the way a human reads them (`H / L`, `Ctrl+E`,
//! `←/→ Space`), so anything stricter would be a formatting test rather than a
//! coverage one. It is deliberately a coverage floor, not a spec — it cannot
//! tell you the description is *right*, only that the binding is not missing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use board_tui::view::HELP_KEYS;

fn app_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app")
}

/// Every `KeyCode::Char('x')` literal in a source file.
fn char_literals(source: &str) -> BTreeSet<char> {
    let mut out = BTreeSet::new();
    let mut rest = source;
    while let Some(at) = rest.find("KeyCode::Char('") {
        rest = &rest[at + "KeyCode::Char('".len()..];
        let mut chars = rest.chars();
        match (chars.next(), chars.next()) {
            // A plain literal: `'x'`.
            (Some(c), Some('\'')) => {
                out.insert(c);
            }
            // An escape: `'\''`, `'\\'`, … — none are bindings we document.
            (Some('\\'), _) => {}
            _ => {}
        }
    }
    out
}

/// The characters spelled by the key column of every row tagged with one of
/// `screens`, lowercased. `screens` is a list because two `Screen` variants can
/// share one handler (the card and column forms) — and because a handler that
/// binds a key globally is satisfied by any section.
fn documented_chars(screens: &[&str]) -> BTreeSet<char> {
    HELP_KEYS
        .iter()
        .filter(|(_, key, _)| *key != "--")
        .filter(|(screen, _, _)| screens.contains(&format!("{screen:?}").as_str()))
        .flat_map(|(_, key, _)| key.chars())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Which help sections may satisfy a given handler file. One file = one
/// screen's key handler, except `mod.rs` (the global pre-dispatch, satisfied by
/// any section) and `forms.rs` (shared by both form screens).
fn sections_for(file: &str) -> Option<Vec<&'static str>> {
    Some(match file {
        "board.rs" => vec!["Board"],
        "detail.rs" => vec!["CardDetail"],
        "forms.rs" => vec!["CardForm", "ColumnForm"],
        "picker.rs" => vec!["Picker"],
        "confirm.rs" => vec!["Confirm"],
        "help.rs" => vec!["Help"],
        "switcher.rs" => vec!["Switcher"],
        "move_column.rs" => vec!["MoveColumn"],
        "reorder_card.rs" => vec!["ReorderCard"],
        "comment_history.rs" => vec!["CommentHistory"],
        // `mod.rs` holds the global pre-dispatch (`?`) and `nav.rs` the shared
        // `↑/↓`+`k/j` decoder every list screen reads through. Neither belongs
        // to one screen, so any row documenting the key will do.
        "mod.rs" | "nav.rs" => vec![
            "Board",
            "CardDetail",
            "CardForm",
            "ColumnForm",
            "Picker",
            "Confirm",
            "Help",
            "Switcher",
            "MoveColumn",
            "ReorderCard",
            "CommentHistory",
        ],
        // `mouse.rs` *synthesizes* key events to reuse a screen's handler (for
        // example, the Card Detail comment `[ Edit ]` action replays `e`); it
        // binds nothing of its own, so its literals are documented wherever the
        // real handler lives.
        "mouse.rs" => return None,
        // Pure state, effect, and drag-lifecycle modules: no key handling at
        // all, so there is nothing here to document.
        "state.rs" | "effect.rs" | "drag.rs" => return None,
        other => panic!(
            "src/app/{other} is a new key handler with no help section mapped — \
             add it to `sections_for` and give it rows in view::HELP_KEYS"
        ),
    })
}

#[test]
fn every_handled_character_key_is_documented_in_its_help_section() {
    let mut missing: Vec<String> = Vec::new();

    for entry in std::fs::read_dir(app_dir()).expect("src/app is readable") {
        let path = entry.expect("readable dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let Some(sections) = sections_for(&name) else {
            continue;
        };
        let documented = documented_chars(&sections);
        let source = std::fs::read_to_string(&path).expect("readable source");
        for ch in char_literals(&source) {
            let lowered = ch.to_lowercase().next().unwrap_or(ch);
            if !documented.contains(&lowered) {
                missing.push(format!(
                    "{name}: KeyCode::Char({ch:?}) — not in the {sections:?} help section"
                ));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "these key handlers have no row in view::HELP_KEYS — add one (or remove \
         the binding):\n  {}",
        missing.join("\n  ")
    );
}

/// The screen tag is what makes the table auditable per handler, so every
/// screen that binds character keys must own at least one documented row.
#[test]
fn every_screen_with_bindings_has_help_rows() {
    use board_tui::app::Screen;

    let screens_with_rows: BTreeSet<String> = HELP_KEYS
        .iter()
        .filter(|(_, key, _)| *key != "--")
        .map(|(screen, _, _)| format!("{screen:?}"))
        .collect();

    // `ColumnForm` shares `CardForm`'s handler and its help section.
    for screen in [
        Screen::Board,
        Screen::CardDetail,
        Screen::CardForm,
        Screen::Picker,
        Screen::MoveColumn,
        Screen::ReorderCard,
        Screen::Confirm,
        Screen::Help,
        Screen::Switcher,
        Screen::CommentHistory,
    ] {
        assert!(
            screens_with_rows.contains(&format!("{screen:?}")),
            "{screen:?} handles keys but has no row in view::HELP_KEYS"
        );
    }
}

/// Separator rows are structural: they must carry the `--` key sentinel the
/// renderers switch on, and never be mistaken for a binding.
#[test]
fn separator_rows_are_well_formed() {
    for (_, key, description) in HELP_KEYS {
        if *key == "--" {
            assert!(
                description.starts_with("--") && description.ends_with("--"),
                "section heading {description:?} is not wrapped in dashes"
            );
        }
    }
}
