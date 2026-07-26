//! Comment-history sheet (`Screen::CommentHistory`) key handling: `j`/`k`
//! scroll clamped to the rendered content, `Esc`/`q` closes back to detail.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, Effect, Screen};

pub(super) fn comment_history_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    match k.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(state) = app.comment_history.as_mut() {
                state.scroll = state.scroll.saturating_sub(1);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.comment_history.is_some() {
                let rect = crate::view::comment_history_rect(app, app.last_area);
                let width = rect.width.max(1);
                let visible = rect.height.max(1) as usize;
                if let Some(state) = app.comment_history.as_mut() {
                    let total = crate::view::comment_history_wrapped_rows(&state.entries, width);
                    let max = total.saturating_sub(visible);
                    state.scroll = (state.scroll + 1).min(max);
                }
            }
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            app.comment_history = None;
            app.screen = Screen::CardDetail;
        }
        _ => {}
    }
    vec![]
}
