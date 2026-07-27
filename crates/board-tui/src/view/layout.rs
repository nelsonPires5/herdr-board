use ratatui::layout::Rect;

use crate::app::App;

use super::{main_area, LayoutMode, CARD_H, COMPACT_CARD_H, MIN_COL_W};

// -- layout / hit-testing ----------------------------------------------------

/// Vertical card-scroll bookkeeping for a single column, computed alongside
/// its card rects so drawing and mouse handling agree on what's visible.
#[derive(Clone, Copy, Default)]
pub struct ScrollInfo {
    pub offset: usize,
    pub total: usize,
    pub visible: usize,
}

impl ScrollInfo {
    pub fn overflowing(&self) -> bool {
        self.total > self.visible
    }
}

pub struct ColLayout {
    pub idx: usize,
    pub rect: Rect,
    pub cards: Vec<(usize, Rect)>,
    pub scroll: ScrollInfo,
    /// 1-cell scrollbar track on the column's right edge, when overflowing.
    pub scrollbar_rect: Option<Rect>,
}

pub struct BoardLayout {
    pub cols: Vec<ColLayout>,
    /// Compact-mode header hit zones (prev / switch-button / next), `None` in
    /// Regular/Wide.
    pub compact_header: Option<CompactHeader>,
}

/// Rects for the three clickable zones of the Compact single-column header.
pub struct CompactHeader {
    pub prev: Rect,
    pub switch: Rect,
    pub next: Rect,
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
    let mode = LayoutMode::from_width(area.width);
    if mode == LayoutMode::Compact {
        return board_layout_compact(app, area);
    }
    let main = main_area(area);
    let n = app.board.columns.len();
    let mut cols = Vec::new();
    if n == 0 || main.width == 0 {
        return BoardLayout {
            cols,
            compact_header: None,
        };
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
        cols.push(col_layout(app, idx, rect, CARD_H));
    }
    BoardLayout {
        cols,
        compact_header: None,
    }
}

/// Compact: exactly one column at full main width, with a 2-row header that
/// carries prev/switch/next hit zones instead of the plain bordered title.
fn board_layout_compact(app: &App, area: Rect) -> BoardLayout {
    let main = main_area(area);
    let n = app.board.columns.len();
    if n == 0 || main.width == 0 {
        return BoardLayout {
            cols: Vec::new(),
            compact_header: None,
        };
    }
    let idx = app.sel_col.min(n - 1);
    let header_h = 2u16.min(main.height);
    let rect = Rect::new(main.x, main.y, main.width, main.height);
    let header = CompactHeader {
        prev: Rect::new(main.x, main.y, 3.min(main.width), 1),
        switch: Rect::new(
            main.x + 3.min(main.width),
            main.y,
            main.width.saturating_sub(6),
            1,
        ),
        next: Rect::new(
            main.x + main.width.saturating_sub(3),
            main.y,
            3.min(main.width),
            1,
        ),
    };
    let col = col_layout_with_header(app, idx, rect, COMPACT_CARD_H, header_h, 0, true);
    BoardLayout {
        cols: vec![col],
        compact_header: Some(header),
    }
}

fn col_layout(app: &App, idx: usize, rect: Rect, card_h: u16) -> ColLayout {
    // Bordered box: 1 header row (border+title) + 1 bottom border row.
    col_layout_with_header(app, idx, rect, card_h, 1, 1, false)
}

/// Rows a Compact card needs: 3 when its title fits on one line, 4 when the
/// title wraps to two (1/2 title rows + 1 status row + 1 spare row, matching
/// how `board::draw_card` splits the rect). `content_w` is the card's content
/// width (after the "▌ " glyph prefix it's actually rendered at).
fn compact_card_height(title: &str, content_w: u16) -> u16 {
    let usable = content_w.saturating_sub(2).max(1);
    let lines = super::detail::wrapped_row_count(title, usable).clamp(1, 2) as u16;
    lines + 2
}

/// Shared card-rect + scroll computation. `header_h` is how many rows at the
/// top of `rect` are consumed by a title/header (1 for the bordered box, 2 for
/// the Compact header); `reserve_bottom` is extra rows reserved below the card
/// list (1 for the bordered box's bottom border, 0 for the borderless Compact
/// column which already sits inside `main_area`). `compact` selects the
/// variable 3/4-row Compact card sizing over the fixed `card_h`.
fn col_layout_with_header(
    app: &App,
    idx: usize,
    rect: Rect,
    card_h: u16,
    header_h: u16,
    reserve_bottom: u16,
    compact: bool,
) -> ColLayout {
    // Display order, not snapshot order: a staged `M` reorder is a permutation
    // applied at read time (see `App::display_column`).
    let Some(column) = app.display_column(idx) else {
        return ColLayout {
            idx,
            rect,
            cards: Vec::new(),
            scroll: ScrollInfo {
                offset: 0,
                total: 0,
                visible: 0,
            },
            scrollbar_rect: None,
        };
    };
    let cards = app.cards_of(column.id);
    let total = cards.len();
    let inner_y = rect.y + header_h;
    let inner_h = rect.height.saturating_sub(header_h + reserve_bottom);
    // When no card can fit at all (`inner_h < card_h`), `visible_count` must
    // be 0 — not `.max(1)` — so the scroll math never claims a card is
    // visible when the render loop below draws none (the "selected card
    // always has a rect" invariant only holds while `visible_count > 0`;
    // `tests/layout.rs` asserts the degenerate case separately).
    let visible_count = inner_h.checked_div(card_h).unwrap_or(0) as usize;

    let sel = if idx == app.sel_col {
        app.sel_card.min(total.saturating_sub(1))
    } else {
        0
    };
    let mut offset = app.col_scroll.get(&column.id).copied().unwrap_or(0);
    if visible_count == 0 {
        // Nothing fits regardless of offset; report no scroll rather than an
        // arbitrary clamped value.
        offset = 0;
    } else {
        let max_offset = total.saturating_sub(visible_count);
        offset = offset.min(max_offset);
        if idx == app.sel_col && total > 0 {
            if sel < offset {
                offset = sel;
            } else if sel >= offset + visible_count {
                offset = sel + 1 - visible_count;
            }
        }
    }

    let mut card_rects = Vec::new();
    let show_scrollbar = total > visible_count && rect.width > 2;
    let content_w = if show_scrollbar {
        rect.width.saturating_sub(3)
    } else {
        rect.width.saturating_sub(2)
    };
    let mut cy = inner_y;
    for (ci, card) in cards.iter().enumerate().take(total).skip(offset) {
        let h = if compact {
            compact_card_height(&card.title, content_w)
        } else {
            card_h
        };
        if cy + h > inner_y + inner_h {
            break;
        }
        card_rects.push((ci, Rect::new(rect.x + 1, cy, content_w, h)));
        cy += h;
    }

    let scrollbar_rect = if show_scrollbar {
        Some(Rect::new(
            rect.x + rect.width.saturating_sub(2),
            inner_y,
            1,
            inner_h,
        ))
    } else {
        None
    };

    ColLayout {
        idx,
        rect,
        cards: card_rects,
        scroll: ScrollInfo {
            offset,
            total,
            visible: visible_count,
        },
        scrollbar_rect,
    }
}
