use ratatui::layout::Rect;

use crate::app::App;

use super::{
    board_body_area, board_header_area, compact_filter_rows, LayoutMode, CARD_H, COMPACT_CARD_H,
    MIN_COL_W,
};

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
    // Keep the board viewport below the persistent top chrome and above the
    // persistent action/footer chrome even while a sheet or detail view is
    // open. Overlays are drawn later into this same content region.
    let main = board_body_area(area);
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

/// Compact: exactly one column at full content width, with a three-control
/// navigator below the persistent identity/filter rows.
fn board_layout_compact(app: &App, area: Rect) -> BoardLayout {
    let main = board_body_area(area);
    let n = app.board.columns.len();
    if main.width == 0 {
        return BoardLayout {
            cols: Vec::new(),
            compact_header: None,
        };
    }
    let rect = Rect::new(main.x, main.y, main.width, main.height);
    let header_area = board_header_area(area);
    // Compact's column navigator sits below the brand/board/filter rows.
    let nav_y = header_area
        .y
        .saturating_add(1)
        .saturating_add(compact_filter_rows(header_area.width));
    let prev_w = 5.min(header_area.width);
    let next_w = 5.min(header_area.width.saturating_sub(prev_w));
    let header = CompactHeader {
        prev: Rect::new(header_area.x, nav_y, prev_w, 1),
        switch: Rect::new(
            header_area.x + prev_w,
            nav_y,
            header_area
                .width
                .saturating_sub(prev_w)
                .saturating_sub(next_w),
            1,
        ),
        next: Rect::new(header_area.right().saturating_sub(next_w), nav_y, next_w, 1),
    };
    if n == 0 {
        return BoardLayout {
            cols: Vec::new(),
            compact_header: Some(header),
        };
    }
    let idx = app.sel_col.min(n - 1);
    let col = col_layout_with_header(app, idx, rect, COMPACT_CARD_H, 0, 0, true);
    BoardLayout {
        cols: vec![col],
        compact_header: Some(header),
    }
}

fn col_layout(app: &App, idx: usize, rect: Rect, card_h: u16) -> ColLayout {
    // Bordered box: 1 header row (border+title) + 1 bottom border row.
    col_layout_with_header(app, idx, rect, card_h, 1, 1, false)
}

/// Rows a Compact card needs: 6 when its title/id fits on one line, 7 when
/// the title wraps to two (borders + title rows + status + two metadata rows,
/// matching how `board::draw_card` splits the rect). Board cards deliberately
/// have no Edit/Delete action row; keyboard `e`/`d` remains the compact route.
fn compact_card_height(id: i64, title: &str, content_w: u16) -> u16 {
    let usable = content_w.saturating_sub(2).max(1);
    let title = format!("#{id} {title}");
    let lines = super::detail::wrapped_row_count(&title, usable).clamp(1, 2) as u16;
    // Title rows + status + harness/permission and model/effort, with two
    // borders.
    lines + 5
}

/// Shared card-rect + scroll computation. `header_h` is how many rows at the
/// top of `rect` are consumed by a title/header (1 for the bordered box, 2 for
/// the Compact header); `reserve_bottom` is extra rows reserved below the card
/// list (1 for the bordered box's bottom border, 0 for the borderless Compact
/// column which already sits inside the board content region). `compact` selects the
/// variable 6/7-row Compact card sizing over the fixed `card_h`.
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
    let sel = if idx == app.sel_col {
        app.sel_card.min(total.saturating_sub(1))
    } else {
        0
    };

    // Compact cards have real variable heights (6/7 rows). Derive overflow,
    // offsets, visible count and rendered rects from the same height vector so
    // the scrollbar and wheel logic can never disagree with the frame.
    let base_content_w = rect.width.saturating_sub(2);
    let heights_for = |width: u16| {
        cards
            .iter()
            .map(|card| {
                if compact {
                    compact_card_height(card.id, &card.title, width)
                } else {
                    card_h
                }
            })
            .collect::<Vec<_>>()
    };
    let mut heights = heights_for(base_content_w);
    let total_height = heights.iter().copied().fold(0u16, u16::saturating_add);
    let show_scrollbar = total_height > inner_h && rect.width > 2;
    let content_w = if show_scrollbar {
        rect.width.saturating_sub(3)
    } else {
        base_content_w
    };
    if show_scrollbar {
        heights = heights_for(content_w);
    }

    let page_end = |start: usize| {
        let mut used = 0u16;
        let mut end = start;
        while end < total && used.saturating_add(heights[end]) <= inner_h {
            used = used.saturating_add(heights[end]);
            end += 1;
        }
        end
    };
    let mut last_page_start = total;
    let mut used = 0u16;
    while last_page_start > 0 && used.saturating_add(heights[last_page_start - 1]) <= inner_h {
        last_page_start -= 1;
        used = used.saturating_add(heights[last_page_start]);
    }

    let any_fits = heights.iter().any(|height| *height <= inner_h);
    let mut offset = if any_fits {
        app.col_scroll
            .get(&column.id)
            .copied()
            .unwrap_or(0)
            .min(last_page_start)
    } else {
        0
    };
    if idx == app.sel_col && total > 0 && any_fits {
        if sel < offset {
            offset = sel;
        } else if sel >= page_end(offset) {
            let mut start = sel;
            let mut selected_page_height = heights[sel];
            while start > 0 && selected_page_height.saturating_add(heights[start - 1]) <= inner_h {
                start -= 1;
                selected_page_height = selected_page_height.saturating_add(heights[start]);
            }
            offset = start;
        }
    }
    let end = page_end(offset);
    let visible_count = end.saturating_sub(offset);

    let mut card_rects = Vec::with_capacity(visible_count);
    let mut cy = inner_y;
    for (ci, h) in heights.iter().copied().enumerate().take(end).skip(offset) {
        card_rects.push((ci, Rect::new(rect.x + 1, cy, content_w, h)));
        cy = cy.saturating_add(h);
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
