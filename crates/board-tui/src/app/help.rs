//! `?` help overlay key handling.
//!
//! `?` itself is bound globally in `app::on_key`, which records
//! `App::help_return_to` and resets `help_scroll` on the way in — so help is
//! reachable from every non-form screen and always closes back to the exact
//! screen it was opened from (card detail included).
//!
//! Both layouts scroll with `j`/`k`, because the table documents every
//! screen's bindings and no longer fits a fixed sheet. Regular/Wide otherwise
//! keeps its "any key closes" behaviour; Compact renders a single scrollable
//! column (see `view::overlays::draw_help_compact`) and closes only on
//! `Esc`/`q`, so its `j`/`k` stay available for reading.

use crossterm::event::{KeyCode, KeyEvent};

use crate::view::{
    help_content_width, help_list_rect, help_regular_max_scroll, help_wrapped_rows, LayoutMode,
};

use super::nav::{nav_delta, step_clamped};
use super::{App, Effect};

pub(super) fn help_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    // Regular/Wide keeps "any key closes", except that the table no longer
    // fits a fixed two-column sheet on a short terminal — so `j`/`k` scroll
    // there too rather than closing on the keys a user reaches for to read on.
    if app.layout_mode() != LayoutMode::Compact {
        match nav_delta(k.code) {
            Some(delta) => {
                let max = help_regular_max_scroll(app, app.last_area);
                app.help_scroll = step_clamped(app.help_scroll, delta, max);
            }
            None => app.screen = app.help_return_to,
        }
        return vec![];
    }
    if let Some(delta) = nav_delta(k.code) {
        let list_rect = help_list_rect(app, app.last_area);
        let width = help_content_width(list_rect);
        let total = help_wrapped_rows(width);
        let visible = list_rect.height.max(1) as usize;
        app.help_scroll = step_clamped(app.help_scroll, delta, total.saturating_sub(visible));
        return vec![];
    }
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => app.screen = app.help_return_to,
        _ => {}
    }
    vec![]
}
