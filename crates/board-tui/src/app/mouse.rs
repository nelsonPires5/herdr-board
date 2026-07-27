use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::widgets::Zone;

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

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
