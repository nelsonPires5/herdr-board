//! Application state machine: `App` state, `Screen`, synthetic `Msg`s, and the
//! pure `update(&mut App, Msg) -> Vec<Effect>` reducer. Rendering lives in `view`;
//! I/O (client calls, `$EDITOR`) lives in `driver` via the returned [`Effect`]s.
//!
//! Keeping `update` free of I/O is what lets tests drive synthetic key/mouse
//! events and assert on state (navigation, form cycling, drag transitions) and on
//! rendered snapshots deterministically.
//!
//! This module holds only the core machine — `Screen`, `App` and its board
//! queries, `update`/`on_key`. Everything around it is a sibling module:
//!
//! | module | holds |
//! |---|---|
//! | [`state`] | the modal/mini-mode state types `App` owns |
//! | [`effect`] | the [`Effect`] alphabet `update` emits |
//! | [`nav`] | the shared `↑/↓` decoder, clamped stepping, board selection |
//! | [`drag`] | the mouse-drag lifecycle |
//! | `board`/`detail`/`forms`/`picker`/`confirm`/`help`/`switcher`/`move_column`/`comment_history` | one screen's key handler each (plus, for `detail`, its viewport arithmetic) |
//! | `mouse` | all mouse input, for every screen |

use std::cell::RefCell;
use std::collections::HashMap;

use board_core::engine::{validate_card_archive, ValidationError};
use board_core::protocol::{BoardSnapshot, CardDetail, CardStatus, Visibility};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::layout::Rect;

use crate::forms::Form;
use crate::widgets::HitMap;
use crate::OriginContext;

mod board;
mod comment_history;
mod confirm;
mod detail;
mod drag;
mod effect;
mod forms;
mod help;
mod mouse;
mod move_column;
mod nav;
mod picker;
mod reorder_card;
mod state;
mod switcher;

pub use effect::Effect;
pub use nav::clamp_selection;
pub use state::{
    CardFilter, CommentHistoryView, Confirm, ConfirmPurpose, DetailScrollTarget, DragKind,
    DragState, MoveColumnState, Picker, PickerAction, PickerPurpose, PickerRow, ReorderCardState,
    SwitcherState, Toast,
};

pub(crate) use state::column_options;

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
///
/// Every modal ([`Picker`], [`Confirm`], [`Form`], [`SwitcherState`]) and the
/// help sheet ([`App::help_return_to`]) records the `Screen` it was opened
/// from in a `return_to` field, set once at open time. Dismissing a modal
/// restores that screen verbatim. This is deliberately *not* re-derived from
/// what the modal is for: `?` from card detail must come back to card detail
/// whatever the reason help was opened, and two callers opening the same modal
/// from different screens must each get their own way back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Board,
    CardDetail,
    CardForm,
    ColumnForm,
    Picker,
    /// `M` mini-mode: ←/→ reorder the focused column, Enter commits, Esc cancels.
    MoveColumn,
    /// `O` mini-mode: j/k reorder the selected card within its column, Enter commits, Esc cancels.
    ReorderCard,
    Confirm,
    Help,
    /// Compact-only column switcher sheet.
    Switcher,
    /// The project picker (`p`): switch project / create one.
    ProjectPicker,
    /// The board picker (`b` / a project picker choice): switch board / create one.
    BoardPicker,
    /// A focused comment's audit trail (`comment.history`), reached via `h`
    /// from `CardDetail`.
    CommentHistory,
}

/// The single archive/restore gate, shared by the board `a` key and the card
/// detail `a` key so the rule and its wording cannot drift between them.
///
/// Restoring is always allowed; archiving defers to
/// [`validate_card_archive`] — the *same* predicate and message the daemon
/// enforces, so the TUI never invents a second copy of the rule.
pub(super) fn archive_card(card: &board_core::model::Card) -> Result<Effect, ValidationError> {
    let archived = card.archived_at.is_none();
    if archived {
        validate_card_archive(card.status)?;
    }
    Ok(Effect::CardArchive {
        id: card.id,
        archived,
    })
}

/// A synthetic event fed to [`update`].
pub enum Msg {
    Key(KeyEvent),
    Mouse(MouseEvent),
    /// A `board_changed` (or fallback) notification: refetch the board.
    Refresh,
}

/// The whole TUI state.
pub struct App {
    pub board: BoardSnapshot,
    /// The project the current board belongs to. Kept in sync from
    /// `project.list` (see `Driver::refresh_projects`) and used by the
    /// project picker and the header's project chip.
    pub project: board_core::model::Project,
    /// Project cache from `project.list`: per-project boards plus the
    /// selection/recency data the pickers need. `projects_loaded` records
    /// whether a fetch has ever landed (a failed first fetch keeps the picker
    /// from showing an empty list as if it were authoritative).
    pub projects: Vec<board_core::protocol::ProjectInfo>,
    pub projects_loaded: bool,
    pub screen: Screen,
    pub sel_col: usize,
    pub sel_card: usize,
    pub card_filter: CardFilter,
    pub picker_visibility: Visibility,
    pub detail: Option<CardDetail>,
    /// Card detail opens as a contextual popup; users can expand it in place.
    pub detail_fullscreen: bool,
    /// Whether the card/column form fills the screen (toggle via `f`).
    pub form_fullscreen: bool,
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
    pub picker: Option<Picker>,
    pub confirm: Option<Confirm>,
    pub move_column: Option<MoveColumnState>,
    pub reorder_card: Option<ReorderCardState>,
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
    /// Vertical scroll offset of the `?` help sheet: wrapped rows in the
    /// Compact single-column list, whole rows per column in Regular/Wide.
    /// Reset every time help is opened.
    pub help_scroll: usize,
    /// Where closing the `?` help sheet lands: the screen it was opened from.
    /// See [`Screen`]'s `return_to` note.
    pub help_return_to: Screen,
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
        let project = board_core::model::Project {
            id: board.board.project_id,
            name: board_core::model::Project::display_name(board.board.scope_path.as_deref()),
            scope_path: board.board.scope_path.clone(),
            archived_at: board.board.archived_at.clone(),
        };
        App {
            board,
            project,
            projects: Vec::new(),
            projects_loaded: false,
            screen: Screen::Board,
            sel_col: 0,
            sel_card: 0,
            card_filter: CardFilter::Active,
            picker_visibility: Visibility::Active,
            detail: None,
            detail_fullscreen: false,
            form_fullscreen: false,
            detail_scroll_target: DetailScrollTarget::Comments,
            detail_comments_scroll: 0,
            detail_runs_scroll: 0,
            detail_comment_sel: 0,
            detail_run_sel: 0,
            comment_history: None,
            form: None,
            picker: None,
            confirm: None,
            move_column: None,
            reorder_card: None,
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
            help_return_to: Screen::Board,
            hit_map: RefCell::new(HitMap::default()),
        }
    }

    pub fn replace_board(&mut self, board: BoardSnapshot) {
        // Re-derive the project from the cache when the board's project is
        // known there; otherwise fall back to deriving it from the board's
        // own scope path (the pre-cache bootstrap).
        match self
            .projects
            .iter()
            .find(|pi| pi.project.id == board.board.project_id)
        {
            Some(pi) => self.project = pi.project.clone(),
            None => {
                self.project = board_core::model::Project {
                    id: board.board.project_id,
                    name: board_core::model::Project::display_name(
                        board.board.scope_path.as_deref(),
                    ),
                    scope_path: board.board.scope_path.clone(),
                    archived_at: board.board.archived_at.clone(),
                };
            }
        }
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
        self.picker = None;
        self.confirm = None;
        self.move_column = None;
        self.reorder_card = None;
        self.drag = None;
        self.switcher = None;
        self.help_return_to = Screen::Board;
        self.col_scroll.clear();
    }

    // -- board queries -------------------------------------------------------

    pub fn layout_mode(&self) -> crate::view::LayoutMode {
        crate::view::LayoutMode::from_width(self.last_area.width)
    }

    pub fn col_id_at(&self, idx: usize) -> Option<i64> {
        self.display_column(idx).map(|c| c.id)
    }

    /// The column shown at display position `idx`.
    ///
    /// Display order is the authoritative snapshot order with the in-progress
    /// `M` move-column staging applied as a pure permutation. Every
    /// index-based read (selection, layout, rendering) goes through here, so
    /// the staged order is visible everywhere without the snapshot ever being
    /// mutated.
    pub fn display_column(&self, idx: usize) -> Option<&board_core::model::Column> {
        self.board.columns.get(self.snapshot_index(idx)?)
    }

    /// Map a display index onto an index into `board.columns`.
    fn snapshot_index(&self, idx: usize) -> Option<usize> {
        let n = self.board.columns.len();
        if idx >= n {
            return None;
        }
        let Some(state) = &self.move_column else {
            return Some(idx);
        };
        // A refresh may have removed the column being moved; then there is no
        // permutation left to apply and the snapshot order stands.
        let Some(from) = self
            .board
            .columns
            .iter()
            .position(|c| c.id == state.column_id)
        else {
            return Some(idx);
        };
        let to = state.staged_index.min(n - 1);
        // `remove(from)` then `insert(to)` rotates exactly the span between
        // them; everything outside it keeps its index.
        Some(if idx == to {
            from
        } else if from < to && (from..to).contains(&idx) {
            idx + 1
        } else if from > to && (to < idx && idx <= from) {
            idx - 1
        } else {
            idx
        })
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
    ///
    /// With the `O` reorder mode active on this column, the staged position is
    /// applied as a pure permutation at read time — `app.board` stays the
    /// daemon's read-only snapshot, so every index-based read (selection,
    /// rendering, hit-testing) sees the staged order and a mid-mode refresh
    /// cannot silently discard it.
    pub fn cards_of(&self, col_id: i64) -> Vec<&board_core::model::Card> {
        let mut cards: Vec<&board_core::model::Card> = self
            .board
            .cards
            .iter()
            .filter(|c| c.column_id == col_id)
            .filter(|c| match self.card_filter {
                CardFilter::Active => c.archived_at.is_none(),
                CardFilter::All => true,
                CardFilter::Archived => c.archived_at.is_some(),
            })
            .collect();
        if let Some(state) = &self.reorder_card {
            if state.column_id == col_id {
                if let Some(from) = cards.iter().position(|c| c.id == state.card_id) {
                    let card = cards.remove(from);
                    let to = state.staged_index.min(cards.len());
                    cards.insert(to, card);
                }
            }
        }
        cards
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
    // Chord guard: the forms are the only screens with modifier-key bindings
    // (`Ctrl+E`, `Ctrl+J`, `Shift+Enter`); every other binding is a bare key
    // matched on `KeyCode` alone, so a chorded press would fire the plain key
    // arm. Terminals deliver `Esc` followed quickly by another key as a
    // single `Alt+<key>` sequence, which made a stray `Esc M` read as `M` and
    // open "move column" unprompted. `Shift` stays allowed (it is the
    // natural modifier of the uppercase letters the board binds, and the
    // click path synthesizes `Char('M')` with `SHIFT`).
    if !matches!(app.screen, Screen::CardForm | Screen::ColumnForm)
        && k.modifiers.intersects(
            KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::META
                | KeyModifiers::SUPER
                | KeyModifiers::HYPER,
        )
    {
        return vec![];
    }
    // `?` is global (B4): every screen that is not swallowing text input can
    // reach help, and each one comes back to itself. The forms are excluded
    // because `?` is a literal character there, and Help itself is excluded
    // because every key already closes/scrolls it.
    if k.code == KeyCode::Char('?')
        && !matches!(
            app.screen,
            Screen::CardForm | Screen::ColumnForm | Screen::Help
        )
    {
        app.help_return_to = app.screen;
        app.help_scroll = 0;
        app.screen = Screen::Help;
        return vec![];
    }
    match app.screen {
        Screen::Board => board::board_key(app, k),
        Screen::CardDetail => detail::detail_key(app, k),
        Screen::CardForm | Screen::ColumnForm => forms::form_key(app, k),
        Screen::Picker | Screen::ProjectPicker | Screen::BoardPicker => picker::picker_key(app, k),
        Screen::MoveColumn => move_column::move_column_key(app, k),
        Screen::ReorderCard => reorder_card::reorder_card_key(app, k),
        Screen::Confirm => confirm::confirm_key(app, k),
        Screen::Help => help::help_key(app, k),
        Screen::Switcher => switcher::switcher_key(app, k),
        Screen::CommentHistory => comment_history::comment_history_key(app, k),
    }
}
