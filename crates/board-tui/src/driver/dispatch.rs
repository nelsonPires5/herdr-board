//! The [`Effect`] dispatch table.
//!
//! Every write effect has the same shape — call the client, toast-and-stop on
//! error, otherwise refresh what the write invalidated — so it is expressed
//! once in [`Driver::mutate`] and each arm only says *which* call and *what to
//! refresh*. Read effects delegate to `driver::load`.

use anyhow::Result;

use crate::app::{CardFilter, CommentHistoryView, Effect, Screen};
use crate::view::pane_title;
use crate::Driver;
use board_core::protocol::{RunFocusAction, RunFocusResult};

/// What a successful mutation refreshes, and in which order.
///
/// The order is part of the observable behaviour (each refresh can raise its
/// own error toast and re-clamp the selection), so the variants spell it out
/// rather than leaving it to the helper.
enum After {
    /// Refetch the board only.
    Board,
    /// Refetch the board, then reload whichever card detail is open.
    BoardThenDetail,
    /// Reload whichever card detail is open, then refetch the board.
    DetailThenBoard,
    /// Reload whichever card detail is open; leave the board alone.
    Detail,
    /// Reload *this* card's detail, then refetch the board. The run effects
    /// carry the card id explicitly instead of depending on what is open.
    CardThenBoard(i64),
}

impl Driver {
    /// Apply one write effect's result: on error toast and change nothing, on
    /// success run `after`. Returns whether the call succeeded, for the two
    /// arms that add a confirmation toast of their own.
    fn mutate<T>(&mut self, result: Result<T>, after: After) -> bool {
        if self.guard(result).is_none() {
            return false;
        }
        match after {
            After::Board => self.refetch(),
            After::BoardThenDetail => {
                self.refetch();
                self.reload_open_detail();
            }
            After::DetailThenBoard => {
                self.reload_open_detail();
                self.refetch();
            }
            After::Detail => self.reload_open_detail(),
            After::CardThenBoard(id) => {
                self.load_detail(id);
                self.refetch();
            }
        }
        true
    }

    pub(super) fn dispatch(&mut self, eff: Effect) {
        match eff {
            Effect::Refetch => self.refetch(),
            Effect::LoadBoards => self.load_boards(),
            Effect::LoadBoardsForSwitcher => self.load_boards_for_switcher(),
            Effect::SwitchBoard(id) => self.switch_board(id),
            Effect::LoadBoardsForMove { card_id } => self.load_boards_for_move(card_id),
            Effect::LoadColumnsForMove { card_id, board_id } => {
                self.load_columns_for_move(card_id, board_id)
            }
            Effect::LoadDetail(id) => self.load_detail(id),
            Effect::CardCreate(p) => {
                let r = self.client.card_create(&p);
                self.mutate(r, After::Board);
            }
            Effect::CardUpdate(p) => {
                let r = self.client.card_update(&p);
                self.mutate(r, After::BoardThenDetail);
            }
            Effect::CardDelete(id) => {
                let r = self.client.card_delete(id);
                self.mutate(r, After::Board);
            }
            Effect::CardArchive { id, archived } => {
                let r = self.client.card_archive(id, archived);
                if self.mutate(r, After::BoardThenDetail) {
                    self.app.set_toast(
                        if archived {
                            "card archived"
                        } else {
                            "card restored"
                        },
                        false,
                    );
                }
            }
            Effect::CardMove(p) => {
                let r = self.client.card_move(&p);
                self.mutate(r, After::Board);
            }
            Effect::ColumnCreate(p) => {
                let r = self.client.column_create(&p);
                self.mutate(r, After::Board);
            }
            Effect::ColumnUpdate(p) => {
                let r = self.client.column_update(&p);
                self.mutate(r, After::Board);
            }
            Effect::ColumnReorder { id, position } => {
                let r = self.client.column_reorder(id, position);
                self.mutate(r, After::Board);
            }
            Effect::ColumnDelete { id, move_cards_to } => {
                let r = self.client.column_delete(id, move_cards_to);
                self.mutate(r, After::Board);
            }
            Effect::CommentAdd { card_id, body } => {
                let r = self.client.comment_add(card_id, &body, None);
                self.mutate(r, After::DetailThenBoard);
            }
            Effect::CommentUpdate { id, body } => {
                let r = self.client.comment_update(id, &body, None);
                self.mutate(r, After::Detail);
            }
            Effect::CommentDelete { id } => {
                let r = self.client.comment_delete(id, None);
                self.mutate(r, After::Detail);
            }
            Effect::LoadCommentHistory { id } => {
                let r = self.client.comment_history(id);
                if let Some(entries) = self.guard(r) {
                    self.app.comment_history = Some(CommentHistoryView {
                        comment_id: id,
                        entries,
                        scroll: 0,
                    });
                    self.app.screen = Screen::CommentHistory;
                }
            }
            Effect::TemplateApply(name) => {
                let r = self
                    .client
                    .template_apply_for_board(&name, Some(self.app.board.board.id));
                self.mutate(r, After::Board);
            }
            Effect::RunCancel(id) => {
                let r = self.client.run_cancel(id);
                self.mutate(r, After::CardThenBoard(id));
            }
            Effect::RunRetry(id) => {
                let r = self.client.run_retry(id);
                self.mutate(r, After::CardThenBoard(id));
            }
            Effect::RunDone(id, outcome) => {
                let r = self.client.run_done(id, outcome, None);
                self.mutate(r, After::CardThenBoard(id));
            }
            Effect::FocusRun(card_id, run_id) => self.focus_run(card_id, run_id),
            Effect::EditFocusedTextArea => self.edit_focused(),
            Effect::LoadFormOptions => self.load_form_options(),
            Effect::SetPaneTitle(filter) => self.set_pane_title(filter),
            Effect::Quit => self.app.should_quit = true,
        }
    }

    /// Update the label Herdr renders in this pane's border, through the
    /// daemon's `pane.set_title` — the TUI owns no Herdr connection of its own.
    ///
    /// Deliberately best-effort at *this* call site: the daemon reports a
    /// failed rename as an error, but a cosmetic border title must never toast
    /// over the board, so the result is dropped exactly as the pre-RPC
    /// subprocess exit status was. Outside a Herdr plugin pane (tests,
    /// examples, standalone TUI) it is a no-op — there is no pane to rename,
    /// and no invoking session to rename it in.
    pub(super) fn set_pane_title(&mut self, filter: CardFilter) {
        if self.origin.plugin_id.as_deref() != Some("herdr-board") {
            return;
        }
        let (Some(pane_id), Some(origin_socket)) = (
            self.origin.pane_id.clone(),
            self.origin.origin_socket.clone(),
        ) else {
            return;
        };
        let title = pane_title(&self.app.board.board, filter);
        let _ = self.client.pane_set_title(&pane_id, &title, &origin_socket);
    }

    fn focus_run(&mut self, card_id: i64, run_id: i64) {
        let Some(origin_socket) = self.origin.origin_socket.clone() else {
            self.app.set_toast(
                "jump to pane requires Herdr (HERDR_SOCKET_PATH is unset)",
                true,
            );
            return;
        };
        match self.client.run_focus(card_id, run_id, &origin_socket) {
            // Focusing an existing pane means the user's attention now
            // belongs to Herdr, so the TUI steps aside. A *rescue* is a
            // new pane the user did not ask for by name, so say what
            // happened before leaving.
            Ok(result) => match result.action {
                RunFocusAction::FocusedRecordedPane => self.app.should_quit = true,
                // A rescue is not what the user literally asked for, so
                // it must be explained. Herdr has already moved focus to
                // the rescued pane, so quitting here would only throw
                // the explanation away — stay up and toast instead.
                RunFocusAction::Rescued | RunFocusAction::FocusedRescuedPane => {
                    self.app.set_toast(focus_rescue_toast(&result), false);
                }
            },
            // The daemon owns the authoritative diagnosis (nothing to
            // resume, harness cannot resume, cross-session, Herdr down)
            // — the TUI only renders it, non-fatally, and leaves the
            // board usable.
            Err(e) => self.app.set_toast(format!("run #{run_id}: {e}"), true),
        }
    }
}

/// Describe a rescue in one toast line: what was reopened, where, and the fact
/// that the new pane is ephemeral (no run row ⇒ the daemon does not watch or
/// time it out).
fn focus_rescue_toast(result: &RunFocusResult) -> String {
    match result.action {
        RunFocusAction::FocusedRescuedPane => format!(
            "run #{}: its pane is gone; focused the reopened pane {} (ephemeral, not tracked \
             as a run)",
            result.run_id, result.pane_id
        ),
        _ => format!(
            "run #{}: its pane is gone; resumed the {} session in new pane {} (ephemeral, not \
             tracked as a run)",
            result.run_id, result.harness, result.pane_id
        ),
    }
}
