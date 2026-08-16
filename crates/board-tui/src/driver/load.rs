//! Every read path the driver performs: refetching the board, building the
//! board/column option lists the pickers show, loading a card's detail, and
//! fetching the catalog metadata (`harness.*`, `session.list`, `space.list`)
//! the card/column forms need.

use anyhow::Result;
use board_core::capability::HarnessCapabilities;
use board_core::client::BoardClient;
use board_core::protocol::{SessionInfo, SpaceInfo};

use crate::app::{clamp_selection, column_options, Picker, PickerPurpose, Screen};
use crate::view::board_picker_label;
use crate::Driver;

impl Driver {
    pub(super) fn refetch(&mut self) {
        let r = self.client.board_get_by_id(self.app.board.board.id);
        if let Some(snap) = self.guard(r) {
            self.app.board = snap;
            clamp_selection(&mut self.app);
        }
    }

    /// `board.list` as picker options plus the index of the active board — the
    /// shared first half of every "choose a board" flow (`b`, the Compact
    /// switcher's second level, and stage 1 of a cross-board move). `None` when
    /// the fetch failed (already toasted).
    fn board_options(&mut self) -> Option<(Vec<(String, i64)>, usize)> {
        let r = self.client.board_list();
        let result = self.guard(r)?;
        let options = result
            .boards
            .iter()
            .map(|board| (board_picker_label(board), board.id))
            .collect();
        let sel = result
            .boards
            .iter()
            .position(|board| board.id == self.app.board.board.id)
            .unwrap_or(0);
        Some((options, sel))
    }

    pub(super) fn load_boards(&mut self) {
        let Some((options, sel)) = self.board_options() else {
            return;
        };
        self.app.picker = Some(Picker {
            title: "Switch board".into(),
            options,
            sel,
            purpose: PickerPurpose::SwitchBoard,
            return_to: Screen::Board,
        });
        self.app.screen = Screen::Picker;
    }

    /// Compact switcher level 2: fetch boards into `app.switcher` in place
    /// (does not touch the Regular/Wide `Picker`).
    pub(super) fn load_boards_for_switcher(&mut self) {
        let Some((boards, sel)) = self.board_options() else {
            return;
        };
        if let Some(state) = self.app.switcher.as_mut() {
            crate::app::enter_boards_level(state, boards, sel);
        }
    }

    pub(super) fn switch_board(&mut self, board_id: i64) {
        let r = self.client.board_get_by_id(board_id);
        if let Some(board) = self.guard(r) {
            self.app.replace_board(board);
            self.set_pane_title(self.app.card_filter);
        }
    }

    /// Cross-board move, stage 1: open the destination-board picker. Reuses the
    /// same board list as `b`, but with a move purpose so `Enter` advances to
    /// stage 2 instead of switching the active board.
    pub(super) fn load_boards_for_move(&mut self, card_id: i64) {
        let Some((options, sel)) = self.board_options() else {
            return;
        };
        self.app.picker = Some(Picker {
            title: "Move card to which board?".into(),
            options,
            sel,
            purpose: PickerPurpose::MoveCardPickBoard { card_id },
            return_to: Screen::Board,
        });
        self.app.screen = Screen::Picker;
    }

    /// Cross-board move, stage 2: load the selected destination board's columns
    /// into the picker. Does not change the active board.
    pub(super) fn load_columns_for_move(&mut self, card_id: i64, board_id: i64) {
        let r = self.client.board_get_by_id(board_id);
        if let Some(snap) = self.guard(r) {
            let options = column_options(&snap.columns, None);
            // A board with no columns has no destination to offer; opening an
            // empty picker would be a dead end (and its Enter would have
            // nothing to index).
            if options.is_empty() {
                self.app
                    .set_toast(format!("{} has no columns", snap.board.name), true);
                return;
            }
            self.app.picker = Some(Picker {
                title: format!("Move card to which column? ({})", snap.board.name),
                options,
                sel: 0,
                purpose: PickerPurpose::MoveCardPickColumn { card_id, board_id },
                return_to: Screen::Board,
            });
            self.app.screen = Screen::Picker;
        }
    }

    pub(super) fn load_detail(&mut self, id: i64) {
        let r = self.client.card_get(id);
        if let Some(detail) = self.guard(r) {
            self.app.detail = Some(detail);
            let len = self
                .app
                .detail
                .as_ref()
                .map(|d| d.comments.len())
                .unwrap_or(0);
            // `detail_comment_sel == usize::MAX` is the sentinel
            // `App::open_detail` sets before dispatching `LoadDetail` ("not yet
            // focused anywhere"); clamping it against the freshly fetched
            // comment count below lands it on the newest comment, matching
            // `scroll_detail_to_latest`'s bottom-open behaviour. A normal
            // in-range value (a reload after a comment edit/delete) is
            // preserved instead of reset, so editing/deleting a comment
            // doesn't jump focus elsewhere.
            self.app.detail_comment_sel = if len == 0 {
                0
            } else {
                self.app.detail_comment_sel.min(len - 1)
            };
            // The run cursor follows the exact same rule: the `usize::MAX`
            // sentinel clamps onto the newest run (what `o` targets by
            // default), while an in-range cursor survives a refresh so a
            // reload never yanks the user off the run they picked. Clamping on
            // every load also keeps a shrinking run list in bounds.
            let runs = self.app.detail.as_ref().map(|d| d.runs.len()).unwrap_or(0);
            self.app.detail_run_sel = if runs == 0 {
                0
            } else {
                self.app.detail_run_sel.min(runs - 1)
            };
            self.app.scroll_detail_to_latest();
        }
    }

    pub(super) fn reload_open_detail(&mut self) {
        if let Some(id) = self.app.detail.as_ref().map(|d| d.card.id) {
            self.load_detail(id);
        }
    }

    /// Fetch form metadata and hand it to the open form. Column forms only
    /// need capabilities and the harness catalog; card forms additionally load
    /// sessions and their session-scoped workspaces. A failed fetch is
    /// non-fatal: affected selectors fall back to free-text and the user gets a
    /// status-line warning.
    pub(super) fn load_form_options(&mut self) {
        let Some(form) = self.app.form.as_ref() else {
            return;
        };
        let harness = form.current_harness();
        let is_card_form = form.is_card_form();
        // Column forms only need the selected harness metadata. They have no
        // session or workspace selectors, so avoid unrelated RPCs entirely.
        let session = form.current_session();
        let caps = fetch_capabilities(self.client.as_mut(), &harness);
        let harnesses = fetch_harness_list(self.client.as_mut());
        let sessions = is_card_form.then(|| fetch_sessions(self.client.as_mut()));
        let spaces = is_card_form.then(|| fetch_spaces(self.client.as_mut(), session.as_deref()));

        let mut warning: Option<String> = None;
        let caps_opt = match caps {
            Ok(c) => Some(c),
            Err(e) => {
                warning = Some(format!("capabilities unavailable ({e}); free-text"));
                None
            }
        };
        // harness.list failing is non-fatal: the selectors keep the built-ins.
        let harnesses_opt = harnesses.ok();
        let spaces_opt = match spaces {
            Some(Ok(s)) => Some(s),
            Some(Err(e)) => {
                if warning.is_none() {
                    warning = Some(format!("spaces unavailable ({e}); free-text"));
                }
                None
            }
            None => None,
        };
        // Sessions failing is non-fatal: keep the default-session option
        // (daemon-sent label) + any preselection.
        let sessions_opt = sessions.and_then(Result::ok);
        if let Some(form) = self.app.form.as_mut() {
            form.apply_options(caps_opt, harnesses_opt, spaces_opt, sessions_opt);
        }
        if let Some(w) = warning {
            self.app.set_toast(w, true);
        }
    }
}

/// Fetch `harness.capabilities` for `harness` through the typed client API.
fn fetch_capabilities(client: &mut dyn BoardClient, harness: &str) -> Result<HarnessCapabilities> {
    client.harness_capabilities(harness)
}

/// Fetch `harness.list` (built-ins + config-defined) through the typed client
/// API. Drives the harness/harness-override selects so config-defined
/// harnesses appear without a client-side config read.
fn fetch_harness_list(client: &mut dyn BoardClient) -> Result<Vec<String>> {
    Ok(client.harness_list()?.harnesses)
}

/// Fetch `space.list` (scoped to `session`, `None` = default) through the typed
/// client API.
fn fetch_spaces(client: &mut dyn BoardClient, session: Option<&str>) -> Result<Vec<SpaceInfo>> {
    Ok(client.space_list(session)?.spaces)
}

/// Fetch `session.list` through the typed client API: the session list plus
/// the daemon-sent `default session` marker label.
fn fetch_sessions(client: &mut dyn BoardClient) -> Result<(Vec<SessionInfo>, String)> {
    let result = client.session_list()?;
    Ok((result.sessions, result.default_label))
}
