//! Yes/no confirmation sheet.
//!
//! Both answers land on `Confirm::return_to`, the screen recorded when the
//! sheet was opened. Nothing here re-derives a destination from `purpose`:
//! the same confirmation can be raised from more than one screen, and only
//! the opener knows where "back" is.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, ConfirmPurpose, Effect, Screen};

pub(super) fn confirm_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(confirm) = app.confirm.as_ref() else {
        app.screen = Screen::Board;
        return vec![];
    };
    let return_to = confirm.return_to;
    match k.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            let purpose = confirm.purpose;
            app.confirm = None;
            app.screen = return_to;
            match purpose {
                ConfirmPurpose::DeleteCard(id) => vec![Effect::CardDelete(id)],
                ConfirmPurpose::DeleteColumn { id, move_cards_to } => {
                    vec![Effect::ColumnDelete { id, move_cards_to }]
                }
                ConfirmPurpose::CancelRun(id) => vec![Effect::RunCancel(id)],
                ConfirmPurpose::RetryRun(id) => vec![Effect::RunRetry(id)],
                ConfirmPurpose::DeleteComment(id) => vec![Effect::CommentDelete { id }],
            }
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.confirm = None;
            app.screen = return_to;
            vec![]
        }
        _ => vec![],
    }
}
