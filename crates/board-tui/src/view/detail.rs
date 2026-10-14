use board_core::engine::{format_duration, run_elapsed};
use board_core::protocol::{parse_timestamp, CardDetail};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, DetailScrollTarget};
use crate::widgets::{
    button_text, render_button_chip_at, ActionButton, ActionStrip, ActionTone, UiAction,
};

use super::{
    board_body_area_for, sheet_area_for_app, status_glyph, status_label, truncate,
    NARROW_DETAIL_WIDTH,
};

// -- detail ------------------------------------------------------------------

fn detail_panel_area(app: &App, area: Rect) -> Rect {
    if app.detail_fullscreen {
        board_body_area_for(app, area)
    } else {
        sheet_area_for_app(app, app.layout_mode(), 120, 30, area)
    }
}

fn detail_control_labels(_panel_width: u16, _fullscreen: bool) -> (&'static str, &'static str) {
    // Keep the hit zones and `f` shortcut, but make the title controls compact
    // icon buttons so a long card title never competes with button text.
    ("X", "□")
}

fn detail_card_action_buttons(detail: &CardDetail) -> Vec<ActionButton<'_>> {
    let card = &detail.card;
    let mut buttons = vec![
        ActionButton {
            label: "Edit card",
            compact_label: "Edit",
            action: UiAction::EditCard,
            tone: ActionTone::Normal,
        },
        ActionButton {
            label: "Duplicate card",
            compact_label: "Duplicate",
            action: UiAction::DuplicateCard,
            tone: ActionTone::Normal,
        },
        ActionButton {
            label: if card.archived_at.is_some() {
                "Restore card"
            } else {
                "Archive card"
            },
            compact_label: if card.archived_at.is_some() {
                "Restore"
            } else {
                "Archive"
            },
            action: UiAction::ArchiveCard,
            tone: ActionTone::Destructive,
        },
    ];
    if card.status == board_core::protocol::CardStatus::Awaiting {
        buttons.push(ActionButton {
            label: "Confirm done",
            compact_label: "Confirm",
            action: UiAction::ConfirmAwaiting,
            tone: ActionTone::Primary,
        });
    }
    // Config-defined actions (`[[card_action]]`), bound to `1`-`9` in order.
    buttons.extend(
        detail
            .actions
            .iter()
            .enumerate()
            .map(|(i, action)| ActionButton {
                label: action.label.as_str(),
                compact_label: action.label.as_str(),
                action: UiAction::CardAction(i as u8),
                tone: ActionTone::Normal,
            }),
    );
    buttons.push(ActionButton {
        label: "Add comment",
        compact_label: "Add",
        action: UiAction::AddComment,
        tone: ActionTone::Normal,
    });
    buttons
}

fn detail_run_action_buttons() -> [ActionButton<'static>; 3] {
    [
        ActionButton {
            label: "Open",
            compact_label: "Open",
            action: UiAction::FocusRunPane,
            tone: ActionTone::Primary,
        },
        ActionButton {
            label: "Retry",
            compact_label: "Retry",
            action: UiAction::RetryRun,
            tone: ActionTone::Normal,
        },
        ActionButton {
            label: "Cancel",
            compact_label: "Cancel",
            action: UiAction::CancelRun,
            tone: ActionTone::Destructive,
        },
    ]
}

fn compact_action_row_width(buttons: &[ActionButton<'_>]) -> u16 {
    buttons
        .iter()
        .map(|button| button_text(button.compact_label).chars().count() as u16)
        .fold(buttons.len().saturating_sub(1) as u16, u16::saturating_add)
}

fn pack_compact_action_rows<'a>(
    buttons: &[ActionButton<'a>],
    width: u16,
) -> Vec<Vec<ActionButton<'a>>> {
    let mut rows: Vec<Vec<ActionButton<'a>>> = Vec::new();
    for button in buttons.iter().copied() {
        let can_append = rows.last().is_some_and(|row| {
            compact_action_row_width(row)
                .saturating_add(button_text(button.compact_label).chars().count() as u16 + 1)
                <= width
        });
        if can_append {
            rows.last_mut().expect("row exists").push(button);
        } else {
            rows.push(vec![button]);
        }
    }
    rows
}

/// Intrinsic width a rail control wants: its full `[ Label ]` chip when that
/// fits the row, otherwise the shorter compact chip.
fn detail_rail_button_width(button: &ActionButton<'_>, row_width: u16) -> u16 {
    let full = button_text(button.label).chars().count() as u16;
    if full <= row_width {
        full
    } else {
        button_text(button.compact_label).chars().count() as u16
    }
}

/// True when the whole rail still fits on one row with full labels — the
/// single-row layout every pre-card-action build produced. Kept so ordinary
/// detail views render exactly as before; only a rail that cannot fit wraps.
fn card_rail_fits_one_row(buttons: &[ActionButton<'_>], width: u16) -> bool {
    let full: u16 = buttons
        .iter()
        .map(|button| button_text(button.label).chars().count() as u16)
        .sum();
    let gaps = buttons.len().saturating_sub(1) as u16;
    full.saturating_add(gaps) <= width
}

/// Greedy multi-row packing for the Regular/Wide card rail. Every row's
/// controls are individually identifiable (full chip width when it fits,
/// compact otherwise) and each row fits `width` with one-column gaps.
fn pack_detail_action_rows<'a>(
    buttons: &[ActionButton<'a>],
    width: u16,
) -> Vec<Vec<ActionButton<'a>>> {
    let mut rows: Vec<Vec<ActionButton<'a>>> = Vec::new();
    for button in buttons.iter().copied() {
        let bw = detail_rail_button_width(&button, width);
        let can_append = rows.last().is_some_and(|row| {
            let used: u16 = row.iter().map(|b| detail_rail_button_width(b, width)).sum();
            let gaps = row.len() as u16; // existing gaps + the new one
            used.saturating_add(gaps).saturating_add(bw) <= width
        });
        if can_append {
            rows.last_mut().expect("row exists").push(button);
        } else {
            rows.push(vec![button]);
        }
    }
    rows
}

/// Compact detail action rows keep the card controls together and the three
/// run controls together. At the 40-column content width an awaiting card's
/// final `[ Add ]` cell shares the second row with `[ Open ] [ Retry ]
/// [ Cancel ]`, so all seven controls remain named in only two rows.
fn compact_detail_action_rows(detail: &CardDetail, width: u16) -> Vec<Vec<ActionButton<'_>>> {
    let card = detail_card_action_buttons(detail);
    let runs = detail_run_action_buttons();
    let mut rows = pack_compact_action_rows(&card, width);
    let run_width = compact_action_row_width(&runs);

    if rows.len() > 1 {
        let last_width = compact_action_row_width(rows.last().expect("row exists"));
        if last_width.saturating_add(1).saturating_add(run_width) <= width {
            let run_row = rows.pop().expect("row exists");
            let mut combined = run_row;
            combined.extend(runs);
            rows.push(combined);
            return rows;
        }
    }

    rows.extend(pack_compact_action_rows(&runs, width));
    rows
}

/// The packed action-rail rows for the current detail and geometry: the
/// exact row list the renderer draws at full rail width. Compact keeps card
/// and run controls in their grouped rows; Regular/Wide uses the measured
/// single- or multi-row rail (Add comment and the run controls join/leave
/// the rail exactly as the renderer decides, so paging, hit-testing, and
/// drawing always agree).
/// A single-row Regular/Wide rail is one row holding every control, matching
/// the unchanged one-row layout ordinary cards always produced.
///
/// NOTE: a 1-row *overflowing* rail does not draw these full-width rows.
/// The single physical row is shared with the pager (`rail_single_row_split`),
/// so the renderer repacks for the narrower action half via
/// `detail_rail_single_row_rows` and pages those rows instead — same rows in
/// drawing and navigation, so every page's chips fit visibly in 22 cells.
pub fn detail_rail_rows<'a>(
    detail: &'a CardDetail,
    layout: &DetailLayout,
    compact: bool,
) -> Vec<Vec<ActionButton<'a>>> {
    if compact {
        return compact_detail_action_rows(detail, layout.card_actions.width);
    }
    let mut card_buttons = detail_card_action_buttons(detail);
    // Add comment lives in its contextual section bar whenever that bar
    // fits; otherwise it stays in this card rail with the other controls.
    if comment_actions_fit(layout) {
        card_buttons.pop();
    }
    if !run_actions_fit(layout) {
        card_buttons.extend(detail_run_action_buttons());
    }
    if card_rail_fits_one_row(&card_buttons, layout.card_actions.width) {
        vec![card_buttons]
    } else {
        pack_detail_action_rows(&card_buttons, layout.card_actions.width)
    }
}

/// The packed action-rail rows repacked for a 1-row overflowing rail's
/// *action half* (`rail_single_row_split().0.width`, 22 at the 40-column
/// floor): the exact row list the single-row renderer draws and pages.
/// Same button order and card/run grouping as `detail_rail_rows`, only
/// packed narrower — so each page holds solely controls that fit visibly in
/// the action area, and `render_compact`/`render_full_row` never squeeze a
/// full-width multi-control row into 22 anonymous cells. A lone oversized
/// control stays one-per-page with its identifiable truncated prefix.
pub fn detail_rail_single_row_rows<'a>(
    detail: &'a CardDetail,
    layout: &DetailLayout,
    compact: bool,
    action_width: u16,
) -> Vec<Vec<ActionButton<'a>>> {
    if compact {
        return compact_detail_action_rows(detail, action_width);
    }
    let mut card_buttons = detail_card_action_buttons(detail);
    if comment_actions_fit(layout) {
        card_buttons.pop();
    }
    if !run_actions_fit(layout) {
        card_buttons.extend(detail_run_action_buttons());
    }
    if card_rail_fits_one_row(&card_buttons, action_width) {
        vec![card_buttons]
    } else {
        pack_detail_action_rows(&card_buttons, action_width)
    }
}
/// How many rail pages the packed `rows` need at `height` rows. A fitting
/// rail is a single page (no pager row); an overflowing rail with `height >= 2`
/// reserves its last row for the pager, so each page holds `height - 1`
/// packed rows. A 1-row overflowing rail shows one packed row per page and
/// shares that row with a compact pager (see `rail_single_row_split`).
pub fn rail_page_count(rows: &[Vec<ActionButton<'_>>], height: u16) -> usize {
    if height == 0 {
        return 1;
    }
    if rows.len() as u16 <= height {
        return 1;
    }
    let per_page = (height - 1).max(1) as usize;
    rows.len().div_ceil(per_page)
}

/// Which packed rows the current page shows: `[start, end)` into `rows`.
/// A fitting rail shows everything; an overflowing rail with `height >= 2`
/// shows one page's window (`height - 1` rows, the last rail row being the
/// pager). A 1-row overflowing rail shows exactly one packed row per page
/// (`row_count` pages); the renderer shares that single row between the
/// row's chips and the pager, so every page keeps a labelled, clickable
/// action alongside the paging affordance — digits stay the execution path
/// for hidden rows on every height.
pub fn rail_page_window(row_count: usize, height: u16, page: usize) -> (usize, usize) {
    if height == 0 {
        return (0, 0);
    }
    if row_count as u16 <= height {
        return (0, row_count);
    }
    if height <= 1 {
        // Single-row overflow: one packed row per page; the renderer draws
        // that row's chips alongside the pager in the same physical row.
        let pages = row_count.max(1);
        let current = page.min(pages - 1);
        return (current, (current + 1).min(row_count));
    }
    let per_page = (height - 1) as usize;
    let pages = row_count.div_ceil(per_page).max(1);
    let current = page.min(pages - 1);
    let start = current * per_page;
    (start, (start + per_page).min(row_count))
}

/// Number of rail controls hidden off the current page (the pager's `+N`).
fn rail_hidden_count(rows: &[Vec<ActionButton<'_>>], start: usize, end: usize) -> usize {
    rows.iter()
        .enumerate()
        .filter(|(i, _)| *i < start || *i >= end)
        .map(|(_, row)| row.len())
        .sum()
}

/// Split a 1-row overflowing rail into its action half (left) and pager half
/// (right) with a single blank column between them. The pager gets up to 15
/// cells — exactly `"+NN more ‹NN/NN›"` — while the action keeps at least 8
/// (`"[ Edit ]"`), so at the 40-column content width (rail width 38) the
/// split is 22 action + 1 gap + 15 pager: the first page keeps `[ Edit ]`
/// legible and every long-label page keeps its `"Deploy service N"`
/// prefix alongside a `‹i/n›` pager with both click halves. Below width 3
/// the dialog cannot show both; the caller documents that floor and digits
/// remain the execution path there.
///
/// Public so navigation (`app::detail_rail_page`) repacks against the same
/// action-half width the renderer draws — never the full rail width.
pub fn rail_single_row_split(rect: Rect) -> (Rect, Rect) {
    if rect.width == 2 {
        return (
            Rect::new(rect.x, rect.y, 1, 1),
            Rect::new(rect.x.saturating_add(1), rect.y, 1, 1),
        );
    }
    if rect.width <= 1 {
        return (rect, Rect::default());
    }
    // Reserve the pager's 15 cells while keeping 8 + 1 gap for the action.
    // At narrower widths the pager shrinks first; both halves stay non-empty
    // whenever the rail is at least 3 wide.
    let pager_w = 15
        .min(rect.width.saturating_sub(9))
        .max(1)
        .min(rect.width.saturating_sub(2));
    let action_w = rect.width.saturating_sub(pager_w).saturating_sub(1);
    let action = Rect::new(rect.x, rect.y, action_w, 1);
    let pager = Rect::new(
        rect.x.saturating_add(action_w).saturating_add(1),
        rect.y,
        pager_w,
        1,
    );
    (action, pager)
}

/// Bounded pager for a height-capped action rail, drawn in the last
/// allocated rail row: `+N more ‹i/n› (1-9 ←/→)`. Every packed row stays one
/// `←`/`→` (or `<`/`>`/wheel/`‹`/`›` click) away, so hidden labels are
/// revealed on demand and every control keeps a mouse target on its page —
/// hidden rows register no hit zones. The pager's own halves page
/// (`RailPagePrev`/`RailPageNext`), so the affordance itself is clickable.
fn render_rail_pager(
    f: &mut Frame,
    rect: Rect,
    page: usize,
    pages: usize,
    hidden: usize,
    hit_map: &mut crate::widgets::HitMap,
) {
    use crate::widgets::Zone;

    if rect.is_empty() {
        return;
    }
    let text = truncate(
        &format!("+{hidden} more ‹{}/{}› (1-9 ←/→)", page + 1, pages),
        rect.width as usize,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(Color::Gray),
        )))
        .alignment(ratatui::layout::Alignment::Center),
        rect,
    );
    // Two halves so the affordance pages both ways by mouse. No overlap
    // with the action rows above (different y), and empty halves (a 1-wide
    // pager) register nothing.
    let mid = rect.width / 2;
    let prev = Rect::new(rect.x, rect.y, mid, 1);
    let next = Rect::new(
        rect.x.saturating_add(mid),
        rect.y,
        rect.width.saturating_sub(mid),
        1,
    );
    if !prev.is_empty() {
        hit_map.push(prev, Zone::RailPagePrev);
    }
    if !next.is_empty() {
        hit_map.push(next, Zone::RailPageNext);
    }
}

/// Click target for the popup/fullscreen action rendered in the detail title.
pub fn detail_toggle_rect(app: &App, area: Rect) -> Rect {
    let panel = detail_panel_area(app, area);
    let (_, toggle_label) = detail_control_labels(panel.width, app.detail_fullscreen);
    let label_w = button_text(toggle_label).chars().count() as u16;
    Rect::new(
        panel.x + panel.width.saturating_sub(label_w + 1),
        panel.y,
        label_w,
        1,
    )
}

fn wrapped_line_count(text: &str, width: u16) -> u16 {
    let width = width.max(1) as usize;
    text.lines()
        .map(|line| line.chars().count().max(1).div_ceil(width) as u16)
        .sum::<u16>()
        .max(1)
}

/// Greedy word-wrap row count for a single comment string `"[author] body"`,
/// approximating ratatui `Wrap { trim: false }`: each rendered line holds as
/// many space-separated words as fit in `width` (by `chars().count()`), an
/// over-long word is hard-broken, and a blank source line still occupies one
/// row. Used for scroll clamping and section sizing so the scroll offset never
/// runs past the real rendered content. Also reused by `layout` to size
/// Compact board cards (1 vs. 2 title rows).
pub(super) fn wrapped_row_count(text: &str, width: u16) -> usize {
    let width = (width as usize).max(1);
    text.lines()
        .map(|line| {
            if line.is_empty() {
                return 1;
            }
            let mut rows = 1;
            let mut col = 0usize;
            let mut start_of_line = true;
            for word in line.split(' ') {
                let wl = word.chars().count();
                let sep = if start_of_line { 0 } else { 1 };
                if col + sep + wl <= width {
                    col += sep + wl;
                } else {
                    rows += 1;
                    col = wl.min(width);
                    if wl > width {
                        // Hard-break an over-long word across further rows.
                        rows += (wl - width).div_ceil(width);
                        col = wl % width;
                    }
                }
                start_of_line = false;
            }
            rows
        })
        .sum::<usize>()
        .max(1)
}

/// Per-comment `(start_row, row_count)` in the wrapped comments block, using
/// exactly the measurement the renderer draws: a 1-char focus gutter (`▸` on
/// the focused comment, a space otherwise — uniform width either way) then
/// `"[author] body"`. Exposed so the app layer can map a comment index to its
/// rows (scroll clamping, focus-follow) and mouse can map a clicked row back
/// to a comment index.
pub fn comment_row_spans(detail: &CardDetail, width: u16) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(detail.comments.len());
    let mut row = 0usize;
    for c in &detail.comments {
        let n = wrapped_row_count(&format!(" [{}] {}", c.author, c.body), width);
        out.push((row, n));
        row += n;
    }
    out
}

/// Total wrapped rows the comment bodies occupy at `width`, one block per
/// comment (the gutter + `"[author] body"`). Exposed so the app layer can
/// clamp comment scrolling (row-based) to the real rendered height.
pub fn comment_wrapped_rows(detail: &CardDetail, width: u16) -> usize {
    comment_row_spans(detail, width)
        .iter()
        .map(|&(_, n)| n)
        .sum::<usize>()
        .max(1)
}

/// Allocate as many wrapped rows as possible to the two metadata sections.
/// When the available budget is too small, each section keeps its closed frame
/// and the renderer supplies an explicit ellipsis for the value.
fn metadata_section_heights(
    configuration: &str,
    session: &str,
    width: u16,
    available: u16,
) -> [u16; 2] {
    let needs = [
        metadata_height(configuration, width),
        metadata_height(session, width),
    ];
    let mut heights = [0u16; 2];
    if available >= 3 {
        heights[0] = 3;
    }
    if available >= 6 {
        heights[1] = 3;
    }

    let mut remaining = available.saturating_sub(heights.iter().sum());
    while remaining > 0 {
        let Some((idx, deficit)) = (0..2)
            .map(|idx| (idx, needs[idx].saturating_sub(heights[idx])))
            .filter(|(idx, deficit)| {
                *deficit > 0 && (heights[*idx] > 0 || remaining >= MIN_CLOSED_SECTION_HEIGHT)
            })
            .max_by_key(|(_, deficit)| *deficit)
        else {
            break;
        };
        let added = if heights[idx] == 0 {
            MIN_CLOSED_SECTION_HEIGHT
        } else {
            1
        };
        heights[idx] += added;
        remaining = remaining.saturating_sub(added);
        debug_assert!(deficit > 0);
    }
    heights
}

fn metadata_height(text: &str, width: u16) -> u16 {
    wrapped_row_count(text, width).min(u16::MAX.saturating_sub(2) as usize) as u16 + 2
}

/// A bordered section needs a title row, one content row, and a bottom border
/// to be legible. Zero means "not rendered"; heights 1–2 are never handed to
/// a section renderer, which prevents a short viewport from leaving a lone top
/// border behind the action rail.
const MIN_CLOSED_SECTION_HEIGHT: u16 = 3;

fn closed_section_height(height: u16) -> u16 {
    if height >= MIN_CLOSED_SECTION_HEIGHT {
        height
    } else {
        0
    }
}

fn preferred_section_height(available: u16, preferred: u16) -> u16 {
    if available >= MIN_CLOSED_SECTION_HEIGHT {
        preferred
    } else {
        0
    }
}

fn detail_metadata(detail: &CardDetail) -> (String, String) {
    let card = &detail.card;
    // Daemon-stamped labels: ready display strings, rendered verbatim. The
    // `default …` markers mean "unset → harness/daemon default".
    let configuration = format!(
        "Harness · Model: {} · {} · effort {} · perm {}",
        card.harness, card.labels.model, card.labels.effort, card.labels.permission
    );

    let space = format!(
        "{}:{}",
        card.space_kind.as_str(),
        card.space_ref.as_deref().unwrap_or("-")
    );
    let session = format!("Herdr session: {} · Space: {space}", card.labels.session);
    (configuration, session)
}

/// Size sections by content. Surplus height stays outside their borders; when
/// content exceeds the viewport, rows go to the greatest unmet demand first.
///
/// `comments_active` accounts for the action bar's row in the comments
/// section's demand (`needs[1]`) when it would render (focused + non-empty),
/// so the section can grow to fit it rather than clip.
fn detail_section_heights(
    detail: &CardDetail,
    width: u16,
    available: u16,
    comments_active: bool,
) -> ([u16; 3], u16) {
    let desc_lines = wrapped_line_count(&detail.card.description, width);
    let comment_lines = comment_wrapped_rows(detail, width) as u16;
    let run_lines = (detail.runs.len() as u16).max(1);
    let bar_row = if comments_active && !detail.comments.is_empty() {
        1
    } else {
        0
    };
    // Full-card sections: title row + bottom border (+ in-card action bar for
    // the histories), so each needs 4 rows to expose one content row.
    let needs = [desc_lines + 2, comment_lines + 2 + bar_row, run_lines + 2];

    // Preserve the contextual action rows before description whitespace when
    // persistent board chrome leaves a short Regular/Compact content region.
    // A three-row history card still has a title, one action row, and its
    // bottom border; its history body can collapse to zero rows rather than
    // making `[ Edit ]`/`[ Open ]` disappear entirely.
    let mut heights = [0u16; 3];
    if available >= 3 {
        heights[2] = 3;
    }
    if available >= 6 {
        heights[1] = 3;
    }
    if available >= 9 {
        heights[0] = 3;
    }
    // Never exceed the available budget: shed Desc first, then Comments, then
    // Runs, so the Layout constraints cannot overflow the inner area on tiny
    // terminals. Bounded loop: each pass removes exactly one row (down to 0),
    // so it always terminates even when `available` is 0.
    for _ in 0..128 {
        if heights.iter().copied().sum::<u16>() <= available {
            break;
        }
        if heights[0] > 0 {
            heights[0] -= 1;
        } else if heights[1] > 0 {
            heights[1] -= 1;
        } else {
            heights[2] -= 1;
        }
    }
    let mut remaining = available.saturating_sub(heights.iter().copied().sum::<u16>());
    while remaining > 0 {
        let Some((idx, deficit)) = (0..3)
            .map(|idx| (idx, needs[idx].saturating_sub(heights[idx])))
            .filter(|(idx, deficit)| {
                *deficit > 0 && (heights[*idx] > 0 || remaining >= MIN_CLOSED_SECTION_HEIGHT)
            })
            .max_by_key(|(_, deficit)| *deficit)
        else {
            break;
        };
        let added = if heights[idx] == 0 {
            MIN_CLOSED_SECTION_HEIGHT
        } else {
            1
        };
        heights[idx] += added;
        remaining = remaining.saturating_sub(added);
        debug_assert!(deficit > 0);
    }
    (heights, remaining)
}

pub struct DetailLayout {
    pub panel: Rect,
    pub status: Rect,
    pub configuration: Rect,
    pub session: Rect,
    pub description: Rect,
    pub card_actions: Rect,
    pub comments: Rect,
    pub comment_actions: Rect,
    pub runs: Rect,
    pub run_actions: Rect,
}

impl DetailLayout {
    fn empty(panel: Rect, inner: Rect) -> Self {
        Self {
            panel,
            status: inner,
            configuration: Rect::default(),
            session: Rect::default(),
            description: Rect::default(),
            card_actions: Rect::default(),
            comments: Rect::default(),
            comment_actions: Rect::default(),
            runs: Rect::default(),
            run_actions: Rect::default(),
        }
    }
}

/// Responsive detail geometry. Wide terminals use the approved three-pane
/// hierarchy; Regular and Compact retain one vertical reading order.
pub fn detail_layout(app: &App, area: Rect) -> DetailLayout {
    let panel = detail_panel_area(app, area);
    let inner = Block::default().borders(Borders::ALL).inner(panel);
    let Some(detail) = &app.detail else {
        return DetailLayout::empty(panel, inner);
    };

    if app.layout_mode() == super::LayoutMode::Wide {
        let usable = inner.width.saturating_sub(2);
        let left_w = usable.saturating_mul(35) / 100;
        let center_w = usable.saturating_mul(40) / 100;
        let right_w = usable.saturating_sub(left_w).saturating_sub(center_w);
        let columns = [
            Rect::new(inner.x, inner.y, left_w, inner.height),
            Rect::new(
                inner.x.saturating_add(left_w).saturating_add(1),
                inner.y,
                center_w,
                inner.height,
            ),
            Rect::new(
                inner
                    .x
                    .saturating_add(left_w)
                    .saturating_add(center_w)
                    .saturating_add(2),
                inner.y,
                right_w,
                inner.height,
            ),
        ];
        let status_height = preferred_section_height(columns[0].height, 3);
        // Comment / run histories are single full-height cards; their
        // contextual action bar lives on the last inner row.
        let runs = if columns[1].height >= MIN_CLOSED_SECTION_HEIGHT {
            columns[1]
        } else {
            Rect::default()
        };
        let comments = if columns[2].height >= MIN_CLOSED_SECTION_HEIGHT {
            columns[2]
        } else {
            Rect::default()
        };
        let run_actions = Rect::new(runs.x, runs.bottom().saturating_sub(2), runs.width, 1);
        let comment_actions = Rect::new(
            comments.x,
            comments.bottom().saturating_sub(2),
            comments.width,
            1,
        );
        // The card rail is measured, not fixed at one row: it grows to as many
        // rows as the controls need so no configured action degenerates into a
        // blank, unidentifiable click target.
        let mut rail = detail_card_action_buttons(detail);
        if !comment_actions.is_empty() {
            rail.pop(); // Add comment lives in its contextual bar.
        }
        if run_actions.is_empty() {
            rail.extend(detail_run_action_buttons());
        }
        let wanted_rows = if card_rail_fits_one_row(&rail, columns[0].width) {
            1
        } else {
            pack_detail_action_rows(&rail, columns[0].width).len() as u16
        };
        let action_height = wanted_rows.min(columns[0].height.saturating_sub(status_height));
        let (configuration, session) = detail_metadata(detail);
        let metadata_heights = metadata_section_heights(
            &configuration,
            &session,
            columns[0].width.saturating_sub(2),
            columns[0]
                .height
                .saturating_sub(status_height + action_height),
        );
        let description_height = closed_section_height(columns[0].height.saturating_sub(
            status_height + metadata_heights[0] + metadata_heights[1] + action_height,
        ));
        let left = Layout::vertical([
            Constraint::Length(status_height),
            Constraint::Length(metadata_heights[0]),
            Constraint::Length(metadata_heights[1]),
            Constraint::Length(description_height),
            Constraint::Length(action_height),
        ])
        .split(columns[0]);
        return DetailLayout {
            panel,
            status: left[0],
            configuration: left[1],
            session: left[2],
            description: left[3],
            card_actions: left[4],
            comments,
            comment_actions,
            runs,
            run_actions,
        };
    }

    // Status and the card action rail are fixed first. The two metadata
    // sections then get as many wrapped rows as the remaining height allows;
    // any surplus is shared by Description, Comments, and Runs. When a narrow
    // viewport cannot fit both metadata values, the renderer deliberately
    // ellipsizes the value instead of letting a Paragraph hard-clip it.
    let (configuration, session) = detail_metadata(detail);
    let compact = app.layout_mode() == super::LayoutMode::Compact;
    let status_height = preferred_section_height(inner.height, 3);
    let max_action_height = inner.height.saturating_sub(status_height);
    let comments_active = !compact && app.detail_scroll_target == DetailScrollTarget::Comments;
    let action_height = if compact {
        compact_detail_action_rows(detail, inner.width)
            .len()
            .min(max_action_height as usize) as u16
    } else {
        // The card rail can absorb the run controls when the Run section is too
        // short, so its row count depends on the heights it leaves below it.
        // Resolve the (monotone, height-bounded) fixed point.
        let mut height = 1u16;
        loop {
            let metadata_available = inner.height.saturating_sub(status_height + height);
            let probe_metadata = metadata_section_heights(
                &configuration,
                &session,
                inner.width.saturating_sub(2),
                metadata_available,
            );
            let section_budget = inner
                .height
                .saturating_sub(status_height + probe_metadata[0] + probe_metadata[1] + height);
            let (section_h, _) = detail_section_heights(
                detail,
                inner.width.saturating_sub(1),
                section_budget,
                comments_active,
            );
            let mut rail = detail_card_action_buttons(detail);
            if section_h[1] >= MIN_CLOSED_SECTION_HEIGHT {
                rail.pop(); // Add comment lives in the comments bar.
            }
            if section_h[2] < MIN_CLOSED_SECTION_HEIGHT {
                rail.extend(detail_run_action_buttons());
            }
            let rows = if card_rail_fits_one_row(&rail, inner.width) {
                1
            } else {
                pack_detail_action_rows(&rail, inner.width).len() as u16
            };
            let next = rows.min(max_action_height);
            if next <= height {
                break;
            }
            height = next;
        }
        height
    };
    let metadata_available = inner.height.saturating_sub(status_height + action_height);
    // At the 40x20 content size, two named action rows leave room for one
    // closed metadata card but not two separate three-row cards. Keep both
    // values visible in a single four-row frame instead of dropping Session
    // or leaving a partial border behind.
    let metadata_combined = compact && (4..6).contains(&metadata_available);
    let metadata_heights = if metadata_combined {
        [4, 0]
    } else {
        metadata_section_heights(
            &configuration,
            &session,
            inner.width.saturating_sub(2),
            metadata_available,
        )
    };
    let section_budget = inner
        .height
        .saturating_sub(status_height + metadata_heights[0] + metadata_heights[1] + action_height);
    let (section_h, spacer) = detail_section_heights(
        detail,
        inner.width.saturating_sub(1),
        section_budget,
        comments_active,
    );
    let description_height = if section_h[0] >= MIN_CLOSED_SECTION_HEIGHT {
        section_h[0].saturating_add(spacer)
    } else {
        0
    };
    let chunks = Layout::vertical([
        Constraint::Length(status_height),
        Constraint::Length(metadata_heights[0]),
        Constraint::Length(metadata_heights[1]),
        Constraint::Length(description_height),
        Constraint::Length(action_height),
        Constraint::Length(section_h[1]),
        Constraint::Length(section_h[2]),
    ])
    .split(inner);
    let comments = chunks[5];
    let runs = chunks[6];
    let comment_actions = if compact && !comments.is_empty() {
        Rect::default()
    } else {
        Rect::new(
            comments.x,
            comments.bottom().saturating_sub(2),
            comments.width,
            1,
        )
    };
    let run_actions = if compact && !runs.is_empty() {
        Rect::default()
    } else {
        Rect::new(runs.x, runs.bottom().saturating_sub(2), runs.width, 1)
    };
    DetailLayout {
        panel,
        status: chunks[0],
        configuration: chunks[1],
        session: chunks[2],
        description: chunks[3],
        card_actions: chunks[4],
        comments,
        comment_actions,
        runs,
        run_actions,
    }
}

/// Selected-comment actions are meaningful only while Comments owns focus.
pub fn comments_action_bar_shown(app: &App, layout: &DetailLayout) -> bool {
    app.detail.as_ref().is_some_and(|detail| {
        app.detail_scroll_target == DetailScrollTarget::Comments
            && !detail.comments.is_empty()
            && layout.comments.height >= 3
            && !layout.comment_actions.is_empty()
    })
}

/// The comments viewport below its divider. The contextual bar has its own
/// rectangle and therefore never steals or overlaps a history row.
pub fn comments_viewport(_app: &App, layout: &DetailLayout) -> (Rect, usize) {
    // A section always reserves its title and bottom border. Reserve the
    // third row only when an in-card action bar is actually rendered; Compact
    // detail actions live in the named card rail instead.
    let reserved = 2 + usize::from(!layout.comment_actions.is_empty());
    let visible = layout.comments.height.saturating_sub(reserved as u16);
    (
        Rect::new(
            layout.comments.x,
            layout.comments.y.saturating_add(1),
            layout.comments.width,
            visible,
        ),
        visible as usize,
    )
}

/// Visible run rows inside the runs card. The title and bottom border are
/// always reserved, and the action row is reserved only when that row is
/// rendered in the section. This is the single arithmetic source used by
/// drawing, scrolling, and hit-testing.
pub fn runs_viewport_height(layout: &DetailLayout) -> usize {
    let reserved = 2 + usize::from(!layout.run_actions.is_empty());
    layout.runs.height.saturating_sub(reserved as u16) as usize
}

fn comment_actions_fit(layout: &DetailLayout) -> bool {
    layout.comments.height >= 3
        && !layout.comment_actions.is_empty()
        && layout.comment_actions.y >= layout.comments.y
        && layout.comment_actions.y < layout.comments.bottom()
}

fn run_actions_fit(layout: &DetailLayout) -> bool {
    layout.runs.height >= 3
        && !layout.run_actions.is_empty()
        && layout.run_actions.y >= layout.runs.y
        && layout.run_actions.y < layout.runs.bottom()
}

pub(super) fn detail_section_title(
    name: &str,
    total: usize,
    offset: usize,
    visible: usize,
) -> String {
    let hidden_above = offset > 0;
    let hidden_below = offset.saturating_add(visible.max(1)) < total;
    let arrows = match (hidden_above, hidden_below) {
        (true, true) => " ↑↓",
        (true, false) => " ↑",
        (false, true) => " ↓",
        (false, false) => "",
    };
    format!("{name}{arrows}")
}

fn section_block<'a>(title: &'a str, focused: bool) -> Block<'a> {
    let style = if focused {
        Color::LightBlue
    } else {
        Color::DarkGray
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(style))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD),
        ))
}

fn metadata_line(label: &'static str, value: String, width: u16, max_rows: u16) -> Line<'static> {
    if width == 0 || max_rows == 0 {
        return Line::default();
    }

    let full = format!("{label}{value}");
    let (label, value) = if wrapped_row_count(&full, width) <= max_rows as usize {
        (label.to_string(), value)
    } else {
        // A metadata section can become smaller than its preferred wrapped
        // height on Compact terminals. Keep the label and add an ellipsis to
        // the value in that case; a one-line bounded string cannot be clipped
        // by the Paragraph at the section border.
        let label_width = label.chars().count();
        if label_width >= width as usize {
            (truncate(label, width as usize), String::new())
        } else {
            (
                label.to_string(),
                truncate(&value, width as usize - label_width),
            )
        }
    };
    Line::from(vec![
        Span::styled(
            label,
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value, Style::default().fg(Color::White)),
    ])
}

/// Click target for the explicit close action in the detail title.
pub fn detail_close_rect(app: &App, area: Rect) -> Rect {
    let panel = detail_panel_area(app, area);
    let (close_label, _) = detail_control_labels(panel.width, app.detail_fullscreen);
    let width = button_text(close_label).chars().count() as u16;
    let toggle = detail_toggle_rect(app, area);
    Rect::new(toggle.x.saturating_sub(width + 1), panel.y, width, 1)
}

pub(super) fn draw_detail(app: &App, f: &mut Frame, area: Rect) {
    use crate::widgets::Zone;

    let Some(detail) = &app.detail else { return };
    let layout = detail_layout(app, area);
    let panel = layout.panel;
    let card = &detail.card;
    f.render_widget(Clear, panel);

    let (close_label, toggle_label) = detail_control_labels(panel.width, app.detail_fullscreen);
    let close_text = button_text(close_label);
    let toggle_text = button_text(toggle_label);
    let controls_width = close_text.chars().count() + 1 + toggle_text.chars().count();
    let title_width = panel.width.saturating_sub(2) as usize;
    let title_limit = if panel.width < NARROW_DETAIL_WIDTH {
        32
    } else {
        48
    };
    let left = format!(
        " Card #{}: {} ",
        card.id,
        truncate(&card.title, title_limit)
    );
    let left = truncate(&left, title_width.saturating_sub(controls_width + 1));
    let gap = title_width.saturating_sub(left.chars().count() + controls_width);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::LightBlue))
            .title(format!("{}{}", left, " ".repeat(gap))),
        panel,
    );
    // The title controls are exact neutral chips. Their existing hit zones and
    // the keyboard `f`/`q`/`Esc` paths are unchanged.
    let close_rect = detail_close_rect(app, area);
    let toggle_rect = detail_toggle_rect(app, area);
    let gap = toggle_rect.x.saturating_sub(close_rect.right());
    if gap > 0 {
        // The title controls sit on the border row. Clear the border glyph in
        // their visual gap so the chips read `[ X ] [ □ ]` rather than a stray
        // `─` that looks like a broken control boundary.
        f.render_widget(
            Paragraph::new(" ".repeat(gap as usize)),
            Rect::new(close_rect.right(), panel.y, gap, 1),
        );
    }
    let mut hit_map = app.hit_map.borrow_mut();
    render_button_chip_at(
        f,
        close_rect,
        close_label,
        &mut hit_map,
        Zone::Action(UiAction::CloseDetail),
    );
    render_button_chip_at(
        f,
        toggle_rect,
        toggle_label,
        &mut hit_map,
        Zone::Action(UiAction::ToggleDetail),
    );
    drop(hit_map);

    let (glyph, color) = status_glyph(card.status);
    let mut status = format!("{glyph} {}", status_label(card));
    if card.archived_at.is_some() {
        status.push_str(" · ARCHIVED");
    }
    let status = truncate(&status, layout.status.width.saturating_sub(2) as usize);
    if layout.status.height >= MIN_CLOSED_SECTION_HEIGHT {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                status,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )))
            .block(section_block("Status", false)),
            layout.status,
        );
    }

    let (configuration, session) = detail_metadata(detail);
    let metadata_combined = app.layout_mode() == super::LayoutMode::Compact
        && layout.session.is_empty()
        && layout.configuration.height == 4;
    if metadata_combined {
        let width = layout.configuration.width.saturating_sub(2);
        let lines = vec![
            metadata_line(
                "Harness · Model: ",
                configuration
                    .strip_prefix("Harness · Model: ")
                    .unwrap_or_default()
                    .to_string(),
                width,
                1,
            ),
            metadata_line(
                "Herdr session: ",
                session
                    .strip_prefix("Herdr session: ")
                    .unwrap_or_default()
                    .to_string(),
                width,
                1,
            ),
        ];
        f.render_widget(
            Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: false })
                .block(section_block("Task Configuration", false)),
            layout.configuration,
        );
    } else {
        if layout.configuration.height >= MIN_CLOSED_SECTION_HEIGHT {
            f.render_widget(
                Paragraph::new(metadata_line(
                    "Harness · Model: ",
                    configuration
                        .strip_prefix("Harness · Model: ")
                        .unwrap_or_default()
                        .to_string(),
                    layout.configuration.width.saturating_sub(2),
                    layout.configuration.height.saturating_sub(2),
                ))
                .wrap(Wrap { trim: false })
                .block(section_block("Task Configuration", false)),
                layout.configuration,
            );
        }

        if layout.session.height >= MIN_CLOSED_SECTION_HEIGHT {
            f.render_widget(
                Paragraph::new(metadata_line(
                    "Herdr session: ",
                    session
                        .strip_prefix("Herdr session: ")
                        .unwrap_or_default()
                        .to_string(),
                    layout.session.width.saturating_sub(2),
                    layout.session.height.saturating_sub(2),
                ))
                .wrap(Wrap { trim: false })
                .block(section_block("Session", false)),
                layout.session,
            );
        }
    }

    if layout.description.height >= MIN_CLOSED_SECTION_HEIGHT {
        f.render_widget(
            Paragraph::new(card.description.as_str())
                .wrap(Wrap { trim: false })
                .block(section_block("Description", false)),
            layout.description,
        );
    }

    let compact = app.layout_mode() == super::LayoutMode::Compact;
    let run_buttons = detail_run_action_buttons();
    let comments_fit = comment_actions_fit(&layout);
    let runs_fit = run_actions_fit(&layout);
    if compact {
        let rows = detail_rail_rows(detail, &layout, true);
        let height = layout.card_actions.height;
        let pages = rail_page_count(&rows, height);
        let page = app.detail_rail_page.min(pages.saturating_sub(1));
        // Never paint or hit-test outside the rectangle the layout
        // allocated. A fitting rail draws every row; a capped rail draws
        // the current page's rows and pages the rest through the pager.
        // A 1-row overflowing rail shares its single row between the page's
        // packed row and the pager, so one action stays labelled/clickable.
        let (start, end) = rail_page_window(rows.len(), height, page);
        let mut hit_map = app.hit_map.borrow_mut();
        if pages > 1 && height == 1 {
            let (action_rect, pager_rect) = rail_single_row_split(layout.card_actions);
            // Repack for the action half — never squeeze full-width rows
            // into 22 cells. Each single-row page then holds only controls
            // that fit visibly; zones exist solely for those visible chips.
            let single_rows = detail_rail_single_row_rows(detail, &layout, true, action_rect.width);
            let single_pages = rail_page_count(&single_rows, 1).max(1);
            let single_page = app.detail_rail_page.min(single_pages - 1);
            let (s_start, s_end) = rail_page_window(single_rows.len(), 1, single_page);
            if let Some(buttons) = single_rows.get(s_start) {
                ActionStrip { buttons }.render_compact(f, action_rect, &mut hit_map);
            }
            render_rail_pager(
                f,
                pager_rect,
                single_page,
                single_pages,
                rail_hidden_count(&single_rows, s_start, s_end),
                &mut hit_map,
            );
        } else {
            for (row, buttons) in rows.iter().enumerate().take(end).skip(start) {
                let rect = Rect::new(
                    layout.card_actions.x,
                    layout.card_actions.y.saturating_add((row - start) as u16),
                    layout.card_actions.width,
                    1,
                );
                ActionStrip { buttons }.render_compact(f, rect, &mut hit_map);
            }
            if pages > 1 && height > 0 {
                let pager = Rect::new(
                    layout.card_actions.x,
                    layout.card_actions.y.saturating_add(height - 1),
                    layout.card_actions.width,
                    1,
                );
                render_rail_pager(
                    f,
                    pager,
                    page,
                    pages,
                    rail_hidden_count(&rows, start, end),
                    &mut hit_map,
                );
            }
        }
    } else {
        let mut card_buttons = detail_card_action_buttons(detail);
        // Add comment lives in its contextual section bar whenever that bar
        // fits; otherwise it stays in this card rail with the other controls.
        if comments_fit {
            card_buttons.pop();
        }
        if !runs_fit {
            card_buttons.extend(run_buttons.iter().copied());
        }
        let mut hit_map = app.hit_map.borrow_mut();
        let rows = detail_rail_rows(detail, &layout, false);
        let height = layout.card_actions.height;
        let pages = rail_page_count(&rows, height);
        if height == 0 {
            // No allocated rail: paint and register nothing.
        } else if pages <= 1 {
            if card_rail_fits_one_row(&card_buttons, layout.card_actions.width) {
                // Unchanged single-row layout for ordinary cards.
                ActionStrip {
                    buttons: &card_buttons,
                }
                .render(f, layout.card_actions, &mut hit_map);
            } else {
                // Measured, wrapped rail: every visible control gets its
                // intrinsic chip width on one of the allocated rows, so nine
                // configured actions stay identifiable instead of collapsing
                // into blank click targets.
                for (row, buttons) in rows.iter().enumerate() {
                    let rect = Rect::new(
                        layout.card_actions.x,
                        layout.card_actions.y.saturating_add(row as u16),
                        layout.card_actions.width,
                        1,
                    );
                    ActionStrip { buttons }.render_full_row(f, rect, &mut hit_map);
                }
            }
        } else if height > 0 {
            // Height-capped rail: the current page's rows keep their chips
            // (and hit zones) and the last rail row pages the rest; hidden
            // rows keep no mouse zones while staying on `1`-`9`. A 1-row
            // overflow shares its single row between the page's packed row
            // and the pager instead of going pager-only.
            let page = app.detail_rail_page.min(pages.saturating_sub(1));
            let (start, end) = rail_page_window(rows.len(), height, page);
            if pages > 1 && height == 1 {
                let (action_rect, pager_rect) = rail_single_row_split(layout.card_actions);
                // Same action-half repack as Compact: page controls packed
                // for 22 cells, never full-width rows squeezed into the half.
                let single_rows =
                    detail_rail_single_row_rows(detail, &layout, false, action_rect.width);
                let single_pages = rail_page_count(&single_rows, 1).max(1);
                let single_page = app.detail_rail_page.min(single_pages - 1);
                let (s_start, s_end) = rail_page_window(single_rows.len(), 1, single_page);
                if let Some(buttons) = single_rows.get(s_start) {
                    ActionStrip { buttons }.render_full_row(f, action_rect, &mut hit_map);
                }
                render_rail_pager(
                    f,
                    pager_rect,
                    single_page,
                    single_pages,
                    rail_hidden_count(&single_rows, s_start, s_end),
                    &mut hit_map,
                );
            } else {
                for (row, buttons) in rows.iter().enumerate().take(end).skip(start) {
                    let rect = Rect::new(
                        layout.card_actions.x,
                        layout.card_actions.y.saturating_add((row - start) as u16),
                        layout.card_actions.width,
                        1,
                    );
                    ActionStrip { buttons }.render_full_row(f, rect, &mut hit_map);
                }
                let pager = Rect::new(
                    layout.card_actions.x,
                    layout.card_actions.y.saturating_add(height - 1),
                    layout.card_actions.width,
                    1,
                );
                render_rail_pager(
                    f,
                    pager,
                    page,
                    pages,
                    rail_hidden_count(&rows, start, end),
                    &mut hit_map,
                );
            }
        }
    }

    draw_comments(app, f, detail, &layout);
    draw_runs(app, f, detail, &layout);

    if comments_fit {
        draw_comment_actions(app, f, layout.comment_actions, &layout);
    }
    if runs_fit {
        ActionStrip {
            buttons: &run_buttons,
        }
        .render(f, layout.run_actions, &mut app.hit_map.borrow_mut());
    }
}

fn draw_comments(app: &App, f: &mut Frame, detail: &CardDetail, layout: &DetailLayout) {
    if layout.comments.height < MIN_CLOSED_SECTION_HEIGHT {
        return;
    }
    let active = app.detail_scroll_target == DetailScrollTarget::Comments;
    let (viewport, visible) = comments_viewport(app, layout);
    let total = comment_wrapped_rows(detail, layout.comments.width);
    let title = detail_section_title("Comments", total, app.detail_comments_scroll, visible);
    if visible == 0 {
        // The title and (when present) action row are the only safe cells in
        // this minimum-height frame; never paint a comment body underneath
        // them.
        f.render_widget(section_block(&title, active), layout.comments);
        return;
    }
    if detail.comments.is_empty() {
        f.render_widget(
            Paragraph::new("(no comments)")
                .style(Style::default().fg(Color::Gray))
                .block(section_block(&title, active)),
            layout.comments,
        );
        return;
    }
    let sel = active.then(|| app.detail_comment_sel.min(detail.comments.len() - 1));
    let lines = detail
        .comments
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (gutter, style) = focus_row_marker(sel == Some(i));
            Line::from(vec![
                gutter,
                Span::styled(
                    format!("[{}] ", c.author),
                    Style::default().fg(Color::LightCyan),
                ),
                Span::styled(c.body.clone(), style),
            ])
        })
        .collect::<Vec<_>>();
    // Paint the frame separately from the history body. The contextual
    // action row occupies the last inner row; rendering a Paragraph across
    // the whole section would let wrapped comment text paint underneath the
    // `[ Add ]`/`[ Edit ]`/`[ Delete ]`/`[ History ]` rail on short layouts.
    f.render_widget(section_block(&title, active), layout.comments);
    let body = Rect::new(
        layout.comments.x.saturating_add(1),
        layout.comments.y.saturating_add(1),
        layout.comments.width.saturating_sub(2),
        visible as u16,
    );
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .scroll((app.detail_comments_scroll as u16, 0)),
        body,
    );
    let spans = comment_row_spans(detail, layout.comments.width);
    let mut hit_map = app.hit_map.borrow_mut();
    for (i, &(start, len)) in spans.iter().enumerate() {
        let lo = start.max(app.detail_comments_scroll);
        let hi = (start + len).min(app.detail_comments_scroll + visible);
        if hi > lo {
            hit_map.push(
                Rect::new(
                    viewport.x,
                    viewport.y + (lo - app.detail_comments_scroll) as u16,
                    viewport.width,
                    (hi - lo) as u16,
                ),
                crate::widgets::Zone::CommentRow(i),
            );
        }
    }
}

fn draw_comment_actions(app: &App, f: &mut Frame, area: Rect, layout: &DetailLayout) {
    use crate::widgets::{UiAction, Zone};
    if area.is_empty() {
        return;
    }
    let selected_actions = comments_action_bar_shown(app, layout);
    let count = if selected_actions { 4 } else { 1 };
    let rects = Layout::horizontal(
        (0..count)
            .map(|_| Constraint::Ratio(1, count as u32))
            .collect::<Vec<_>>(),
    )
    .split(area);
    let labels = [
        ("Add comment", "Add"),
        ("Edit comment", "Edit"),
        ("Delete comment", "Delete"),
        ("History", "History"),
    ];
    let mut hit_map = app.hit_map.borrow_mut();
    for (idx, rect) in rects.iter().copied().enumerate() {
        let (full, compact) = labels[idx];
        let label = if button_text(full).chars().count() as u16 <= rect.width {
            full
        } else {
            compact
        };
        let zone = match idx {
            0 => Zone::Action(UiAction::AddComment),
            1 => Zone::CommentEdit,
            2 => Zone::CommentDelete,
            3 => Zone::CommentHistory,
            _ => unreachable!(),
        };
        render_button_chip_at(f, rect, label, &mut hit_map, zone);
    }
}

fn draw_runs(app: &App, f: &mut Frame, detail: &CardDetail, layout: &DetailLayout) {
    if layout.runs.height < MIN_CLOSED_SECTION_HEIGHT {
        return;
    }
    let active = app.detail_scroll_target == DetailScrollTarget::Runs;
    let selected = (!detail.runs.is_empty()).then(|| app.detail_run_sel.min(detail.runs.len() - 1));
    let visible = runs_viewport_height(layout);
    // A zero-row body has no legal scroll position. Clamping here mirrors the
    // app-layer helpers and also makes a resize safe before the next reducer
    // event has had a chance to recalculate the offset.
    let offset = if visible == 0 {
        // Keep the logical latest anchor in the title while painting no body
        // rows. This makes resize/reflow restore the same history position
        // without ever putting a row under the action rail.
        app.detail_runs_scroll
    } else {
        app.detail_runs_scroll
            .min(detail.runs.len().saturating_sub(visible))
    };
    let title = detail_section_title("Runs", detail.runs.len(), offset, visible);
    let width = (layout.runs.width as usize).saturating_sub(1);
    let items: Vec<ListItem> = if visible == 0 {
        // The title and action row (if present) are the only safe cells in a
        // three-row Runs frame. In particular, do not paint `(no runs)` in
        // the action row's cell.
        Vec::new()
    } else if detail.runs.is_empty() {
        vec![ListItem::new(Span::styled(
            "(no runs)",
            Style::default().fg(Color::Gray),
        ))]
    } else {
        detail
            .runs
            .iter()
            .enumerate()
            .skip(offset)
            .take(visible)
            .map(|(i, run)| {
                let (gutter, style) = focus_row_marker(active && selected == Some(i));
                ListItem::new(Line::from(vec![
                    gutter,
                    Span::styled(truncate(&run_row_text(app, run), width), style),
                ]))
            })
            .collect()
    };
    f.render_widget(
        List::new(items).block(section_block(&title, active)),
        layout.runs,
    );
    if !detail.runs.is_empty() {
        let mut hit_map = app.hit_map.borrow_mut();
        for (row, idx) in (offset..detail.runs.len()).take(visible).enumerate() {
            hit_map.push(
                Rect::new(
                    layout.runs.x,
                    layout.runs.y + 1 + row as u16,
                    layout.runs.width,
                    1,
                ),
                crate::widgets::Zone::RunRow(idx),
            );
        }
    }
}

/// The 1-char focus gutter (`▸` on the focused/selected row) plus the body
/// style that goes with it. The single definition shared by the comments list
/// and the runs list, so both mark their cursor identically.
///
/// The arrow is **bright** blue (`LightBlue`, the terminal's intense blue),
/// matching the blue used for the focused section's divider and the status
/// labels. Plain `Color::Blue` would be the dark navy of the 256-colour palette
/// and would lose contrast against a dark terminal background, which is the
/// opposite of the point.
fn focus_row_marker(focused: bool) -> (Span<'static>, Style) {
    if focused {
        (
            Span::styled(
                "▸",
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )
    } else {
        (Span::raw(" "), Style::default())
    }
}

/// One run row, deliberately minimal: the **run number**, the **harness**, the
/// **status** (`outcome`, or `active` while the run is still open) and **how
/// long it ran** — or has been running, since `run_duration` measures an open
/// run against `app.now`.
///
/// Nothing else. The column is already implied by the card, the harness
/// **conversation id** and the herdr **session name** are carried by the
/// detail's status fields (never in the same slot as each other — the confusion
/// `run.focus`'s separate `session` / `session_id` fields exist to prevent),
/// and a `pane ✓|-` marker would now be actively misleading: a run whose pane
/// is gone is reopened by resuming its conversation, so a missing pane no longer
/// predicts whether `o` works.
fn run_row_text(app: &App, run: &board_core::model::Run) -> String {
    let outcome = run.outcome.map(|o| o.as_str()).unwrap_or("active");
    format!(
        "#{} {} · {} · {}",
        run.id,
        run.harness,
        outcome,
        run_duration(app, run)
    )
}

/// Formatting only: `run_elapsed` owns "open runs measure against `now`,
/// closed runs against their own end, out-of-order clamps to 0", and a run
/// that never started has no duration to show.
fn run_duration(app: &App, run: &board_core::model::Run) -> String {
    let started = run.started_at.as_deref().and_then(parse_timestamp);
    let ended = run.ended_at.as_deref().and_then(parse_timestamp);
    match run_elapsed(started, ended, app.now) {
        Some(secs) => format_duration(Some(secs)),
        None => "-".to_string(),
    }
}
