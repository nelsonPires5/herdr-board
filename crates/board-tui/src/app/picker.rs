use crossterm::event::{KeyCode, KeyEvent};

use super::nav::{nav_delta, step_clamped};
use super::{App, Confirm, ConfirmPurpose, Effect, PickerAction, PickerPurpose, PickerRow, Screen};

pub(super) fn picker_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Some(picker) = app.picker.as_mut() else {
        app.screen = Screen::Board;
        return vec![];
    };
    if let Some(delta) = nav_delta(k.code) {
        picker.sel = step_clamped(picker.sel, delta, picker.rows.len().saturating_sub(1));
        return vec![];
    }
    match k.code {
        KeyCode::Enter => {
            // A row list can be empty, so this indexes with `get`: an empty
            // picker's Enter does nothing rather than panicking.
            let Some(row) = picker.rows.get(picker.sel) else {
                return vec![];
            };
            let purpose = picker.purpose;
            let return_to = picker.return_to;
            let project_id = picker.project_id;
            // Owned copy of the row so the arms below can mutate `app` freely.
            let (label, item_id, row_action) = match row {
                PickerRow::Item(label, id) => (label.clone(), Some(*id), None),
                PickerRow::Action(label, action) => (label.clone(), None, Some(*action)),
            };
            return match (purpose, item_id, row_action) {
                // -- project picker -------------------------------------------
                (PickerPurpose::SwitchProject, Some(id), None) => {
                    if id == app.project.id {
                        app.picker = None;
                        app.screen = return_to;
                        return vec![];
                    }
                    // Choosing a project is not yet a selection side effect:
                    // the follow-up board picker collects the exact board, and
                    // only choosing one there emits `project.select`. The
                    // screen stays on the project picker so the board picker's
                    // `return_to` (read by `load_board_picker`) is this picker.
                    app.picker = None;
                    vec![Effect::LoadBoardPicker {
                        project_id: Some(id),
                    }]
                }
                (PickerPurpose::SwitchProject, None, Some(PickerAction::NewProject)) => {
                    app.picker = None;
                    app.screen = return_to;
                    open_project_create_form(app);
                    vec![]
                }
                // -- board picker ---------------------------------------------
                (PickerPurpose::SwitchBoard, Some(board_id), None) => {
                    app.picker = None;
                    app.screen = return_to;
                    if project_id == app.project.id {
                        vec![Effect::SelectBoard(board_id)]
                    } else {
                        vec![Effect::SelectProject {
                            project_id,
                            board_id,
                        }]
                    }
                }
                (PickerPurpose::SwitchBoard, None, Some(PickerAction::OtherProjects)) => {
                    // Same drill-down pattern as the project picker's item
                    // rows: the screen stays on the board picker so the
                    // project picker's `return_to` is this picker.
                    app.picker = None;
                    vec![Effect::LoadProjectPicker]
                }
                (PickerPurpose::SwitchBoard, None, Some(PickerAction::NewBoard)) => {
                    app.picker = None;
                    app.screen = return_to;
                    open_board_create_form(app, project_id);
                    vec![]
                }
                // -- delete-column relocation picker --------------------------
                (PickerPurpose::DeleteColumnMoveTo { column_id }, Some(target), None) => {
                    // Deleting a column that still holds cards is the
                    // destructive path, so it confirms — the same as the
                    // empty-column path, which has always confirmed. Picking
                    // a destination is not consent to the delete.
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
                // A purpose/row mismatch (e.g. an action row in the
                // delete-column picker, which never has one) is a no-op.
                _ => vec![],
            };
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

/// Open the project-create form where the picker was, so save/cancel lands
/// back on the same screen.
fn open_project_create_form(app: &mut App) {
    let return_to = app.screen;
    app.form = Some(crate::forms::Form::project_create().returning_to(return_to));
    app.screen = Screen::CardForm;
}

/// Open the board-create form with its Project field preselected to
/// `project_id`, so save/cancel lands back where the picker was.
fn open_board_create_form(app: &mut App, project_id: i64) {
    let return_to = app.screen;
    app.form = Some(
        crate::forms::Form::board_create(project_id, &app.projects, &app.project)
            .returning_to(return_to),
    );
    app.screen = Screen::CardForm;
}
