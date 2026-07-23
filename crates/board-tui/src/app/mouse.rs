use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::{App, DetailScrollTarget, Effect, Screen};

pub(super) fn on_mouse(app: &mut App, m: MouseEvent) -> Vec<Effect> {
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
                        app.detail_fullscreen = false;
                        app.detail_scroll_target = DetailScrollTarget::Comments;
                        app.detail_comments_scroll = 0;
                        app.detail_runs_scroll = 0;
                        app.screen = Screen::CardDetail;
                        return vec![Effect::LoadDetail(id)];
                    }
                }
                if let Some(card) = app.selected_card() {
                    if card.archived_at.is_some() {
                        app.set_toast("restore archived card before moving", true);
                    } else {
                        app.begin_card_drag(card.id, col_idx);
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
        MouseEventKind::ScrollDown => app.move_card(1),
        MouseEventKind::ScrollUp => app.move_card(-1),
        _ => {}
    }
    vec![]
}
