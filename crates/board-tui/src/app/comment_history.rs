//! Comment-history sheet (`Screen::CommentHistory`) key handling: `j`/`k`
//! scroll clamped to the rendered content, `Esc`/`q` closes back to detail.

use crossterm::event::{KeyCode, KeyEvent};

use super::nav::{nav_delta, step_clamped};
use super::{App, Effect, Screen};

pub(super) fn comment_history_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    if let Some(delta) = nav_delta(k.code) {
        if app.comment_history.is_some() {
            let rect = crate::view::comment_history_rect(app, app.last_area);
            let width = rect.width.max(1);
            let visible = rect.height.max(1) as usize;
            if let Some(state) = app.comment_history.as_mut() {
                let total = crate::view::comment_history_wrapped_rows(&state.entries, width);
                state.scroll = step_clamped(state.scroll, delta, total.saturating_sub(visible));
            }
        }
        return vec![];
    }
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.comment_history = None;
            app.screen = Screen::CardDetail;
        }
        _ => {}
    }
    vec![]
}
