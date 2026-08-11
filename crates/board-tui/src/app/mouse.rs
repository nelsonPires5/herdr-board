use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::widgets::{UiAction, Zone};

use super::{App, DetailScrollTarget, Effect, Screen, SwitcherLevel};

pub(super) fn on_mouse(app: &mut App, m: MouseEvent) -> Vec<Effect> {
    // New Compact-mode widgets (header buttons, switcher rows, button bars,
    // sheet close) are checked first, on every screen, via the HitMap the
    // last `view()` call registered. Existing board/detail hit-testing below
    // is untouched.
    if m.kind == MouseEventKind::Down(MouseButton::Left) {
        let hit = app.hit_map.borrow().hit(m.column, m.row);
        if let Some(zone) = hit {
            if let Some(effects) = handle_zone(app, zone) {
                return effects;
            }
        }
    }

    if matches!(app.screen, Screen::CardForm | Screen::ColumnForm) {
        return match m.kind {
            MouseEventKind::ScrollDown => super::on_key(app, key(KeyCode::Tab)),
            MouseEventKind::ScrollUp => super::on_key(app, key(KeyCode::BackTab)),
            _ => vec![],
        };
    }

    if matches!(app.screen, Screen::Help | Screen::CommentHistory) {
        return match m.kind {
            MouseEventKind::ScrollDown => super::on_key(app, key(KeyCode::Down)),
            MouseEventKind::ScrollUp => super::on_key(app, key(KeyCode::Up)),
            _ => vec![],
        };
    }

    if app.screen == Screen::CardDetail {
        let detail_layout = crate::view::detail_layout(app, app.last_area);
        let in_rect = |rect: Rect| {
            m.column >= rect.x
                && m.column < rect.x.saturating_add(rect.width)
                && m.row >= rect.y
                && m.row < rect.y.saturating_add(rect.height)
        };
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if in_rect(crate::view::detail_toggle_rect(app, app.last_area)) {
                    app.toggle_detail_fullscreen();
                } else if in_rect(detail_layout.comments) {
                    app.detail_scroll_target = DetailScrollTarget::Comments;
                } else if in_rect(detail_layout.runs) {
                    app.detail_scroll_target = DetailScrollTarget::Runs;
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                if in_rect(detail_layout.comments) {
                    app.detail_scroll_target = DetailScrollTarget::Comments;
                } else if in_rect(detail_layout.runs) {
                    app.detail_scroll_target = DetailScrollTarget::Runs;
                } else {
                    return vec![];
                }
                app.scroll_detail(if matches!(m.kind, MouseEventKind::ScrollDown) {
                    1
                } else {
                    -1
                });
                // A raw offset move can leave the focused/selected row off
                // screen; carry the cursor along so the `▸` marker (and what
                // `o`/`e`/`d`/`h` act on) stays inside the rendered window.
                app.follow_detail_scroll();
            }
            _ => {}
        }
        return vec![];
    }
    if app.screen != Screen::Board {
        return vec![];
    }
    let layout = crate::view::board_layout(app, app.last_area);
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some((col_idx, card_idx)) = layout.hit_card(m.column, m.row) {
                app.sel_col = col_idx;
                app.sel_card = card_idx;
                // double-click → open detail
                let dbl = app
                    .last_click
                    .map(|(x, y, t)| {
                        x == m.column && y == m.row && app.now_ms.saturating_sub(t) < 400
                    })
                    .unwrap_or(false);
                app.last_click = Some((m.column, m.row, app.now_ms));
                if dbl {
                    if let Some(id) = app.selected_card_id() {
                        return app.open_detail(id);
                    }
                }
                if !app.reject_archived_move() {
                    if let Some(id) = app.selected_card_id() {
                        app.begin_card_drag(id, col_idx);
                    }
                }
            } else if let Some(col_idx) = layout.hit_header(m.column, m.row) {
                app.sel_col = col_idx;
                app.clamp_card();
                if let Some(id) = app.col_id_at(col_idx) {
                    app.begin_column_drag(id, col_idx);
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(col_idx) = layout.hit_any_column(m.column) {
                app.drag_hover(col_idx);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(col_idx) = layout.hit_any_column(m.column) {
                app.drag_hover(col_idx);
            }
            return app.finish_drag();
        }
        // Wheel scrolls the column's card list (per-column offset); card
        // reordering by mouse wheel is gone — use keyboard `H`/`L` instead.
        MouseEventKind::ScrollDown => scroll_hovered_column(app, &layout, m, 1),
        MouseEventKind::ScrollUp => scroll_hovered_column(app, &layout, m, -1),
        _ => {}
    }
    vec![]
}

/// Move the hovered column's scroll offset, then — if the wheel landed on
/// the currently *selected* column — move the selection along with the
/// viewport instead of leaving it to `col_layout_with_header`'s
/// selection-follow clamp, which otherwise fires every frame and silently
/// snaps the offset straight back to wherever the (unmoved) selection was,
/// making the wheel look like a no-op on the focused column (the common
/// case). Keyboard-driven selection changes are unaffected: they still pull
/// the viewport to the selection via that same clamp.
///
/// `layout.cols[..].scroll.{total,visible}` are pure functions of the
/// column's rect/card-height/card-count — independent of both the current
/// scroll offset and the current selection — so they're safe to reuse here
/// to compute the new window without re-deriving that geometry.
fn scroll_hovered_column(
    app: &mut App,
    layout: &crate::view::BoardLayout,
    m: MouseEvent,
    delta: isize,
) {
    let Some(col_idx) = layout.hit_any_column(m.column) else {
        return;
    };
    let Some(col_id) = app.col_id_at(col_idx) else {
        return;
    };
    let Some(col) = layout.cols.iter().find(|c| c.idx == col_idx) else {
        return;
    };
    let total = col.scroll.total;
    let visible = col.scroll.visible;
    let max_offset = total.saturating_sub(visible);

    let entry = app.col_scroll.entry(col_id).or_insert(0);
    let new_offset = (*entry as isize + delta).clamp(0, max_offset as isize) as usize;
    *entry = new_offset;

    if visible > 0 && col_idx == app.sel_col {
        if app.sel_card < new_offset {
            app.sel_card = new_offset;
        } else if app.sel_card >= new_offset + visible {
            app.sel_card = new_offset + visible - 1;
        }
    }
}

/// Handle a click on one of the new HitMap zones. Returns `Some(effects)` when
/// the zone consumed the click (short-circuiting the existing board/detail
/// hit-testing below), `None` to fall through unhandled.
fn handle_zone(app: &mut App, zone: Zone) -> Option<Vec<Effect>> {
    match zone {
        Zone::Filter(filter) if app.screen == Screen::Board => {
            Some(super::board::set_card_filter(app, filter))
        }
        // Board card Edit/Delete zones were removed. Keep the legacy variant
        // as an explicit no-op so stale callers fail closed instead of
        // falling through to card-body drag/focus handling.
        Zone::CardAction { .. } if app.screen == Screen::Board => Some(vec![]),
        Zone::Action(action) => {
            // Card-level Edit must not inherit Comments focus, where the `e`
            // reducer intentionally edits the selected comment instead. Route
            // through that same key branch without changing the focus users
            // return to after Save/Cancel.
            let restore_focus = (app.screen == Screen::CardDetail && action == UiAction::EditCard)
                .then_some(app.detail_scroll_target);
            if restore_focus.is_some() {
                app.detail_scroll_target = DetailScrollTarget::Runs;
            }
            let effects = action_event(app.screen, action).map(|event| super::on_key(app, event));
            if let Some(focus) = restore_focus {
                app.detail_scroll_target = focus;
            }
            effects
        }
        Zone::HeaderPrev if app.screen == Screen::Board => {
            app.move_col(-1);
            Some(vec![])
        }
        Zone::HeaderNext if app.screen == Screen::Board => {
            app.move_col(1);
            Some(vec![])
        }
        Zone::HeaderSwitch if app.screen == Screen::Board => {
            // Tapping the header's center button opens at Columns (unlike
            // `b`, which opens directly at Boards); `entered_at_boards:
            // false` makes `Esc` from a Boards level reached by drilling
            // down step back to Columns instead of closing outright.
            app.switcher = Some(super::SwitcherState {
                level: SwitcherLevel::Columns,
                sel: app.sel_col,
                columns_sel: app.sel_col,
                boards: Vec::new(),
                entered_at_boards: false,
                return_to: Screen::Board,
            });
            app.screen = Screen::Switcher;
            Some(vec![])
        }
        Zone::SwitcherRow(idx) if app.screen == Screen::Switcher => {
            if let Some(state) = app.switcher.as_mut() {
                state.sel = idx;
            }
            Some(super::on_key(app, key(KeyCode::Enter)))
        }
        Zone::SwitcherSwitchBoard if app.screen == Screen::Switcher => {
            if let Some(state) = app.switcher.as_mut() {
                let trailing = app.board.columns.len();
                state.sel = trailing;
            }
            Some(super::on_key(app, key(KeyCode::Enter)))
        }
        Zone::SwitcherApplyTemplate if app.screen == Screen::Switcher => {
            if let Some(state) = app.switcher.as_mut() {
                let trailing = app.board.columns.len() + 1;
                state.sel = trailing;
            }
            Some(super::on_key(app, key(KeyCode::Enter)))
        }
        Zone::FormField(idx) if matches!(app.screen, Screen::CardForm | Screen::ColumnForm) => {
            let valid = app
                .form
                .as_ref()
                .is_some_and(|form| idx < form.fields.len() && form.field_visible(idx));
            if valid {
                app.form.as_mut().expect("validated form").focus = idx;
            }
            Some(vec![])
        }
        Zone::FormChoicePrev(idx) | Zone::FormChoiceNext(idx)
            if matches!(app.screen, Screen::CardForm | Screen::ColumnForm) =>
        {
            let valid = app
                .form
                .as_ref()
                .is_some_and(|form| idx < form.fields.len() && form.field_visible(idx));
            if !valid {
                return Some(vec![]);
            }
            app.form.as_mut().expect("validated form").focus = idx;
            let code = if matches!(zone, Zone::FormChoicePrev(_)) {
                KeyCode::Left
            } else {
                KeyCode::Right
            };
            Some(super::on_key(app, key(code)))
        }
        Zone::FormEditor(idx) if matches!(app.screen, Screen::CardForm | Screen::ColumnForm) => {
            let valid = app
                .form
                .as_ref()
                .is_some_and(|form| idx < form.fields.len() && form.field_visible(idx));
            if !valid {
                return Some(vec![]);
            }
            app.form.as_mut().expect("validated form").focus = idx;
            if app
                .form
                .as_ref()
                .is_some_and(|form| form.focused_is_multiline())
            {
                Some(super::on_key(
                    app,
                    KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL),
                ))
            } else {
                Some(vec![])
            }
        }
        Zone::PickerRow(idx) if app.screen == Screen::Picker => {
            let valid = app
                .picker
                .as_ref()
                .is_some_and(|picker| idx < picker.options.len());
            if valid {
                app.picker.as_mut().expect("validated picker").sel = idx;
                Some(super::on_key(app, key(KeyCode::Enter)))
            } else {
                Some(vec![])
            }
        }
        Zone::HelpScrollUp if app.screen == Screen::Help => {
            Some(super::on_key(app, key(KeyCode::Up)))
        }
        Zone::HelpScrollDown if app.screen == Screen::Help => {
            Some(super::on_key(app, key(KeyCode::Down)))
        }
        Zone::HistoryScrollUp if app.screen == Screen::CommentHistory => {
            Some(super::on_key(app, key(KeyCode::Up)))
        }
        Zone::HistoryScrollDown if app.screen == Screen::CommentHistory => {
            Some(super::on_key(app, key(KeyCode::Down)))
        }
        Zone::Shield => Some(vec![]),
        Zone::BarSave if matches!(app.screen, Screen::CardForm | Screen::ColumnForm) => {
            Some(super::on_key(app, key(KeyCode::Enter)))
        }
        Zone::BarCancel if matches!(app.screen, Screen::CardForm | Screen::ColumnForm) => {
            Some(super::on_key(app, key(KeyCode::Esc)))
        }
        Zone::SheetClose => Some(super::on_key(app, key(KeyCode::Esc))),
        Zone::CommentRow(idx) if app.screen == Screen::CardDetail => {
            app.detail_scroll_target = DetailScrollTarget::Comments;
            app.detail_comment_sel = idx;
            app.follow_comment_focus();
            Some(vec![])
        }
        Zone::RunRow(idx) if app.screen == Screen::CardDetail => {
            app.detail_scroll_target = DetailScrollTarget::Runs;
            app.detail_run_sel = idx;
            app.follow_run_focus();
            Some(vec![])
        }
        Zone::CommentEdit if app.screen == Screen::CardDetail => {
            Some(super::on_key(app, key(KeyCode::Char('e'))))
        }
        Zone::CommentDelete if app.screen == Screen::CardDetail => {
            Some(super::on_key(app, key(KeyCode::Char('d'))))
        }
        Zone::CommentHistory if app.screen == Screen::CardDetail => {
            Some(super::on_key(app, key(KeyCode::Char('h'))))
        }
        _ => None,
    }
}

fn action_event(screen: Screen, action: UiAction) -> Option<KeyEvent> {
    use UiAction as A;
    let (code, modifiers) = match (screen, action) {
        (Screen::Board, A::Help) => (KeyCode::Char('?'), KeyModifiers::NONE),
        (Screen::Board, A::Quit) => (KeyCode::Char('q'), KeyModifiers::NONE),
        (Screen::Board, A::SwitchBoard) => (KeyCode::Char('b'), KeyModifiers::NONE),
        (Screen::Board, A::NewCard) => (KeyCode::Char('n'), KeyModifiers::NONE),
        (Screen::Board, A::NewColumn) => (KeyCode::Char('N'), KeyModifiers::SHIFT),
        (Screen::Board, A::EditCard) => (KeyCode::Char('e'), KeyModifiers::NONE),
        (Screen::Board, A::EditColumn) => (KeyCode::Char('E'), KeyModifiers::SHIFT),
        (Screen::Board, A::ArchiveCard) => (KeyCode::Char('a'), KeyModifiers::NONE),
        (Screen::Board, A::DuplicateCard) => (KeyCode::Char('C'), KeyModifiers::SHIFT),
        (Screen::Board, A::CycleFilter) => (KeyCode::Char('v'), KeyModifiers::NONE),
        (Screen::Board, A::DeleteCard) => (KeyCode::Char('d'), KeyModifiers::NONE),
        (Screen::Board, A::DeleteColumn) => (KeyCode::Char('D'), KeyModifiers::SHIFT),
        (Screen::Board, A::MoveCard) => (KeyCode::Char('m'), KeyModifiers::NONE),
        (Screen::Board, A::MoveColumn) => (KeyCode::Char('M'), KeyModifiers::SHIFT),
        (Screen::Board, A::ShoveCardLeft) => (KeyCode::Char('H'), KeyModifiers::SHIFT),
        (Screen::Board, A::ShoveCardRight) => (KeyCode::Char('L'), KeyModifiers::SHIFT),
        (Screen::Board, A::OpenCard) => (KeyCode::Enter, KeyModifiers::NONE),
        (Screen::Board, A::ApplyTemplate) => (KeyCode::Char('T'), KeyModifiers::SHIFT),
        (Screen::Board, A::Refresh) => (KeyCode::Char('r'), KeyModifiers::NONE),

        (Screen::CardDetail, A::Help) => (KeyCode::Char('?'), KeyModifiers::NONE),
        (
            Screen::Picker
            | Screen::MoveColumn
            | Screen::Confirm
            | Screen::Switcher
            | Screen::CommentHistory,
            A::Help,
        ) => (KeyCode::Char('?'), KeyModifiers::NONE),
        (Screen::CardDetail, A::ConfirmAwaiting) => (KeyCode::Enter, KeyModifiers::NONE),
        (Screen::CardDetail, A::EditCard) => (KeyCode::Char('e'), KeyModifiers::NONE),
        (Screen::CardDetail, A::ArchiveCard) => (KeyCode::Char('a'), KeyModifiers::NONE),
        (Screen::CardDetail, A::DuplicateCard) => (KeyCode::Char('C'), KeyModifiers::SHIFT),
        (Screen::CardDetail, A::AddComment) => (KeyCode::Char('c'), KeyModifiers::NONE),
        (Screen::CardDetail, A::DeleteComment) => (KeyCode::Char('d'), KeyModifiers::NONE),
        (Screen::CardDetail, A::CommentHistory) => (KeyCode::Char('h'), KeyModifiers::NONE),
        (Screen::CardDetail, A::ToggleDetail) => (KeyCode::Char('f'), KeyModifiers::NONE),
        (Screen::CardDetail, A::FocusRunPane) => (KeyCode::Char('o'), KeyModifiers::NONE),
        (Screen::CardDetail, A::CancelRun) => (KeyCode::Char('x'), KeyModifiers::NONE),
        (Screen::CardDetail, A::RetryRun) => (KeyCode::Char('r'), KeyModifiers::NONE),
        (Screen::CardDetail, A::CloseDetail) => (KeyCode::Esc, KeyModifiers::NONE),

        (Screen::CardForm | Screen::ColumnForm, A::SubmitForm) => {
            (KeyCode::Enter, KeyModifiers::NONE)
        }
        (Screen::CardForm | Screen::ColumnForm, A::CancelForm) => {
            (KeyCode::Esc, KeyModifiers::NONE)
        }
        (Screen::CardForm | Screen::ColumnForm, A::EditInExternalEditor) => {
            (KeyCode::Char('e'), KeyModifiers::CONTROL)
        }

        (Screen::Picker, A::ChoosePickerRow) => (KeyCode::Enter, KeyModifiers::NONE),
        (Screen::Picker, A::PickerOtherBoard) => (KeyCode::Char('b'), KeyModifiers::NONE),
        (Screen::Picker, A::CancelPicker) => (KeyCode::Esc, KeyModifiers::NONE),
        (Screen::Confirm, A::ConfirmYes) => (KeyCode::Char('y'), KeyModifiers::NONE),
        (Screen::Confirm, A::ConfirmNo) => (KeyCode::Char('n'), KeyModifiers::NONE),
        (Screen::MoveColumn, A::StageColumnLeft) => (KeyCode::Left, KeyModifiers::NONE),
        (Screen::MoveColumn, A::StageColumnRight) => (KeyCode::Right, KeyModifiers::NONE),
        (Screen::MoveColumn, A::CommitColumnMove) => (KeyCode::Enter, KeyModifiers::NONE),
        (Screen::MoveColumn, A::CancelColumnMove) => (KeyCode::Esc, KeyModifiers::NONE),
        (Screen::Switcher, A::ChooseSwitcherRow) => (KeyCode::Enter, KeyModifiers::NONE),
        (Screen::Switcher, A::CloseSwitcher) => (KeyCode::Esc, KeyModifiers::NONE),
        (Screen::CommentHistory, A::CloseCommentHistory) => (KeyCode::Esc, KeyModifiers::NONE),
        (Screen::Help, A::CloseHelp) => (KeyCode::Esc, KeyModifiers::NONE),
        _ => return None,
    };
    Some(KeyEvent::new(code, modifiers))
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
