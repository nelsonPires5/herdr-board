use super::{
    board_picker_label, detail_section_title, pane_title, HELP_GUTTER_WIDTH, HELP_KEYS,
    HELP_KEY_WIDTH,
};
use crate::app::CardFilter;
use board_core::model::Board;

#[test]
fn pane_titles_include_scope_filter_and_sanitize_long_labels() {
    let global = Board {
        id: 1,
        name: "Global".into(),
        scope_path: None,
    };
    assert_eq!(
        pane_title(&global, CardFilter::Active),
        "Board [Global · ACTIVE]"
    );

    let scoped = Board {
        id: 2,
        name: "/tmp/repo".into(),
        scope_path: Some("/tmp/a[unsafe]/abcdefghijklmnopqrstuvwxyz0123456789".into()),
    };
    let title = pane_title(&scoped, CardFilter::Archived);
    assert!(title.starts_with("Board [abcdefghijklmnopqrstuvwxyz01234"));
    assert!(title.ends_with("… · ARCHIVED]"));
    assert!(!title.contains('[') || title.starts_with("Board ["));
    assert_eq!(
        board_picker_label(&scoped),
        "abcdefghijklmnopqrstuvwxyz01234… — /tmp/a(unsafe)/abcdefghijklmnopqrstuvwxyz0123456789"
    );
}

#[test]
fn detail_titles_show_only_overflow_arrows() {
    assert_eq!(detail_section_title("comments", 3, 0, 3), "comments");
    assert_eq!(detail_section_title("comments", 8, 0, 3), "comments ↓");
    assert_eq!(detail_section_title("comments", 8, 2, 3), "comments ↑↓");
    assert_eq!(detail_section_title("runs", 8, 5, 3), "runs ↑");
}

#[test]
fn help_descriptions_fit_each_80_column_panel_column() {
    let inner_width = 80_u16 - 2;
    let column_width = (inner_width - HELP_GUTTER_WIDTH) / 2;
    let description_width = column_width - HELP_KEY_WIDTH;
    for (key, description) in HELP_KEYS {
        if *key != "--" {
            assert!(
                description.chars().count() <= description_width as usize,
                "{key} description does not fit: {description}"
            );
        }
    }
}
