use board_core::protocol::CardMoveParams;
use crossterm::event::{KeyCode, KeyEvent};

use super::{App, Effect, PickerPurpose, Screen};

pub(super) fn picker_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(picker) = app.picker.as_mut() else {
        app.screen = Screen::Board;
        return vec![];
    };
    match k.code {
        KeyCode::Up | KeyCode::Char('k') => {
            if picker.sel > 0 {
                picker.sel -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if picker.sel + 1 < picker.options.len() {
                picker.sel += 1;
            }
        }
        KeyCode::Enter => {
            let (_, target) = picker.options[picker.sel];
            let purpose = picker.purpose;
            app.picker = None;
            app.screen = Screen::Board;
            return match purpose {
                PickerPurpose::SwitchBoard => vec![Effect::SwitchBoard(target)],
                PickerPurpose::MoveCard { card_id } => vec![Effect::CardMove(CardMoveParams {
                    id: card_id,
                    column_id: target,
                    position: None,
                })],
                PickerPurpose::DeleteColumnMoveTo { column_id } => vec![Effect::ColumnDelete {
                    id: column_id,
                    move_cards_to: Some(target),
                }],
            };
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            app.picker = None;
            app.screen = Screen::Board;
        }
        _ => {}
    }
    vec![]
}
