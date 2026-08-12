use board_core::engine::{validate_column_delete, ValidationError};
use board_core::protocol::CardMoveParams;
use crossterm::event::{KeyCode, KeyEvent};

use crate::forms::Form;
use crate::view::LayoutMode;

use super::nav::nav_delta;
use super::{
    column_options, App, CardFilter, Confirm, ConfirmPurpose, Effect, MoveColumnState, Picker,
    PickerPurpose, ReorderCardState, Screen, SwitcherLevel, SwitcherState,
};

pub(super) fn board_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    if let Some(delta) = nav_delta(k.code) {
        app.move_card(delta);
        return vec![];
    }
    match k.code {
        KeyCode::Left | KeyCode::Char('h') => app.move_col(-1),
        KeyCode::Right | KeyCode::Char('l') => app.move_col(1),
        KeyCode::Char('b') => {
            if app.layout_mode() == LayoutMode::Compact {
                // `b` means "switch board": open the sheet straight at the
                // Boards level (board names only exist there), unlike the
                // header's center-button tap, which opens at Columns. The
                // board list loads via `Effect::LoadBoardsForSwitcher`,
                // which (synchronously) flips `level` to `Boards` once it
                // resolves; `entered_at_boards` records the entry point so
                // `Esc` from `Boards` closes the sheet instead of stepping
                // back to a Columns view the user never asked for.
                app.switcher = Some(SwitcherState {
                    level: SwitcherLevel::Columns,
                    sel: app.sel_col,
                    columns_sel: app.sel_col,
                    boards: Vec::new(),
                    entered_at_boards: true,
                    return_to: Screen::Board,
                });
                app.screen = Screen::Switcher;
                return vec![Effect::LoadBoardsForSwitcher];
            }
            return vec![Effect::LoadBoards];
        }
        KeyCode::Char('n') => {
            if let Some(col_id) = app.col_id_at(app.sel_col) {
                app.form = Some(Form::card_create_with_session(
                    col_id,
                    app.origin_context.session.as_deref(),
                ));
                app.screen = Screen::CardForm;
                return vec![Effect::LoadFormOptions];
            }
        }
        KeyCode::Char('N') => {
            app.form = Some(Form::column_create(&app.board.columns));
            app.screen = Screen::ColumnForm;
            return vec![Effect::LoadFormOptions];
        }
        KeyCode::Char('e') => {
            if let Some(card) = app.selected_card().cloned() {
                app.form = Some(Form::card_edit(&card));
                app.screen = Screen::CardForm;
                return vec![Effect::LoadFormOptions];
            }
        }
        KeyCode::Char('E') => {
            if let Some(col) = app.display_column(app.sel_col).cloned() {
                app.form = Some(Form::column_edit(&col, &app.board.columns));
                app.screen = Screen::ColumnForm;
                return vec![Effect::LoadFormOptions];
            }
        }
        KeyCode::Char('a') => return archive_selected_card(app),
        KeyCode::Char('C') => {
            if let Some(id) = app.selected_card_id() {
                return vec![Effect::CardDuplicate(id)];
            }
        }
        KeyCode::Char('v') => return set_card_filter(app, app.card_filter.next()),
        KeyCode::Char('d') => {
            if let Some(id) = app.selected_card_id() {
                app.confirm = Some(Confirm {
                    message: "Delete this card?".into(),
                    purpose: ConfirmPurpose::DeleteCard(id),
                    return_to: Screen::Board,
                });
                app.screen = Screen::Confirm;
            }
        }
        KeyCode::Char('D') => return delete_column(app),
        KeyCode::Char('m') => return open_move_picker(app),
        KeyCode::Char('M') => return open_move_column_mode(app),
        KeyCode::Char('O') => return open_reorder_card_mode(app),
        KeyCode::Char('H') => return shove_card(app, -1),
        KeyCode::Char('L') => return shove_card(app, 1),
        KeyCode::Enter => {
            if let Some(id) = app.selected_card_id() {
                return app.open_detail(id);
            }
        }
        KeyCode::Char('T') => return super::apply_template(app),
        KeyCode::Char('r') | KeyCode::Char('R') => {
            app.set_toast("refreshed", false);
            return vec![Effect::Refetch];
        }
        KeyCode::Char('q') | KeyCode::Esc => return vec![Effect::Quit],
        _ => {}
    }
    vec![]
}

/// Apply an explicit visibility filter through the same state/effect path as
/// the legacy `v` cycle binding.
pub(super) fn set_card_filter(app: &mut App, filter: CardFilter) -> Vec<Effect> {
    app.card_filter = filter;
    app.sel_card = 0;
    app.clamp_card();
    vec![Effect::SetPaneTitle(app.card_filter)]
}

fn archive_selected_card(app: &mut App) -> Vec<Effect> {
    let Some(result) = app.selected_card().map(super::archive_card) else {
        return vec![];
    };
    match result {
        Ok(effect) => vec![effect],
        Err(err) => {
            app.set_toast(err.to_string(), true);
            vec![]
        }
    }
}

fn delete_column(app: &mut App) -> Vec<Effect> {
    let Some(col_id) = app.col_id_at(app.sel_col) else {
        return vec![];
    };
    // Column deletion must account for cards hidden by the current archive
    // filter; the daemon still needs a destination for every persisted card.
    let has_cards = app.board.cards.iter().any(|card| card.column_id == col_id);
    // An open run in the column is refused outright — asking "move the cards
    // where?" first would only collect an answer the daemon then throws away.
    let has_active_card = app.board.cards.iter().any(|card| {
        card.column_id == col_id
            && app
                .board
                .active_runs
                .iter()
                .any(|run| run.card_id == card.id)
    });
    match validate_column_delete(has_cards, has_active_card, None) {
        Err(err @ ValidationError::ColumnHasActiveCard) => {
            app.set_toast(err.to_string(), true);
            return vec![];
        }
        // `ColumnHasCards` is the only other verdict this call can produce, and
        // it is not a refusal — it is the request for a destination that the
        // picker below collects.
        Err(_) => {}
        Ok(()) => {}
    }
    if has_cards {
        // Ask where to move them.
        let options = column_options(&app.board.columns, Some(col_id));
        if options.is_empty() {
            app.set_toast("no other column to move cards to", true);
            return vec![];
        }
        app.picker = Some(Picker {
            title: "Move cards to which column?".into(),
            options,
            sel: 0,
            purpose: PickerPurpose::DeleteColumnMoveTo { column_id: col_id },
            return_to: Screen::Board,
        });
        app.screen = Screen::Picker;
    } else {
        app.confirm = Some(Confirm {
            message: "Delete this column?".into(),
            purpose: ConfirmPurpose::DeleteColumn {
                id: col_id,
                move_cards_to: None,
            },
            return_to: Screen::Board,
        });
        app.screen = Screen::Confirm;
    }
    vec![]
}

fn open_move_picker(app: &mut App) -> Vec<Effect> {
    if app.reject_archived_move() {
        return vec![];
    }
    let Some(card) = app.selected_card() else {
        return vec![];
    };
    // Fast path: open the active board's column picker directly (one step for
    // the common same-board move). Press `b` inside it to switch to the
    // destination-board picker for a cross-board move.
    let board_id = app.board.board.id;
    let cur = card.column_id;
    let options = column_options(&app.board.columns, Some(cur));
    if options.is_empty() {
        app.set_toast("no other column to move cards to", true);
        return vec![];
    }
    app.picker = Some(Picker {
        title: format!(
            "Move card to which column? ({})  · b = other board",
            app.board.board.name
        ),
        options,
        sel: 0,
        purpose: PickerPurpose::MoveCardPickColumn {
            card_id: card.id,
            board_id,
        },
        return_to: Screen::Board,
    });
    app.screen = Screen::Picker;
    vec![]
}

fn open_move_column_mode(app: &mut App) -> Vec<Effect> {
    let Some(col_id) = app.col_id_at(app.sel_col) else {
        return vec![];
    };
    app.move_column = Some(MoveColumnState {
        column_id: col_id,
        original_index: app.sel_col,
        staged_index: app.sel_col,
    });
    app.screen = Screen::MoveColumn;
    vec![]
}

fn open_reorder_card_mode(app: &mut App) -> Vec<Effect> {
    if app.reject_archived_move() {
        return vec![];
    }
    let Some(card) = app.selected_card() else {
        return vec![];
    };
    app.reorder_card = Some(ReorderCardState {
        card_id: card.id,
        column_id: card.column_id,
        original_index: app.sel_card,
        staged_index: app.sel_card,
    });
    app.screen = Screen::ReorderCard;
    vec![]
}

fn shove_card(app: &mut App, delta: isize) -> Vec<Effect> {
    if app.reject_archived_move() {
        return vec![];
    }
    let Some(card_id) = app.selected_card_id() else {
        return vec![];
    };
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
        board_id: None,
        position: None,
    })]
}
