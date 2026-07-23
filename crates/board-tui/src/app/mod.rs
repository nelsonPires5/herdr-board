//! Application state machine: `App` state, `Screen`, synthetic `Msg`s, and the
//! pure `update(&mut App, Msg) -> Vec<Effect>` reducer. Rendering lives in `view`;
//! I/O (client calls, `$EDITOR`) lives in `lib` via the returned [`Effect`]s.
//!
//! Keeping `update` free of I/O is what lets tests drive synthetic key/mouse
//! events and assert on state (navigation, form cycling, drag transitions) and on
//! rendered snapshots deterministically.

use board_core::protocol::{BoardSnapshot, CardDetail, CardMoveParams, CardStatus, RunOutcome};
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::layout::Rect;

use crate::forms::Form;
use crate::OriginContext;

mod board;
mod confirm;
mod detail;
mod forms;
mod mouse;
mod picker;

/// Which modal/screen is active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Board,
    CardDetail,
    CardForm,
    ColumnForm,
    Picker,
    Confirm,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DetailScrollTarget {
    Comments,
    Runs,
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
    SwitchBoard(i64),
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
    TemplateApply(String),
    RunCancel(i64),
    RunRetry(i64),
    RunDone(i64, RunOutcome),
    FocusRun(i64),
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
    MoveCard { card_id: i64 },
    DeleteColumnMoveTo { column_id: i64 },
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
    pub form: Option<Form>,
    /// Forms opened from card detail return there on save/cancel.
    pub form_from_detail: bool,
    pub picker: Option<Picker>,
    pub confirm: Option<Confirm>,
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
            form: None,
            form_from_detail: false,
            picker: None,
            confirm: None,
            drag: None,
            toast: None,
            should_quit: false,
            origin_context,
            now: 0,
            now_ms: 0,
            last_area: Rect::new(0, 0, 80, 24),
            last_click: None,
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
        self.form = None;
        self.form_from_detail = false;
        self.picker = None;
        self.confirm = None;
        self.drag = None;
    }

    // -- board queries -------------------------------------------------------

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
        let comments_total = detail.comments.len();
        let runs_total = detail.runs.len();
        let layout = crate::view::detail_layout(self, self.last_area);
        let comments_visible = layout.comments.height.saturating_sub(1) as usize;
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
        let (offset, total, visible) = match self.detail_scroll_target {
            DetailScrollTarget::Comments => (
                &mut self.detail_comments_scroll,
                detail.comments.len(),
                layout.comments.height.saturating_sub(1) as usize,
            ),
            DetailScrollTarget::Runs => (
                &mut self.detail_runs_scroll,
                detail.runs.len(),
                layout.runs.height.saturating_sub(1) as usize,
            ),
        };
        let max = total.saturating_sub(visible.max(1));
        *offset = (*offset as isize + delta).clamp(0, max as isize) as usize;
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
        Screen::Confirm => confirm::confirm_key(app, k),
        Screen::Help => {
            app.screen = Screen::Board;
            vec![]
        }
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
