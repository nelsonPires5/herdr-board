//! Card detail (`Screen::CardDetail`): opening it, its key handling, and all
//! of the two-section (comments / runs) viewport arithmetic — cursor movement,
//! the follow-the-cursor scroll, and the mirror-image follow-the-scroll cursor
//! pull the mouse wheel needs.
//!
//! The geometry itself lives in `view` (`detail_layout`, `comment_row_spans`,
//! `comments_viewport`); this module only decides where the cursor and offsets
//! should land given that geometry, so it stays pure and testable.

use board_core::model::Comment;
use board_core::protocol::{CardStatus, RunOutcome};
use crossterm::event::{KeyCode, KeyEvent};

use crate::forms::Form;

use super::nav::{nav_delta, step_clamped};
use super::{App, Confirm, ConfirmPurpose, DetailScrollTarget, Effect, Screen};

impl App {
    /// Open the card detail popup for `id` from a closed state.
    ///
    /// One place for the seven-field reset the two openers (`Enter` on the
    /// board, a double-click on a card) both need, including the `usize::MAX`
    /// cursor sentinels that `Driver::load_detail` resolves against the
    /// freshly fetched comment/run counts — see its comment for why "not yet
    /// focused anywhere" cannot just be `0`.
    pub(super) fn open_detail(&mut self, id: i64) -> Vec<Effect> {
        self.detail_fullscreen = false;
        self.detail_scroll_target = DetailScrollTarget::Comments;
        self.detail_comments_scroll = 0;
        self.detail_runs_scroll = 0;
        self.detail_comment_sel = usize::MAX;
        self.detail_run_sel = usize::MAX;
        self.screen = Screen::CardDetail;
        vec![Effect::LoadDetail(id)]
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
        let runs_visible = crate::view::runs_viewport_height(&layout);
        // Keep one logical row as the latest anchor even when the frame has
        // only title/action/bottom rows. The renderer paints no body at a
        // zero-row viewport, and the anchor becomes useful immediately after
        // a resize reveals rows.
        self.detail_comments_scroll = comments_total.saturating_sub(comments_visible.max(1));
        self.detail_runs_scroll = runs_total.saturating_sub(runs_visible.max(1));
    }

    pub(super) fn toggle_detail_fullscreen(&mut self) {
        self.detail_fullscreen = !self.detail_fullscreen;
        self.scroll_detail_to_latest();
    }

    pub(super) fn scroll_detail(&mut self, delta: isize) {
        let Some(detail) = &self.detail else { return };
        let layout = crate::view::detail_layout(self, self.last_area);
        let (_, comments_visible) = crate::view::comments_viewport(self, &layout);
        let comments_total = crate::view::comment_wrapped_rows(detail, layout.comments.width);
        let runs_total = detail.runs.len();
        let runs_visible = crate::view::runs_viewport_height(&layout);
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
    /// action bar's `[ Edit ]`/`[ Delete ]` labels must treat it as read-only.
    /// `comment.history` is unaffected — history stays available regardless.
    pub fn focused_comment_is_system(&self) -> bool {
        self.focused_comment().is_some_and(Comment::is_system)
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
    pub(super) fn follow_comment_focus(&mut self) {
        let len = match &self.detail {
            Some(d) if !d.comments.is_empty() => d.comments.len(),
            _ => return,
        };
        self.detail_comment_sel = self.detail_comment_sel.min(len - 1);
        let layout = crate::view::detail_layout(self, self.last_area);
        let spans =
            crate::view::comment_row_spans(self.detail.as_ref().unwrap(), layout.comments.width);
        let (_, visible) = crate::view::comments_viewport(self, &layout);
        // A compact content region can retain the title/action/bottom rows
        // while leaving zero history rows. Do not move the scroll anchor to a
        // hidden comment in that case; the action rail remains usable and the
        // next resize can reveal the same latest anchor.
        if visible == 0 {
            return;
        }
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
        // Do not pull a valid bottom anchor back toward the comment's start:
        // the final comment may itself span more than one viewport row, and
        // the latest position is the only one that keeps its tail visible.
        let total = crate::view::comment_wrapped_rows(
            self.detail.as_ref().expect("detail exists"),
            layout.comments.width,
        );
        self.detail_comments_scroll = scroll.min(total.saturating_sub(visible));
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
    pub(super) fn follow_run_focus(&mut self) {
        let len = match &self.detail {
            Some(d) if !d.runs.is_empty() => d.runs.len(),
            _ => return,
        };
        self.detail_run_sel = self.detail_run_sel.min(len - 1);
        let layout = crate::view::detail_layout(self, self.last_area);
        let visible = crate::view::runs_viewport_height(&layout).max(1);
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
    pub(super) fn follow_detail_scroll(&mut self) {
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
                if visible == 0 {
                    return;
                }
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
                let visible = crate::view::runs_viewport_height(&layout).max(1);
                let first = self.detail_runs_scroll.min(len - 1);
                let last = (first + visible - 1).min(len - 1);
                self.detail_run_sel = self.detail_run_sel.min(len - 1).clamp(first, last);
            }
        }
    }
}

pub(super) fn detail_key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let card_id = app.detail.as_ref().map(|d| d.card.id);
    if let Some(delta) = nav_delta(k.code) {
        detail_nav(app, delta);
        return vec![];
    }
    match k.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.screen = Screen::Board;
            app.detail = None;
            app.detail_fullscreen = false;
            app.detail_comments_scroll = 0;
            app.detail_runs_scroll = 0;
            app.detail_comment_sel = 0;
            app.detail_run_sel = 0;
            app.comment_history = None;
        }
        KeyCode::Char('f') => app.toggle_detail_fullscreen(),
        KeyCode::Tab => {
            app.detail_scroll_target = match app.detail_scroll_target {
                DetailScrollTarget::Comments => DetailScrollTarget::Runs,
                DetailScrollTarget::Runs => DetailScrollTarget::Comments,
            };
            if let Some(detail) = &app.detail {
                if !detail.comments.is_empty() {
                    app.detail_comment_sel = app.detail_comment_sel.min(detail.comments.len() - 1);
                }
                if !detail.runs.is_empty() {
                    app.detail_run_sel = app.detail_run_sel.min(detail.runs.len() - 1);
                }
            }
        }
        KeyCode::Char('e') => {
            if let Some(comment) = app.focused_comment().cloned() {
                if comment.is_system() {
                    app.set_toast("system comments are immutable", true);
                } else {
                    app.form = Some(Form::comment_edit(&comment).returning_to(Screen::CardDetail));
                    app.screen = Screen::CardForm;
                }
            } else if let Some(card) = app.detail.as_ref().map(|d| d.card.clone()) {
                app.form = Some(Form::card_edit(&card).returning_to(Screen::CardDetail));
                app.screen = Screen::CardForm;
                return vec![Effect::LoadFormOptions];
            }
        }
        KeyCode::Char('d') => {
            if let Some(comment) = app.focused_comment() {
                if comment.is_system() {
                    app.set_toast("system comments are immutable", true);
                } else {
                    let id = comment.id;
                    app.confirm = Some(Confirm {
                        message: "Delete this comment?".into(),
                        purpose: ConfirmPurpose::DeleteComment(id),
                        return_to: Screen::CardDetail,
                    });
                    app.screen = Screen::Confirm;
                }
            }
        }
        KeyCode::Char('h') => {
            if let Some(comment) = app.focused_comment() {
                return vec![Effect::LoadCommentHistory { id: comment.id }];
            }
        }
        KeyCode::Char('a') => {
            let Some(result) = app
                .detail
                .as_ref()
                .map(|detail| super::archive_card(&detail.card))
            else {
                return vec![];
            };
            match result {
                Ok(effect) => return vec![effect],
                Err(err) => app.set_toast(err.to_string(), true),
            }
        }
        KeyCode::Char('C') => {
            if let Some(id) = card_id {
                return vec![Effect::CardDuplicate(id)];
            }
        }
        KeyCode::Char('c') => {
            if let Some(id) = card_id {
                app.form = Some(Form::comment(id).returning_to(Screen::CardDetail));
                app.screen = Screen::CardForm;
            }
        }
        KeyCode::Char('o') => {
            // Jump to the *selected* run (the highlighted row in the Runs
            // section, the newest run until the user moves the cursor). Never
            // re-derive a "latest run with a pane" here: whether that run's
            // pane is recorded and still exists is the daemon's call, so its
            // error stays the single source of the diagnosis.
            match (card_id, app.focused_run().map(|run| run.id)) {
                (Some(id), Some(run_id)) => return vec![Effect::FocusRun(id, run_id)],
                // A loaded card that has never run: nothing to jump to.
                (Some(_), None) => app.set_toast("this card has no run to jump to", true),
                // `card.get` has not come back yet (or failed): there is no run
                // list to select from, so say so instead of doing nothing.
                (None, _) => app.set_toast("card detail has not loaded yet", true),
            }
        }
        // Enter on an `awaiting` card confirms completion: the same `run.done`
        // (ok) channel as `board done ok`. Other statuses: Enter is a no-op
        // (`done` is a final visual state).
        KeyCode::Enter => {
            if let Some(detail) = &app.detail {
                if detail.card.status == CardStatus::Awaiting {
                    return vec![Effect::RunDone(detail.card.id, RunOutcome::Ok)];
                }
            }
        }
        // Cancel only makes sense while a run is actually open — `run.cancel`
        // on a finished card is refused, so asking "cancel the running run?"
        // when there is none is a question with no true answer.
        KeyCode::Char('x') => {
            if let Some(id) = card_id {
                if has_open_run(app) {
                    app.confirm = Some(Confirm {
                        message: "Cancel the running run?".into(),
                        purpose: ConfirmPurpose::CancelRun(id),
                        return_to: Screen::CardDetail,
                    });
                    app.screen = Screen::Confirm;
                } else {
                    app.set_toast("this card has no open run to cancel", true);
                }
            }
        }
        // Retry relaunches a real agent — strictly more consequential than the
        // cancel above, which already confirms. So it confirms too.
        KeyCode::Char('r') => {
            if let Some(id) = card_id {
                app.confirm = Some(Confirm {
                    message: "Retry this card? A new agent run is launched.".into(),
                    purpose: ConfirmPurpose::RetryRun(id),
                    return_to: Screen::CardDetail,
                });
                app.screen = Screen::Confirm;
            }
        }
        _ => {}
    }
    vec![]
}

/// `↑`/`↓` in card detail: move the focused section's cursor when that section
/// has rows, otherwise fall back to a raw scroll of the whole viewport.
fn detail_nav(app: &mut App, delta: isize) {
    let comments = app.detail.as_ref().map(|d| d.comments.len()).unwrap_or(0);
    let runs = app.detail.as_ref().map(|d| d.runs.len()).unwrap_or(0);
    // Selection, not raw scroll: saturating, never wrapping, with the viewport
    // following the cursor.
    if app.detail_scroll_target == DetailScrollTarget::Comments && comments > 0 {
        app.detail_comment_sel = step_clamped(app.detail_comment_sel, delta, comments - 1);
        app.follow_comment_focus();
    } else if app.detail_scroll_target == DetailScrollTarget::Runs && runs > 0 {
        app.detail_run_sel = step_clamped(app.detail_run_sel, delta, runs - 1);
        app.follow_run_focus();
    } else {
        app.scroll_detail(delta);
    }
}

/// Whether the open card has a run that is still open. The board-scoped
/// active-run summary is the authoritative answer; the detail's own run list
/// is the fallback for a card whose board snapshot has not caught up (an
/// unfinished run is one with no recorded outcome).
fn has_open_run(app: &App) -> bool {
    let Some(detail) = app.detail.as_ref() else {
        return false;
    };
    app.active_run_for_card(detail.card.id).is_some()
        || detail.runs.iter().any(|run| run.outcome.is_none())
}
