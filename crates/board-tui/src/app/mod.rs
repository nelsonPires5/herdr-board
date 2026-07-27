//! Application state machine: `App` state, `Screen`, synthetic `Msg`s, and the
//! pure `update(&mut App, Msg) -> Vec<Effect>` reducer. Rendering lives in `view`;
//! I/O (client calls, `$EDITOR`) lives in `lib` via the returned [`Effect`]s.
//!
//! Keeping `update` free of I/O is what lets tests drive synthetic key/mouse
//! events and assert on state (navigation, form cycling, drag transitions) and on
//! rendered snapshots deterministically.

use std::cell::RefCell;
use std::collections::HashMap;

use board_core::model::CommentHistory;
use board_core::protocol::{BoardSnapshot, CardDetail, CardMoveParams, CardStatus, RunOutcome};
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::layout::Rect;

use crate::forms::Form;
use crate::widgets::HitMap;
use crate::OriginContext;

mod board;
mod comment_history;
mod confirm;
mod detail;
mod forms;
mod help;
mod mouse;
mod move_column;
mod picker;
mod switcher;

pub use switcher::enter_boards_level;

/// The only template that exists today. Single source of truth so the board
/// `T` key and the switcher's "Apply template" row can't drift apart.
pub const PIPELINE_TEMPLATE: &str = "pipeline";

/// Shared gate for applying [`PIPELINE_TEMPLATE`]: only onto an empty board
/// (`App::is_empty_board`), otherwise raises the same explanatory toast
/// everywhere it's invoked from (board `T` key, switcher "Apply template"
/// row) instead of silently doing nothing.
pub(super) fn apply_template(app: &mut App) -> Vec<Effect> {
    if app.is_empty_board() {
        return vec![Effect::TemplateApply(PIPELINE_TEMPLATE.into())];
    }
    app.set_toast("template only applies to an empty board", true);
    vec![]
}

/// Which modal/screen is active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Board,
    CardDetail,
    CardForm,
    ColumnForm,
    Picker,
    /// `M` mini-mode: ←/→ reorder the focused column, Enter commits, Esc cancels.
    MoveColumn,
    Confirm,
    Help,
    /// Compact-only column/board switcher sheet.
    Switcher,
    /// A focused comment's audit trail (`comment.history`), reached via `h`
    /// from `CardDetail`.
    CommentHistory,
}

/// Which level the Compact switcher sheet is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SwitcherLevel {
    /// The current board's columns, plus a trailing "switch board" row.
    Columns,
    /// The list of boards (reached via the trailing row).
    Boards,
}

/// State for the Compact-only two-level switcher sheet (`Screen::Switcher`).
pub struct SwitcherState {
    pub level: SwitcherLevel,
    pub sel: usize,
    /// The Columns-level selection to restore when backing out of `Boards`
    /// via `Esc`, rather than resetting to the top row. Only meaningful when
    /// `entered_at_boards` is `false`.
    pub columns_sel: usize,
    /// Whether this sheet was opened directly at `Boards` (`b`, which means
    /// "switch board") rather than drilled into from `Columns` (the header's
    /// center-button tap). Determines what `Esc` does at the `Boards` level:
    /// `true` closes the sheet outright, `false` steps back to `Columns` and
    /// restores `columns_sel`. Never used at the `Columns` level.
    pub entered_at_boards: bool,
    /// Populated on transition to `Boards`; `(label, board_id)`.
    pub boards: Vec<(String, i64)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DetailScrollTarget {
    Comments,
    Runs,
}

/// State for the `Screen::CommentHistory` sheet: one comment's full audit
/// trail, oldest → newest, with its own scroll offset.
pub struct CommentHistoryView {
    pub comment_id: i64,
    pub entries: Vec<CommentHistory>,
    pub scroll: usize,
}

/// The single definition of "is this a system comment" (author `"system"`),
/// shared by key handling (`e`/`d` immutability) and rendering (dimmed
/// `[Edit]`/`[Del]` labels) so the `"system"` literal lives in exactly one
/// place.
pub fn is_system_comment(c: &board_core::model::Comment) -> bool {
    c.author == "system"
}

/// Which cards are visible on the board. Archiving never deletes history.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardFilter {
    Active,
    All,
    Archived,
}

impl CardFilter {
    pub fn next(self) -> Self {
        match self {
            Self::Active => Self::All,
            Self::All => Self::Archived,
            Self::Archived => Self::Active,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "ACTIVE",
            Self::All => "ALL",
            Self::Archived => "ARCHIVED",
        }
    }
}

/// A synthetic event fed to [`update`].
pub enum Msg {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// A `board_changed` (or fallback) notification: refetch the board.
    Refresh,
}

/// A side effect for the driver to perform (client I/O, editor, quit).
pub enum Effect {
    Refetch,
    LoadBoards,
    /// Compact-only switcher, level 2: fetch the board list into `app.switcher`
    /// instead of the Regular/Wide `Picker`.
    LoadBoardsForSwitcher,
    SwitchBoard(i64),
    /// Cross-board move, stage 1: open the destination-board picker.
    LoadBoardsForMove {
        card_id: i64,
    },
    /// Cross-board move, stage 2: load the selected destination board's columns
    /// into the picker.
    LoadColumnsForMove {
        card_id: i64,
        board_id: i64,
    },
    LoadDetail(i64),
    CardCreate(board_core::protocol::CardCreateParams),
    CardUpdate(board_core::protocol::CardUpdateParams),
    CardDelete(i64),
    CardArchive {
        id: i64,
        archived: bool,
    },
    CardMove(CardMoveParams),
    ColumnCreate(board_core::protocol::ColumnCreateParams),
    ColumnUpdate(board_core::protocol::ColumnUpdateParams),
    ColumnReorder {
        id: i64,
        position: i64,
    },
    ColumnDelete {
        id: i64,
        move_cards_to: Option<i64>,
    },
    CommentAdd {
        card_id: i64,
        body: String,
    },
    CommentUpdate {
        id: i64,
        body: String,
    },
    CommentDelete {
        id: i64,
    },
    /// Fetch a comment's full audit trail (`comment.history`) and open
    /// `Screen::CommentHistory` with it.
    LoadCommentHistory {
        id: i64,
    },
    TemplateApply(String),
    RunCancel(i64),
    RunRetry(i64),
    RunDone(i64, RunOutcome),
    /// Focus one exact run's pane: `(card_id, run_id)`. The run is chosen by
    /// the TUI (`run.focus` never picks one implicitly).
    FocusRun(i64, i64),
    /// Hand the focused multiline text field to `$EDITOR`.
    EditFocusedTextArea,
    /// Fetch `harness.capabilities` + `session.list` + `space.list` for the open
    /// card form and populate its guided selectors. Emitted on form open and on
    /// harness/session change (the latter re-scopes the workspace list).
    LoadFormOptions,
    /// Keep the Herdr pane border title in sync with the archive filter.
    SetPaneTitle(CardFilter),
    Quit,
}

/// A transient status message.
pub struct Toast {
    pub text: String,
    pub is_error: bool,
    /// Wall-clock second at which it was raised (for expiry in the run loop).
    pub at: i64,
}

/// A column picker (move card / choose where a deleted column's cards go).
pub struct Picker {
    pub title: String,
    pub options: Vec<(String, i64)>,
    pub sel: usize,
    pub purpose: PickerPurpose,
}

#[derive(Clone, Copy)]
pub enum PickerPurpose {
    SwitchBoard,
    /// Cross-board move: choosing the destination board (stage 1).
    MoveCardPickBoard {
        card_id: i64,
    },
    /// Cross-board move: choosing a column of `board_id` (stage 2).
    MoveCardPickColumn {
        card_id: i64,
        board_id: i64,
    },
    DeleteColumnMoveTo {
        column_id: i64,
    },
}

/// A yes/no confirmation.
pub struct Confirm {
    pub message: String,
    pub purpose: ConfirmPurpose,
}

#[derive(Clone, Copy)]
pub enum ConfirmPurpose {
    DeleteCard(i64),
    DeleteColumn(i64),
    CancelRun(i64),
    DeleteComment(i64),
}

/// In-progress "move column" mini-mode state (entered with `M`).
pub struct MoveColumnState {
    pub column_id: i64,
    pub original_index: usize,
}

/// Mouse drag in progress.
pub struct DragState {
    pub kind: DragKind,
    pub from_col: usize,
    pub hover_col: usize,
}

#[derive(Clone, Copy)]
pub enum DragKind {
    Card { card_id: i64 },
    Column { column_id: i64 },
}

/// The whole TUI state.
pub struct App {
    pub board: BoardSnapshot,
    pub screen: Screen,
    pub sel_col: usize,
    pub sel_card: usize,
    pub card_filter: CardFilter,
    pub detail: Option<CardDetail>,
    /// Card detail opens as a contextual popup; users can expand it in place.
    pub detail_fullscreen: bool,
    pub detail_scroll_target: DetailScrollTarget,
    pub detail_comments_scroll: usize,
    pub detail_runs_scroll: usize,
    /// Index into `detail.comments` of the focused comment (edit/delete/
    /// history act on it). Only meaningful while `detail_scroll_target ==
    /// Comments` and `detail.comments` is non-empty — see `focused_comment`.
    pub detail_comment_sel: usize,
    /// Index into `detail.runs` of the selected run — the run `o` jumps to.
    /// This is the cursor; `detail_runs_scroll` is only the viewport offset
    /// that follows it (`follow_run_focus`). Unlike `detail_comment_sel` it is
    /// *not* gated on `detail_scroll_target`: `o` works from either section, so
    /// the selection stays meaningful while comments have key focus — see
    /// `focused_run`.
    pub detail_run_sel: usize,
    /// The comment-history sheet's state (`Screen::CommentHistory`); `None`
    /// when not open.
    pub comment_history: Option<CommentHistoryView>,
    pub form: Option<Form>,
    /// Forms opened from card detail return there on save/cancel.
    pub form_from_detail: bool,
    pub picker: Option<Picker>,
    pub confirm: Option<Confirm>,
    pub move_column: Option<MoveColumnState>,
    pub drag: Option<DragState>,
    pub toast: Option<Toast>,
    pub should_quit: bool,
    /// Explicit invoking Herdr/plugin context; default in tests.
    pub origin_context: OriginContext,
    /// Injected clock (epoch seconds) for deterministic timer rendering.
    pub now: i64,
    /// Injected millisecond clock for double-click detection (0 in tests).
    pub now_ms: u128,
    /// Last full draw area, for mouse hit-testing.
    pub last_area: Rect,
    last_click: Option<(u16, u16, u128)>,
    /// Per-column vertical card-scroll offset, keyed by column id.
    pub col_scroll: HashMap<i64, usize>,
    /// Compact-only column/board switcher sheet state.
    pub switcher: Option<SwitcherState>,
    /// Vertical scroll offset (in wrapped rows) of the Compact single-column
    /// help sheet; unused in Regular/Wide (fixed two-column layout).
    pub help_scroll: usize,
    /// Rects registered by the last `view()` call, for the new Compact-mode
    /// widgets (header buttons, switcher rows, button bars). Cleared at the
    /// start of every draw.
    pub hit_map: RefCell<HitMap>,
}

impl App {
    pub fn new(board: BoardSnapshot) -> App {
        Self::with_origin_context(board, OriginContext::default())
    }

    pub fn with_origin_context(board: BoardSnapshot, origin_context: OriginContext) -> App {
        App {
            board,
            screen: Screen::Board,
            sel_col: 0,
            sel_card: 0,
            card_filter: CardFilter::Active,
            detail: None,
            detail_fullscreen: false,
            detail_scroll_target: DetailScrollTarget::Comments,
            detail_comments_scroll: 0,
            detail_runs_scroll: 0,
            detail_comment_sel: 0,
            detail_run_sel: 0,
            comment_history: None,
            form: None,
            form_from_detail: false,
            picker: None,
            confirm: None,
            move_column: None,
            drag: None,
            toast: None,
            should_quit: false,
            origin_context,
            now: 0,
            now_ms: 0,
            last_area: Rect::new(0, 0, 80, 24),
            last_click: None,
            col_scroll: HashMap::new(),
            switcher: None,
            help_scroll: 0,
            hit_map: RefCell::new(HitMap::default()),
        }
    }

    pub fn replace_board(&mut self, board: BoardSnapshot) {
        self.board = board;
        self.screen = Screen::Board;
        self.sel_col = 0;
        self.sel_card = 0;
        self.detail = None;
        self.detail_fullscreen = false;
        self.detail_comments_scroll = 0;
        self.detail_runs_scroll = 0;
        self.detail_comment_sel = 0;
        self.detail_run_sel = 0;
        self.comment_history = None;
        self.form = None;
        self.form_from_detail = false;
        self.picker = None;
        self.confirm = None;
        self.move_column = None;
        self.drag = None;
        self.switcher = None;
        self.col_scroll.clear();
    }

    // -- board queries -------------------------------------------------------

    pub fn layout_mode(&self) -> crate::view::LayoutMode {
        crate::view::LayoutMode::from_width(self.last_area.width)
    }

    pub fn col_id_at(&self, idx: usize) -> Option<i64> {
        self.board.columns.get(idx).map(|c| c.id)
    }

    /// Find the live-run summary for a card in the current board snapshot.
    pub fn active_run_for_card(
        &self,
        card_id: i64,
    ) -> Option<&board_core::protocol::ActiveRunSummary> {
        self.board
            .active_runs
            .iter()
            .find(|run| run.card_id == card_id)
    }

    /// Cards of a column, in board order.
    pub fn cards_of(&self, col_id: i64) -> Vec<&board_core::model::Card> {
        self.board
            .cards
            .iter()
            .filter(|c| c.column_id == col_id)
            .filter(|c| match self.card_filter {
                CardFilter::Active => c.archived_at.is_none(),
                CardFilter::All => true,
                CardFilter::Archived => c.archived_at.is_some(),
            })
            .collect()
    }

    pub fn selected_card_id(&self) -> Option<i64> {
        let col_id = self.col_id_at(self.sel_col)?;
        self.cards_of(col_id).get(self.sel_card).map(|c| c.id)
    }

    pub fn selected_card(&self) -> Option<&board_core::model::Card> {
        let col_id = self.col_id_at(self.sel_col)?;
        self.cards_of(col_id).get(self.sel_card).copied()
    }

    pub fn selected_card_status(&self) -> Option<CardStatus> {
        self.selected_card().map(|c| c.status)
    }

    /// A pristine board that a template could be applied onto.
    pub fn is_empty_board(&self) -> bool {
        self.board.cards.is_empty() && self.board.columns.len() == 1
    }

    pub fn set_toast(&mut self, text: impl Into<String>, is_error: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            is_error,
            at: self.now,
        });
    }

    /// Keep chronological order (oldest → newest) and open both histories at
    /// their bottom so the most recent item is always the last visible row.
    pub fn scroll_detail_to_latest(&mut self) {
        let Some(detail) = &self.detail else { return };
        let layout = crate::view::detail_layout(self, self.last_area);
        // Comments word-wrap, so their scroll is row-based against the summed
        // wrapped height; runs stay one row each.
        let comments_total = crate::view::comment_wrapped_rows(detail, layout.comments.width);
        let runs_total = detail.runs.len();
        let (_, comments_visible) = crate::view::comments_viewport(self, &layout);
        let runs_visible = layout.runs.height.saturating_sub(1) as usize;
        self.detail_comments_scroll = comments_total.saturating_sub(comments_visible.max(1));
        self.detail_runs_scroll = runs_total.saturating_sub(runs_visible.max(1));
    }

    fn toggle_detail_fullscreen(&mut self) {
        self.detail_fullscreen = !self.detail_fullscreen;
        self.scroll_detail_to_latest();
    }

    fn scroll_detail(&mut self, delta: isize) {
        let Some(detail) = &self.detail else { return };
        let layout = crate::view::detail_layout(self, self.last_area);
        let (_, comments_visible) = crate::view::comments_viewport(self, &layout);
        let comments_total = crate::view::comment_wrapped_rows(detail, layout.comments.width);
        let runs_total = detail.runs.len();
        let runs_visible = layout.runs.height.saturating_sub(1) as usize;
        let (offset, total, visible) = match self.detail_scroll_target {
            // Row-based: comments wrap, so total/visible are wrapped rows.
            DetailScrollTarget::Comments => (
                &mut self.detail_comments_scroll,
                comments_total,
                comments_visible,
            ),
            DetailScrollTarget::Runs => (&mut self.detail_runs_scroll, runs_total, runs_visible),
        };
        let max = total.saturating_sub(visible.max(1));
        *offset = (*offset as isize + delta).clamp(0, max as isize) as usize;
    }

    /// Whether the focused comment can be edited/deleted: system comments are
    /// immutable (`Db::update_comment`/`soft_delete_comment` reject
    /// `author == "system"`; see `docs/protocol.md`), so `e`/`d` and the
    /// action bar's `[Edit]`/`[Del]` labels must treat it as read-only.
    /// `comment.history` is unaffected — history stays available regardless.
    pub fn focused_comment_is_system(&self) -> bool {
        self.focused_comment().is_some_and(is_system_comment)
    }

    /// The focused comment (edit/delete/history act on it): `Some` only while
    /// the comments section is focused and non-empty.
    pub fn focused_comment(&self) -> Option<&board_core::model::Comment> {
        if self.detail_scroll_target != DetailScrollTarget::Comments {
            return None;
        }
        let detail = self.detail.as_ref()?;
        if detail.comments.is_empty() {
            return None;
        }
        let idx = self.detail_comment_sel.min(detail.comments.len() - 1);
        detail.comments.get(idx)
    }

    /// After `detail_comment_sel` changes, keep it in range and scroll the
    /// comments viewport just enough to keep the focused comment's wrapped
    /// row span fully visible (when it fits).
    fn follow_comment_focus(&mut self) {
        let len = match &self.detail {
            Some(d) if !d.comments.is_empty() => d.comments.len(),
            _ => return,
        };
        self.detail_comment_sel = self.detail_comment_sel.min(len - 1);
        let layout = crate::view::detail_layout(self, self.last_area);
        let spans =
            crate::view::comment_row_spans(self.detail.as_ref().unwrap(), layout.comments.width);
        let (_, visible) = crate::view::comments_viewport(self, &layout);
        let Some(&(start, span_len)) = spans.get(self.detail_comment_sel) else {
            return;
        };
        let mut scroll = self.detail_comments_scroll;
        if start < scroll {
            scroll = start;
        }
        if start + span_len > scroll + visible {
            scroll = (start + span_len).saturating_sub(visible);
        }
        if scroll > start {
            scroll = start;
        }
        self.detail_comments_scroll = scroll;
    }

    /// The selected run — what `o` jumps to. `Some` whenever a detail with at
    /// least one run is open, regardless of which section has key focus (`o`
    /// has no competing binding, so it must keep working from the comments
    /// section too). The runs list only *highlights* the row while the runs
    /// section is focused, mirroring the comments list.
    pub fn focused_run(&self) -> Option<&board_core::model::Run> {
        let detail = self.detail.as_ref()?;
        if detail.runs.is_empty() {
            return None;
        }
        let idx = self.detail_run_sel.min(detail.runs.len() - 1);
        detail.runs.get(idx)
    }

    /// After `detail_run_sel` changes, keep it in range and scroll the runs
    /// viewport just enough to keep the selected row visible. Runs are exactly
    /// one row each, so this needs no wrapped-span arithmetic.
    fn follow_run_focus(&mut self) {
        let len = match &self.detail {
            Some(d) if !d.runs.is_empty() => d.runs.len(),
            _ => return,
        };
        self.detail_run_sel = self.detail_run_sel.min(len - 1);
        let layout = crate::view::detail_layout(self, self.last_area);
        let visible = (layout.runs.height.saturating_sub(1) as usize).max(1);
        let sel = self.detail_run_sel;
        let mut scroll = self.detail_runs_scroll.min(len - 1);
        if sel < scroll {
            scroll = sel;
        }
        if sel >= scroll + visible {
            scroll = sel + 1 - visible;
        }
        self.detail_runs_scroll = scroll;
    }

    /// The mirror image of `follow_*_focus`, for a **raw** scroll (the mouse
    /// wheel) that moves the offset without moving the cursor: pull the cursor
    /// into the rows the wheel just brought into view. The wheel keeps its
    /// natural "scroll" meaning while the `▸` marker stays on screen, so the
    /// keys that act on the focused row — `o` on the selected run, `e`/`d`/`h`
    /// on the focused comment — can never target a row the user cannot see.
    /// Both detail sections behave identically here.
    fn follow_detail_scroll(&mut self) {
        let layout = crate::view::detail_layout(self, self.last_area);
        match self.detail_scroll_target {
            DetailScrollTarget::Comments => {
                let Some(detail) = self.detail.as_ref() else {
                    return;
                };
                if detail.comments.is_empty() {
                    return;
                }
                // Comments wrap, so "visible" is a row window that a comment's
                // span must intersect — not a comment index range.
                let spans = crate::view::comment_row_spans(detail, layout.comments.width);
                let (_, visible) = crate::view::comments_viewport(self, &layout);
                let lo = self.detail_comments_scroll;
                let hi = lo + visible.max(1);
                let visible_idx: Vec<usize> = spans
                    .iter()
                    .enumerate()
                    .filter(|(_, &(start, len))| start < hi && start + len > lo)
                    .map(|(i, _)| i)
                    .collect();
                let (Some(&first), Some(&last)) = (visible_idx.first(), visible_idx.last()) else {
                    return;
                };
                self.detail_comment_sel = self
                    .detail_comment_sel
                    .min(spans.len() - 1)
                    .clamp(first, last);
            }
            DetailScrollTarget::Runs => {
                let len = match &self.detail {
                    Some(d) if !d.runs.is_empty() => d.runs.len(),
                    _ => return,
                };
                let visible = (layout.runs.height.saturating_sub(1) as usize).max(1);
                let first = self.detail_runs_scroll.min(len - 1);
                let last = (first + visible - 1).min(len - 1);
                self.detail_run_sel = self.detail_run_sel.min(len - 1).clamp(first, last);
            }
        }
    }

    // -- navigation ----------------------------------------------------------

    fn clamp_card(&mut self) {
        let len = self
            .col_id_at(self.sel_col)
            .map(|id| self.cards_of(id).len())
            .unwrap_or(0);
        if len == 0 {
            self.sel_card = 0;
        } else if self.sel_card >= len {
            self.sel_card = len - 1;
        }
    }

    fn move_col(&mut self, delta: isize) {
        let n = self.board.columns.len();
        if n == 0 {
            return;
        }
        self.sel_col = (self.sel_col as isize + delta).rem_euclid(n as isize) as usize;
        self.clamp_card();
    }

    fn move_card(&mut self, delta: isize) {
        let len = self
            .col_id_at(self.sel_col)
            .map(|id| self.cards_of(id).len())
            .unwrap_or(0);
        if len == 0 {
            return;
        }
        self.sel_card = (self.sel_card as isize + delta).rem_euclid(len as isize) as usize;
    }

    // -- drag helpers (also exercised directly by unit tests) ----------------

    pub fn begin_card_drag(&mut self, card_id: i64, from_col: usize) {
        self.drag = Some(DragState {
            kind: DragKind::Card { card_id },
            from_col,
            hover_col: from_col,
        });
    }

    pub fn begin_column_drag(&mut self, column_id: i64, from_col: usize) {
        self.drag = Some(DragState {
            kind: DragKind::Column { column_id },
            from_col,
            hover_col: from_col,
        });
    }

    pub fn drag_hover(&mut self, col: usize) {
        if let Some(d) = &mut self.drag {
            d.hover_col = col;
        }
    }

    /// Complete a drag, producing a move/reorder effect when it landed elsewhere.
    pub fn finish_drag(&mut self) -> Vec<Effect> {
        let Some(d) = self.drag.take() else {
            return vec![];
        };
        if d.hover_col == d.from_col {
            return vec![];
        }
        match d.kind {
            DragKind::Card { card_id } => match self.col_id_at(d.hover_col) {
                Some(column_id) => vec![Effect::CardMove(CardMoveParams {
                    id: card_id,
                    column_id,
                    board_id: None,
                    position: None,
                })],
                None => vec![],
            },
            DragKind::Column { column_id } => vec![Effect::ColumnReorder {
                id: column_id,
                position: d.hover_col as i64,
            }],
        }
    }
}

/// The pure reducer. Mutates `app` and returns effects for the driver.
pub fn update(app: &mut App, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Refresh => vec![Effect::Refetch],
        Msg::Key(k) => on_key(app, k),
        Msg::Mouse(m) => mouse::on_mouse(app, m),
    }
}

fn on_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    match app.screen {
        Screen::Board => board::board_key(app, k),
        Screen::CardDetail => detail::detail_key(app, k),
        Screen::CardForm | Screen::ColumnForm => forms::form_key(app, k),
        Screen::Picker => picker::picker_key(app, k),
        Screen::MoveColumn => move_column::move_column_key(app, k),
        Screen::Confirm => confirm::confirm_key(app, k),
        Screen::Help => help::help_key(app, k),
        Screen::Switcher => switcher::switcher_key(app, k),
        Screen::CommentHistory => comment_history::comment_history_key(app, k),
    }
}

/// Post-mutation helper: after the board is refetched the selection may point
/// past the end of a shrunk column; clamp it. Also used by the driver.
pub fn clamp_selection(app: &mut App) {
    if app.sel_col >= app.board.columns.len() {
        app.sel_col = app.board.columns.len().saturating_sub(1);
    }
    app.clamp_card();
}
