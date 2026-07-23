use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::forms::{FieldId, FieldKind, Submit};

use super::{App, Effect, Screen};

pub(super) fn form_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    if app.form.is_none() {
        app.screen = Screen::Board;
        return vec![];
    }

    // Ctrl+E: hand a multiline text field to $EDITOR.
    if k.code == KeyCode::Char('e') && k.modifiers.contains(KeyModifiers::CONTROL) {
        let multiline = app
            .form
            .as_ref()
            .map(|f| f.focused_is_multiline())
            .unwrap_or(false);
        return if multiline {
            vec![Effect::EditFocusedTextArea]
        } else {
            vec![]
        };
    }

    match k.code {
        KeyCode::Esc => close_form(app, false),
        KeyCode::Enter => return submit_form(app),
        KeyCode::Tab => app.form.as_mut().unwrap().focus_step(1),
        KeyCode::BackTab => app.form.as_mut().unwrap().focus_step(-1),
        _ => {
            let form = app.form.as_mut().unwrap();
            if form.focused_is_choice() {
                let delta = match k.code {
                    KeyCode::Left | KeyCode::Up => Some(-1),
                    KeyCode::Right | KeyCode::Down | KeyCode::Char(' ') => Some(1),
                    _ => None,
                };
                if let Some(delta) = delta {
                    let fid = form.focused().id;
                    form.focused_mut().cycle(delta);
                    // A changed harness needs fresh capabilities; a changed
                    // session needs its own workspace list; a changed column
                    // harness-override needs its own capabilities; model/space-
                    // kind changes reshape the dependent selectors in place.
                    match fid {
                        FieldId::Harness | FieldId::Session | FieldId::HarnessOverride => {
                            return vec![Effect::LoadFormOptions]
                        }
                        FieldId::Model => form.on_model_changed(),
                        FieldId::SpaceKind => form.on_space_kind_changed(),
                        _ => {}
                    }
                }
            } else if let FieldKind::Text(ta) = &mut form.focused_mut().kind {
                // Enter/Tab/Esc are handled above; everything else is editing.
                ta.input(k);
            }
        }
    }
    vec![]
}

fn submit_form(app: &mut App) -> Vec<Effect> {
    let Some(form) = app.form.as_ref() else {
        return vec![];
    };
    match form.submit() {
        Ok(submit) => {
            let board_id = app.board.board.id;
            let effects = match submit {
                Submit::CardCreate(mut p) => {
                    p.board_id = Some(board_id);
                    vec![Effect::CardCreate(p)]
                }
                Submit::CardUpdate(p) => vec![Effect::CardUpdate(p)],
                Submit::ColumnCreate(mut p) => {
                    p.board_id = Some(board_id);
                    vec![Effect::ColumnCreate(p)]
                }
                Submit::ColumnUpdate(p) => vec![Effect::ColumnUpdate(p)],
                Submit::Comment { card_id, body } => vec![Effect::CommentAdd { card_id, body }],
            };
            close_form(app, true);
            effects
        }
        Err(msg) => {
            app.set_toast(msg, true);
            vec![]
        }
    }
}

fn close_form(app: &mut App, _submitted: bool) {
    let back_to_detail = app.form_from_detail;
    app.form = None;
    app.form_from_detail = false;
    app.screen = if back_to_detail {
        Screen::CardDetail
    } else {
        Screen::Board
    };
}
