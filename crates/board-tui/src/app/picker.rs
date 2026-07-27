use board_core::protocol::CardMoveParams;
use crossterm::event::{KeyCode, KeyEvent};

use super::nav::{nav_delta, step_clamped};
use super::{App, Confirm, ConfirmPurpose, Effect, PickerPurpose, Screen};

pub(super) fn picker_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(picker) = app.picker.as_mut() else {
        app.screen = Screen::Board;
        return vec![];
    };
    if let Some(delta) = nav_delta(k.code) {
        picker.sel = step_clamped(picker.sel, delta, picker.options.len().saturating_sub(1));
        return vec![];
    }
    match k.code {
        KeyCode::Enter => {
            // An option list can be empty (a destination board with no
            // columns), so this indexes with `get`: an empty picker's Enter
            // does nothing rather than panicking.
            let Some(&(ref label, target)) = picker.options.get(picker.sel) else {
                return vec![];
            };
            let label = label.clone();
            let purpose = picker.purpose;
            let return_to = picker.return_to;
            app.picker = None;
            app.screen = return_to;
            return match purpose {
                PickerPurpose::SwitchBoard => vec![Effect::SwitchBoard(target)],
                PickerPurpose::MoveCardPickBoard { card_id } => {
                    vec![Effect::LoadColumnsForMove {
                        card_id,
                        board_id: target,
                    }]
                }
                PickerPurpose::MoveCardPickColumn { card_id, board_id } => {
                    vec![Effect::CardMove(CardMoveParams {
                        id: card_id,
                        column_id: target,
                        board_id: Some(board_id),
                        position: None,
                    })]
                }
                // Deleting a column that still holds cards is the destructive
                // path, so it confirms — the same as the empty-column path,
                // which has always confirmed. Picking a destination is not
                // consent to the delete.
                PickerPurpose::DeleteColumnMoveTo { column_id } => {
                    let moved = app
                        .board
                        .cards
                        .iter()
                        .filter(|card| card.column_id == column_id)
                        .count();
                    let plural = if moved == 1 { "card" } else { "cards" };
                    app.confirm = Some(Confirm {
                        message: format!("Delete column and move {moved} {plural} to {label}?"),
                        purpose: ConfirmPurpose::DeleteColumn {
                            id: column_id,
                            move_cards_to: Some(target),
                        },
                        return_to,
                    });
                    app.screen = Screen::Confirm;
                    vec![]
                }
            };
        }
        KeyCode::Char('b') => {
            // Inside a move-column picker, `b` switches to the destination-board
            // picker for a cross-board move. No-op for other picker purposes.
            if let PickerPurpose::MoveCardPickColumn { card_id, .. } = picker.purpose {
                let return_to = picker.return_to;
                app.picker = None;
                app.screen = return_to;
                return vec![Effect::LoadBoardsForMove { card_id }];
            }
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            let return_to = picker.return_to;
            app.picker = None;
            app.screen = return_to;
        }
        _ => {}
    }
    vec![]
}
