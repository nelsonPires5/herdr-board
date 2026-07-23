use crossterm::event::{KeyCode, KeyEvent};

use super::{App, ConfirmPurpose, Effect, Screen};

pub(super) fn confirm_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(confirm) = app.confirm.as_ref() else {
        app.screen = Screen::Board;
        return vec![];
    };
    match k.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            let purpose = confirm.purpose;
            app.confirm = None;
            match purpose {
                ConfirmPurpose::DeleteCard(id) => {
                    app.screen = Screen::Board;
                    vec![Effect::CardDelete(id)]
                }
                ConfirmPurpose::DeleteColumn(id) => {
                    app.screen = Screen::Board;
                    vec![Effect::ColumnDelete {
                        id,
                        move_cards_to: None,
                    }]
                }
                ConfirmPurpose::CancelRun(id) => {
                    app.screen = Screen::CardDetail;
                    vec![Effect::RunCancel(id)]
                }
            }
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            let back_detail = matches!(confirm.purpose, ConfirmPurpose::CancelRun(_));
            app.confirm = None;
            app.screen = if back_detail {
                Screen::CardDetail
            } else {
                Screen::Board
            };
            vec![]
        }
        _ => vec![],
    }
}
