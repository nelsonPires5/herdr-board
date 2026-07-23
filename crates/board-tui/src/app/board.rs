use board_core::protocol::{CardMoveParams, CardStatus};
use crossterm::event::{KeyCode, KeyEvent};

use crate::forms::Form;

use super::{
    App, Confirm, ConfirmPurpose, DetailScrollTarget, Effect, Picker, PickerPurpose, Screen,
};

pub(super) fn board_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    match k.code {
        KeyCode::Left | KeyCode::Char('h') => app.move_col(-1),
        KeyCode::Right | KeyCode::Char('l') => app.move_col(1),
        KeyCode::Up | KeyCode::Char('k') => app.move_card(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_card(1),
        KeyCode::Char('b') => return vec![Effect::LoadBoards],
        KeyCode::Char('n') => {
            if let Some(col_id) = app.col_id_at(app.sel_col) {
                app.form_from_detail = false;
                app.form = Some(Form::card_create_with_session(
                    col_id,
                    app.origin_context.session.as_deref(),
                ));
                app.screen = Screen::CardForm;
                return vec![Effect::LoadFormOptions];
            }
        }
        KeyCode::Char('N') => {
            app.form_from_detail = false;
            app.form = Some(Form::column_create(&app.board.columns));
            app.screen = Screen::ColumnForm;
            return vec![Effect::LoadFormOptions];
        }
        KeyCode::Char('e') => {
            if let Some(card) = app.selected_card().cloned() {
                app.form_from_detail = false;
                app.form = Some(Form::card_edit(&card));
                app.screen = Screen::CardForm;
                return vec![Effect::LoadFormOptions];
            }
        }
        KeyCode::Char('E') => {
            if let Some(col) = app.board.columns.get(app.sel_col).cloned() {
                app.form_from_detail = false;
                app.form = Some(Form::column_edit(&col, &app.board.columns));
                app.screen = Screen::ColumnForm;
                return vec![Effect::LoadFormOptions];
            }
        }
        KeyCode::Char('a') => return archive_selected_card(app),
        KeyCode::Char('v') => {
            app.card_filter = app.card_filter.next();
            app.sel_card = 0;
            app.clamp_card();
            return vec![Effect::SetPaneTitle(app.card_filter)];
        }
        KeyCode::Char('d') => {
            if let Some(id) = app.selected_card_id() {
                app.confirm = Some(Confirm {
                    message: "Delete this card?".into(),
                    purpose: ConfirmPurpose::DeleteCard(id),
                });
                app.screen = Screen::Confirm;
            }
        }
        KeyCode::Char('D') => return delete_column(app),
        KeyCode::Char('m') => return open_move_picker(app),
        KeyCode::Char('H') => return shove_card(app, -1),
        KeyCode::Char('L') => return shove_card(app, 1),
        KeyCode::Enter => {
            if let Some(id) = app.selected_card_id() {
                app.detail_fullscreen = false;
                app.detail_scroll_target = DetailScrollTarget::Comments;
                app.detail_comments_scroll = 0;
                app.detail_runs_scroll = 0;
                app.screen = Screen::CardDetail;
                return vec![Effect::LoadDetail(id)];
            }
        }
        KeyCode::Char('T') => {
            if app.is_empty_board() {
                return vec![Effect::TemplateApply("pipeline".into())];
            }
            app.set_toast("template only applies to an empty board", true);
        }
        KeyCode::Char('r') | KeyCode::Char('R') => {
            app.set_toast("refreshed", false);
            return vec![Effect::Refetch];
        }
        KeyCode::Char('?') => app.screen = Screen::Help,
        KeyCode::Char('q') | KeyCode::Esc => return vec![Effect::Quit],
        _ => {}
    }
    vec![]
}

fn archive_selected_card(app: &mut App) -> Vec<Effect> {
    let Some(card) = app.selected_card() else {
        return vec![];
    };
    if card.archived_at.is_none()
        && matches!(
            card.status,
            CardStatus::Queued | CardStatus::Running | CardStatus::Blocked | CardStatus::Awaiting
        )
    {
        app.set_toast("card has an active run; cancel it before archiving", true);
        return vec![];
    }
    vec![Effect::CardArchive {
        id: card.id,
        archived: card.archived_at.is_none(),
    }]
}

fn delete_column(app: &mut App) -> Vec<Effect> {
    let Some(col_id) = app.col_id_at(app.sel_col) else {
        return vec![];
    };
    // Column deletion must account for cards hidden by the current archive
    // filter; the daemon still needs a destination for every persisted card.
    let has_cards = app.board.cards.iter().any(|card| card.column_id == col_id);
    if has_cards {
        // Ask where to move them (daemon still refuses if a card is running).
        let options: Vec<(String, i64)> = app
            .board
            .columns
            .iter()
            .filter(|c| c.id != col_id)
            .map(|c| (c.name.clone(), c.id))
            .collect();
        if options.is_empty() {
            app.set_toast("no other column to move cards to", true);
            return vec![];
        }
        app.picker = Some(Picker {
            title: "Move cards to which column?".into(),
            options,
            sel: 0,
            purpose: PickerPurpose::DeleteColumnMoveTo { column_id: col_id },
        });
        app.screen = Screen::Picker;
    } else {
        app.confirm = Some(Confirm {
            message: "Delete this column?".into(),
            purpose: ConfirmPurpose::DeleteColumn(col_id),
        });
        app.screen = Screen::Confirm;
    }
    vec![]
}

fn open_move_picker(app: &mut App) -> Vec<Effect> {
    let Some(card) = app.selected_card() else {
        return vec![];
    };
    if card.archived_at.is_some() {
        app.set_toast("restore archived card before moving", true);
        return vec![];
    }
    let card_id = card.id;
    let cur = app.col_id_at(app.sel_col);
    let options: Vec<(String, i64)> = app
        .board
        .columns
        .iter()
        .filter(|c| Some(c.id) != cur)
        .map(|c| (c.name.clone(), c.id))
        .collect();
    if options.is_empty() {
        return vec![];
    }
    app.picker = Some(Picker {
        title: "Move card to which column?".into(),
        options,
        sel: 0,
        purpose: PickerPurpose::MoveCard { card_id },
    });
    app.screen = Screen::Picker;
    vec![]
}

fn shove_card(app: &mut App, delta: isize) -> Vec<Effect> {
    let Some(card) = app.selected_card() else {
        return vec![];
    };
    if card.archived_at.is_some() {
        app.set_toast("restore archived card before moving", true);
        return vec![];
    }
    let card_id = card.id;
    let n = app.board.columns.len() as isize;
    if n == 0 {
        return vec![];
    }
    let target = (app.sel_col as isize + delta).rem_euclid(n) as usize;
    if target == app.sel_col {
        return vec![];
    }
    let Some(column_id) = app.col_id_at(target) else {
        return vec![];
    };
    app.sel_col = target;
    app.sel_card = 0;
    vec![Effect::CardMove(CardMoveParams {
        id: card_id,
        column_id,
        position: None,
    })]
}
