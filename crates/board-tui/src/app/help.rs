//! `?` help overlay key handling.
//!
//! Regular/Wide keep the original "any key closes" behaviour (the fixed
//! two-column layout always shows every entry, nothing to scroll). Compact
//! renders a single scrollable column instead (see `view::overlays::
//! draw_help_compact`), so it needs its own `j`/`k` scroll + `Esc`-only close.

use crossterm::event::{KeyCode, KeyEvent};

use crate::view::{help_content_width, help_list_rect, help_wrapped_rows, LayoutMode};

use super::{App, Effect, Screen};

pub(super) fn help_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    if app.layout_mode() != LayoutMode::Compact {
        app.screen = Screen::Board;
        return vec![];
    }
    match k.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.help_scroll = app.help_scroll.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let list_rect = help_list_rect(app, app.last_area);
            let width = help_content_width(list_rect);
            let total = help_wrapped_rows(width);
            let visible = list_rect.height.max(1) as usize;
            let max = total.saturating_sub(visible);
            app.help_scroll = (app.help_scroll + 1).min(max);
        }
        KeyCode::Esc => app.screen = Screen::Board,
        _ => {}
    }
    vec![]
}
