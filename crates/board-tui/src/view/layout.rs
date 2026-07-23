use ratatui::layout::Rect;

use crate::app::App;

use super::{main_area, CARD_H, MIN_COL_W};

// -- layout / hit-testing ----------------------------------------------------

pub struct ColLayout {
    pub idx: usize,
    pub rect: Rect,
    pub cards: Vec<(usize, Rect)>,
}

pub struct BoardLayout {
    pub cols: Vec<ColLayout>,
}

impl BoardLayout {
    pub fn hit_card(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        for c in &self.cols {
            for (ci, r) in &c.cards {
                if x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height {
                    return Some((c.idx, *ci));
                }
            }
        }
        None
    }
    pub fn hit_header(&self, x: u16, y: u16) -> Option<usize> {
        for c in &self.cols {
            if y == c.rect.y && x >= c.rect.x && x < c.rect.x + c.rect.width {
                return Some(c.idx);
            }
        }
        None
    }
    pub fn hit_any_column(&self, x: u16) -> Option<usize> {
        for c in &self.cols {
            if x >= c.rect.x && x < c.rect.x + c.rect.width {
                return Some(c.idx);
            }
        }
        None
    }
}

/// Compute visible-column and card geometry. Pure function of `app` + `area`,
/// so mouse handling can recompute the exact rects the last frame used.
pub fn board_layout(app: &App, area: Rect) -> BoardLayout {
    let main = main_area(area);
    let n = app.board.columns.len();
    let mut cols = Vec::new();
    if n == 0 || main.width == 0 {
        return BoardLayout { cols };
    }
    // Fill the entire viewport. Keep columns readable via a minimum width;
    // when every column fits, distribute all remaining cells across them.
    // When they do not all fit, the selected column drives a full-width window.
    let capacity = (main.width / MIN_COL_W).max(1) as usize;
    let visible = capacity.min(n);
    let start = app
        .sel_col
        .saturating_add(1)
        .saturating_sub(visible)
        .min(n.saturating_sub(visible));
    let base_w = main.width / visible as u16;
    let remainder = main.width % visible as u16;
    let mut x = main.x;
    for i in 0..visible {
        let idx = start + i;
        let w = base_w + u16::from((i as u16) < remainder);
        let rect = Rect::new(x, main.y, w, main.height);
        x = x.saturating_add(w);
        let col = &app.board.columns[idx];
        let cards = app.cards_of(col.id);
        let mut card_rects = Vec::new();
        let inner_y = rect.y + 1;
        let inner_h = rect.height.saturating_sub(2);
        for (ci, _) in cards.iter().enumerate() {
            let cy = inner_y + (ci as u16) * CARD_H;
            if cy >= inner_y + inner_h {
                break;
            }
            let h = CARD_H.min(inner_y + inner_h - cy);
            card_rects.push((
                ci,
                Rect::new(rect.x + 1, cy, rect.width.saturating_sub(2), h),
            ));
        }
        cols.push(ColLayout {
            idx,
            rect,
            cards: card_rects,
        });
    }
    BoardLayout { cols }
}
