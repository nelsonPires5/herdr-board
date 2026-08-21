//! Every read path the driver performs: refetching the board, refreshing the
//! project cache, building the project/board pickers, loading a card's detail,
//! and fetching the catalog metadata (`harness.*`, `session.list`, `space.list`)
//! the card/column forms need.

use anyhow::Result;
use board_core::capability::HarnessCapabilities;
use board_core::client::BoardClient;
use board_core::protocol::{
    ProjectCreateParams, ProjectInfo, ProjectListResult, SessionInfo, SpaceInfo,
};

use crate::app::{clamp_selection, Picker, PickerAction, PickerPurpose, PickerRow, Screen};
use crate::forms::FormKind;
use crate::view::{board_label, project_label};
use crate::Driver;

impl Driver {
    pub(super) fn refetch(&mut self) {
        let r = self.client.board_get_by_id(self.app.board.board.id);
        if let Some(snap) = self.guard(r) {
            self.app.board = snap;
            clamp_selection(&mut self.app);
        }
    }

    /// `project.list` → refresh the `app.projects` cache and re-derive
    /// `app.project` from the current board's project when the cache knows it.
    /// Errors are toasted; a failed refresh keeps the previous cache.
    pub(super) fn refresh_projects(&mut self) {
        let r = self.client.project_list();
        if let Some(result) = self.guard(r) {
            self.install_projects(result);
        }
    }

    fn install_projects(&mut self, result: ProjectListResult) {
        self.app.projects = result.projects;
        self.app.projects_loaded = true;
        if let Some(pi) = self
            .app
            .projects
            .iter()
            .find(|pi| pi.project.id == self.app.board.board.project_id)
        {
            self.app.project = pi.project.clone();
        }
        // An open move form built its Project/Board selectors before the
        // cache loaded; seed them now (and on every later refresh).
        if let Some(form) = self.app.form.as_mut() {
            if matches!(form.kind, FormKind::MoveCard { .. }) {
                form.apply_projects(&self.app.projects, &self.app.project, &self.app.board.board);
            }
        }
    }

    /// Fetch `project.list` and open the project picker. Rows: current project
    /// first, then recent projects in order, then the remaining projects
    /// alphabetically (Global last), then the "＋ New project" action. The
    /// picker's `return_to` is whatever screen is active right now — the board
    /// when opened via `p`, the board picker when drilled into via "⇄ Other
    /// projects…".
    pub(super) fn load_project_picker(&mut self) {
        let r = self
            .client
            .project_list_visible(Some(self.app.picker_visibility));
        let Some(result) = self.guard(r) else {
            return;
        };
        self.install_projects(result.clone());
        let current = self.app.project.clone();

        let mut rows: Vec<PickerRow> = Vec::new();
        rows.push(PickerRow::Item(project_label(&current), current.id));
        let mut seen: Vec<i64> = vec![current.id];
        for id in result.recent_project_ids {
            if seen.contains(&id) {
                continue;
            }
            if let Some(pi) = result.projects.iter().find(|pi| pi.project.id == id) {
                rows.push(PickerRow::Item(project_label(&pi.project), id));
                seen.push(id);
            }
        }
        let mut rest: Vec<&ProjectInfo> = result
            .projects
            .iter()
            .filter(|pi| !seen.contains(&pi.project.id))
            .collect();
        rest.sort_by(|a, b| {
            (
                a.project.scope_path.is_none(),
                a.project.name.to_lowercase(),
            )
                .cmp(&(
                    b.project.scope_path.is_none(),
                    b.project.name.to_lowercase(),
                ))
        });
        rows.extend(
            rest.into_iter()
                .map(|pi| PickerRow::Item(project_label(&pi.project), pi.project.id)),
        );
        rows.push(PickerRow::Action(
            "＋ New project".into(),
            PickerAction::NewProject,
        ));

        let return_to = self.app.screen;
        let visibility = self.app.picker_visibility;
        self.app.picker = Some(Picker {
            title: format!(
                "Switch project [{}]",
                visibility.as_str().to_ascii_uppercase()
            ),
            rows,
            sel: 0,
            purpose: PickerPurpose::SwitchProject,
            return_to,
            project_id: current.id,
        });
        self.app.screen = Screen::ProjectPicker;
    }

    /// Fetch `project.list` and open the board picker for `project_id` (`None`
    /// = the current project). Rows: the project's selected board first (its
    /// first board when none is selected), then recent boards in order, then
    /// the remaining boards alphabetically, then the "⇄ Other projects…" and
    /// "＋ New board" actions. The picker's `return_to` is whatever screen is
    /// active right now — the board when opened via `b`, the switcher when
    /// drilled from its "switch board" row, the project picker when drilled
    /// from a project choice.
    pub(super) fn load_board_picker(&mut self, project_id: Option<i64>) {
        use board_core::protocol::Visibility;
        // Board picker must keep the target project reachable even when the
        // picker's visibility would otherwise filter it away (e.g. an active
        // project while the picker is showing "ARCHIVED"). Fetch all projects
        // and filter only the boards inside the target project.
        let r = self.client.project_list_visible(Some(Visibility::All));
        if let Some(result) = self.guard(r) {
            self.install_projects(result);
        }
        let target_id = project_id.unwrap_or(self.app.board.board.project_id);
        let Some(mut info) = self
            .app
            .projects
            .iter()
            .find(|pi| pi.project.id == target_id)
            .cloned()
        else {
            self.app.set_toast("project not found", true);
            return;
        };
        // Filter boards inside the target project according to the current picker visibility.
        let picker_vis = self.app.picker_visibility;
        info.boards.retain(|b| match picker_vis {
            Visibility::Active => b.archived_at.is_none(),
            Visibility::Archived => b.archived_at.is_some(),
            Visibility::All => true,
        });
        // Recent board ids that no longer survive the filter must be ignored.
        info.recent_board_ids
            .retain(|id| info.boards.iter().any(|b| b.id == *id));
        // selected_board_id may point to a now-hidden board; fall back to first visible.
        if let Some(sel) = info.selected_board_id {
            if !info.boards.iter().any(|b| b.id == sel) {
                info.selected_board_id = info.boards.first().map(|b| b.id);
            }
        }

        let mut rows: Vec<PickerRow> = Vec::new();
        let mut seen: Vec<i64> = Vec::new();
        let first_id = info
            .selected_board_id
            .or_else(|| info.boards.first().map(|b| b.id));
        if let Some(id) = first_id {
            if let Some(board) = info.boards.iter().find(|b| b.id == id) {
                rows.push(PickerRow::Item(board_label(board), id));
                seen.push(id);
            }
        }
        for id in &info.recent_board_ids {
            if seen.contains(id) {
                continue;
            }
            if let Some(board) = info.boards.iter().find(|b| b.id == *id) {
                rows.push(PickerRow::Item(board_label(board), *id));
                seen.push(*id);
            }
        }
        let mut rest: Vec<board_core::model::Board> = info
            .boards
            .iter()
            .filter(|b| !seen.contains(&b.id))
            .cloned()
            .collect();
        rest.sort_by_key(|b| b.name.to_lowercase());
        rows.extend(
            rest.into_iter()
                .map(|b| PickerRow::Item(board_label(&b), b.id)),
        );
        rows.push(PickerRow::Action(
            "⇄ Other projects…".into(),
            PickerAction::OtherProjects,
        ));
        rows.push(PickerRow::Action(
            "＋ New board".into(),
            PickerAction::NewBoard,
        ));

        let return_to = self.app.screen;
        let title = format!(
            "Switch board · {} [{}]",
            info.project.name,
            picker_vis.as_str().to_ascii_uppercase()
        );
        self.app.picker = Some(Picker {
            title,
            rows,
            sel: 0,
            purpose: PickerPurpose::SwitchBoard,
            return_to,
            project_id: target_id,
        });
        self.app.screen = Screen::BoardPicker;
    }

    /// `board.select`: persist the board (and its project) as the context, then
    /// replace the TUI's context and refresh the project cache.
    pub(super) fn select_board(&mut self, board_id: i64) {
        let r = self.client.board_select(board_id);
        if let Some(snap) = self.guard(r) {
            self.app.replace_board(snap);
            self.refresh_projects();
            self.set_pane_title(self.app.card_filter);
        }
    }

    /// `project.select`: persist the project + explicit board choice as the
    /// context, then replace the TUI's context and refresh the project cache.
    /// The Global project has no scope path, so selecting one of its boards
    /// goes through `board.select` — the same persisted selection side effect.
    pub(super) fn select_project(&mut self, project_id: i64, board_id: i64) {
        let Some(project) = self
            .app
            .projects
            .iter()
            .find(|pi| pi.project.id == project_id)
            .map(|pi| pi.project.clone())
        else {
            self.app.set_toast("project not found", true);
            return;
        };
        let Some(scope_path) = project.scope_path.clone() else {
            self.select_board(board_id);
            return;
        };
        let r = self.client.project_select(&scope_path, Some(board_id));
        if let Some(result) = self.guard(r) {
            self.app.project = result.project;
            self.app.replace_board(result.board);
            self.refresh_projects();
            self.set_pane_title(self.app.card_filter);
        }
    }

    /// `project.create`: create the project (with its `main` board), land on
    /// it, refresh the cache, and confirm with a toast.
    pub(super) fn create_project(&mut self, p: ProjectCreateParams) {
        let r = self.client.project_create(&p.scope_path);
        let Some(result) = self.guard(r) else {
            return;
        };
        self.app.project = result.project.clone();
        self.app.replace_board(result.board);
        self.refresh_projects();
        self.set_pane_title(self.app.card_filter);
        self.app
            .set_toast(format!("Created project {}", result.project.name), false);
    }

    /// `board.create`: create a named board in a project (auto-selected by the
    /// daemon), land on it, refresh the cache, and confirm with a toast.
    pub(super) fn create_board(&mut self, p: board_core::protocol::BoardCreateParams) {
        let r = self.client.board_create(p.project_id, &p.name);
        let Some(snap) = self.guard(r) else {
            return;
        };
        // The board's project may be new to the cache (created in this same
        // session), so refresh before `replace_board` re-derives `app.project`.
        self.refresh_projects();
        self.app.replace_board(snap);
        self.set_pane_title(self.app.card_filter);
        self.app
            .set_toast(format!("Created board {}", p.name), false);
    }

    /// Fetch `board.get` for the move form's chosen destination board and
    /// populate its Column selector. No selection side effect.
    pub(super) fn load_move_columns(&mut self, board_id: i64) {
        let r = self.client.board_get_by_id(board_id);
        let Some(snap) = self.guard(r) else {
            return;
        };
        if let Some(form) = self.app.form.as_mut() {
            if matches!(form.kind, FormKind::MoveCard { .. }) {
                form.apply_move_columns(snap.columns);
            }
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
