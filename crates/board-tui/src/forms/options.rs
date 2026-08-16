//! Dynamic option rebuilding, cascading choices, focus, and visibility.

use std::borrow::Cow;

use board_core::capability::{default_capabilities, HarnessCapabilities};
use board_core::harness::DEFAULT_HARNESS;
use board_core::model::Column;
use board_core::protocol::{ProjectInfo, SessionInfo, SpaceInfo};

use super::builders::{board_choice_opts, project_choice_opts};
use super::{
    build_card_fields, column_fields_from_values, CardValues, ChoiceOpt, ChoiceVal, ColumnValues,
    Field, FieldId, FieldKind, Form, FormKind,
};

impl Form {
    /// Whether this form has the guided card selectors (model/effort/…).
    pub fn is_card_form(&self) -> bool {
        matches!(
            self.kind,
            FormKind::CardCreate { .. } | FormKind::CardEdit { .. }
        )
    }

    /// Whether this form is a column create/edit form.
    pub fn is_column_form(&self) -> bool {
        matches!(
            self.kind,
            FormKind::ColumnCreate | FormKind::ColumnEdit { .. }
        )
    }

    /// The harness the guided selectors should be populated for: the card
    /// `Harness` field for card forms, the column `HarnessOverride` field for
    /// column forms. Drives which `harness.capabilities` the loader fetches.
    pub fn current_harness(&self) -> String {
        let id = if self.is_column_form() {
            FieldId::HarnessOverride
        } else {
            FieldId::Harness
        };
        self.opt_choice_str(id)
            .unwrap_or_else(|| DEFAULT_HARNESS.to_string())
    }

    /// The currently selected herdr session (`None` = the daemon's default
    /// session). Drives which session `space.list` is fetched for.
    pub fn current_session(&self) -> Option<String> {
        self.opt_choice_str(FieldId::Session)
    }

    /// Install freshly fetched capabilities / harness list / spaces / sessions
    /// and rebuild the guided fields (preserving whatever the user already
    /// selected/typed). A `None` argument means the fetch failed — the affected
    /// selectors fall back to free-text / minimal menus. No-op for comment forms.
    pub fn apply_options(
        &mut self,
        caps: Option<HarnessCapabilities>,
        harnesses: Option<Vec<String>>,
        spaces: Option<Vec<SpaceInfo>>,
        sessions: Option<(Vec<SessionInfo>, String)>,
    ) {
        self.caps = caps;
        if let Some(h) = harnesses {
            self.harnesses = h;
        }
        if self.is_card_form() {
            if let Some(sp) = spaces {
                self.spaces = sp;
            }
            if let Some((se, default_label)) = sessions {
                self.sessions = se;
                self.session_default_label = Some(default_label);
            }
        }
        self.rebuild_fields();
    }

    /// React to a model change: effort options follow the selected model, and
    /// the current effort is kept only if still valid (else reset to default).
    pub fn on_model_changed(&mut self) {
        self.rebuild_card_fields();
    }

    /// React to a space-kind change: the space ref flips between the workspace
    /// selector and free text.
    pub fn on_space_kind_changed(&mut self) {
        self.rebuild_card_fields();
    }

    /// React to a column trigger change: the `system_prompt` field is shown for
    /// `auto` (a run is launched) and hidden for `manual` (no run). Visibility
    /// is dynamic, so this only reconciles focus — a `SystemPrompt` that was
    /// focused when the user switched to `manual` must move to the next visible
    /// field. The field is **hidden, not omitted**: it stays in the field list
    /// carrying its value, so submit still sends a `system_prompt` Patch that
    /// preserves the DB value (manual columns keep their prompt).
    pub fn on_trigger_changed(&mut self) {
        if !self.field_visible(self.focus) {
            self.focus_step(1);
        }
    }

    /// React to the move form's Project field changing: rebuild the Board
    /// selector from `projects` for the newly chosen project (selected board
    /// first, then alphabetically) and reset the Column selector — the loaded
    /// columns belong to the previous board. Focus moves to the Board field so
    /// the next choice is the natural one.
    pub fn apply_move_project(&mut self, projects: &[ProjectInfo], project_id: i64) {
        let Some(info) = projects.iter().find(|pi| pi.project.id == project_id) else {
            return;
        };
        let (opts, idx) = board_choice_opts(Some(info), None);
        if let Some(field) = self.fields.iter_mut().find(|f| f.id == FieldId::MoveBoard) {
            field.kind = FieldKind::Choice { opts, idx };
        }
        if let Some(field) = self.fields.iter_mut().find(|f| f.id == FieldId::MoveColumn) {
            field.kind = FieldKind::Choice {
                opts: Vec::new(),
                idx: 0,
            };
        }
        if let Some(focus) = self.fields.iter().position(|f| f.id == FieldId::MoveBoard) {
            self.focus = focus;
        }
    }

    /// Seed (or re-seed) the move form's Project/Board selectors from a fresh
    /// `project.list` cache, preserving the current project/board selections
    /// wherever they still exist. Called when the cache first loads — the form
    /// can open before any picker has fetched it — and on every later refresh.
    pub fn apply_projects(
        &mut self,
        projects: &[ProjectInfo],
        current: &board_core::model::Project,
        current_board: &board_core::model::Board,
    ) {
        let (project_opts, _) = project_choice_opts(projects, current);
        let project_idx = self
            .opt_col(FieldId::MoveProject)
            .and_then(|id| {
                project_opts
                    .iter()
                    .position(|o| matches!(o.val, ChoiceVal::Col(x) if x == id))
            })
            .unwrap_or(0);
        if let Some(field) = self
            .fields
            .iter_mut()
            .find(|f| f.id == FieldId::MoveProject)
        {
            field.kind = FieldKind::Choice {
                opts: project_opts,
                idx: project_idx,
            };
        }
        // Board options follow the selected project; the current board stands
        // in as the fallback (and preselection) only for the current project.
        let selected_project = self.opt_col(FieldId::MoveProject).unwrap_or(current.id);
        let info = projects.iter().find(|pi| pi.project.id == selected_project);
        let fallback = (selected_project == current.id).then_some(current_board);
        let (board_opts, board_idx) = board_choice_opts(info, fallback);
        let board_idx = self
            .opt_col(FieldId::MoveBoard)
            .and_then(|id| {
                board_opts
                    .iter()
                    .position(|o| matches!(o.val, ChoiceVal::Col(x) if x == id))
            })
            .unwrap_or(board_idx);
        if let Some(field) = self.fields.iter_mut().find(|f| f.id == FieldId::MoveBoard) {
            field.kind = FieldKind::Choice {
                opts: board_opts,
                idx: board_idx,
            };
        }
    }

    /// Install freshly fetched destination-board columns into the move form's
    /// Column selector, preserving the current selection when it is still one
    /// of the columns (a reload of the same board).
    pub fn apply_move_columns(&mut self, columns: Vec<Column>) {
        self.columns = columns;
        let current = self.opt_col(FieldId::MoveColumn);
        let opts: Vec<ChoiceOpt> = self
            .columns
            .iter()
            .map(|c| ChoiceOpt {
                label: c.name.clone(),
                val: ChoiceVal::Col(c.id),
            })
            .collect();
        let idx = current
            .and_then(|id| {
                opts.iter()
                    .position(|o| matches!(o.val, ChoiceVal::Col(x) if x == id))
            })
            .unwrap_or(0);
        if let Some(field) = self.fields.iter_mut().find(|f| f.id == FieldId::MoveColumn) {
            field.kind = FieldKind::Choice { opts, idx };
        }
    }

    fn rebuild_fields(&mut self) {
        if self.is_card_form() {
            self.rebuild_card_fields();
        } else if self.is_column_form() {
            self.rebuild_column_fields();
        }
    }

    fn rebuild_card_fields(&mut self) {
        if !self.is_card_form() {
            return;
        }
        let values = self.card_values();
        self.fields = build_card_fields(
            &values,
            self.caps.as_ref(),
            &self.harnesses,
            &self.spaces,
            &self.sessions,
            self.session_default_label.as_deref(),
        );
        if self.focus >= self.fields.len() {
            self.focus = 0;
        }
        if !self.field_visible(self.focus) {
            self.focus_step(1);
        }
    }

    /// Rebuild the column fields after caps / harness list arrive or after the
    /// harness-override changes. Values that stay valid are preserved; an
    /// effort/permission override that the new harness doesn't offer resets to
    /// the default option.
    fn rebuild_column_fields(&mut self) {
        if !self.is_column_form() {
            return;
        }
        let values = self.column_values();
        self.fields =
            column_fields_from_values(&values, &self.columns, self.caps.as_ref(), &self.harnesses);
        if self.focus >= self.fields.len() {
            self.focus = 0;
        }
        if !self.field_visible(self.focus) {
            self.focus_step(1);
        }
    }

    /// Snapshot the current card field values (for rebuilding in place).
    fn card_values(&self) -> CardValues {
        CardValues {
            title: self.raw_text(FieldId::Title),
            description: self.raw_text(FieldId::Description),
            harness: self.current_harness(),
            model: self.card_model().unwrap_or_default(),
            model_custom_selected: self.model_is_custom(),
            effort: self.opt_choice_str(FieldId::Effort),
            permission: self.opt_choice_str(FieldId::Permission),
            session: self.current_session(),
            space_kind: self
                .opt_choice_str(FieldId::SpaceKind)
                .unwrap_or_else(|| "workspace".to_string()),
            space_ref: self.card_space_ref().unwrap_or_default(),
            space_cwd: self.raw_text(FieldId::SpaceCwd),
        }
    }

    /// Snapshot the current column field values (for rebuilding in place).
    /// The override harness drives caps; `effort_override`/`permission_override`
    /// are read as the effective choice/text string (`None` = default/none).
    fn column_values(&self) -> ColumnValues {
        ColumnValues {
            name: self.raw_text(FieldId::Name),
            system_prompt: self.raw_text(FieldId::SystemPrompt),
            trigger: self.opt_choice_str(FieldId::Trigger),
            on_success: self.opt_col(FieldId::OnSuccess),
            on_fail: self.opt_col(FieldId::OnFail),
            fresh_session: self.opt_bool(FieldId::FreshSession),
            harness_override: self.opt_str_field(FieldId::HarnessOverride),
            model_override: self.opt_text(FieldId::ModelOverride),
            effort_override: self.opt_str_field(FieldId::EffortOverride),
            permission_override: self.opt_str_field(FieldId::PermissionOverride),
            timeout: self.opt_int(FieldId::Timeout),
        }
    }

    fn raw_text(&self, id: FieldId) -> String {
        self.field(id).map(|f| f.get_text()).unwrap_or_default()
    }

    fn model_is_custom(&self) -> bool {
        matches!(
            self.field(FieldId::Model).and_then(|f| f.choice_val()),
            Some(ChoiceVal::Custom)
        )
    }

    fn space_ref_is_custom(&self) -> bool {
        matches!(
            self.field(FieldId::SpaceRef).and_then(|f| f.choice_val()),
            Some(ChoiceVal::Custom)
        )
    }

    /// The effective model string: the selected catalog id, the custom text, or
    /// the raw free-text field (fallback).
    pub(super) fn card_model(&self) -> Option<String> {
        match self.field(FieldId::Model).map(|f| &f.kind) {
            Some(FieldKind::Choice { .. }) => {
                match self.field(FieldId::Model).and_then(|f| f.choice_val()) {
                    Some(ChoiceVal::Str(s)) => Some(s.clone()),
                    Some(ChoiceVal::Custom) => self.opt_text(FieldId::ModelCustom),
                    _ => None,
                }
            }
            _ => self.opt_text(FieldId::Model),
        }
    }

    /// The effective space ref: the selected workspace id, the custom text, or
    /// the raw free-text field (non-workspace kinds / fallback).
    pub(super) fn card_space_ref(&self) -> Option<String> {
        match self.field(FieldId::SpaceRef).map(|f| &f.kind) {
            Some(FieldKind::Choice { .. }) => {
                match self.field(FieldId::SpaceRef).and_then(|f| f.choice_val()) {
                    Some(ChoiceVal::Str(s)) => Some(s.clone()),
                    Some(ChoiceVal::Custom) => self.opt_text(FieldId::SpaceRefCustom),
                    _ => None,
                }
            }
            _ => self.opt_text(FieldId::SpaceRef),
        }
    }

    // -- focus / visibility --------------------------------------------------

    /// Whether a field is currently shown. The `(custom)` free-text companion
    /// appears only when the `SpaceRef` selector is on `(custom)`; `cwd` only for
    /// the `new_workspace` space kind; both `permission` selectors disappear
    /// when the driving harness has no permission modes (e.g. Pi); the column
    /// `system prompt` is hidden for `manual` triggers (no run → no prompt) but
    /// stays in the field list so its value is preserved and submitted.
    pub fn field_visible(&self, idx: usize) -> bool {
        match self.fields[idx].id {
            FieldId::SpaceCwd => self.space_kind_is_new_workspace(),
            FieldId::ModelCustom => self.model_is_custom(),
            FieldId::Permission | FieldId::PermissionOverride => self.permission_is_applicable(),
            FieldId::SpaceRefCustom => self.space_ref_is_custom(),
            FieldId::SystemPrompt => self.trigger_is_auto(),
            _ => true,
        }
    }

    /// Whether the column trigger is `auto` (a run launches on card entry).
    /// `manual` (and the unset create-form default) hides the `system_prompt`
    /// field — manual columns never launch a run, so the prompt is unused.
    pub(super) fn trigger_is_auto(&self) -> bool {
        self.fields
            .iter()
            .find(|f| f.id == FieldId::Trigger)
            .and_then(|f| f.choice_val())
            .map(|v| matches!(v, ChoiceVal::Str(s) if s == "auto"))
            .unwrap_or(false)
    }

    /// Whether any permission selector applies for the form's driving harness:
    /// `!permission_modes.is_empty()` against [`Form::effective_caps`], so the
    /// answer is capability data in both the fetched and the not-yet-fetched
    /// case and never a harness-name comparison.
    pub(super) fn permission_is_applicable(&self) -> bool {
        !self.effective_caps().permission_modes.is_empty()
    }

    /// The capability snapshot driving the guided selectors: the live daemon
    /// catalog whenever it has been fetched, else board-core's built-in
    /// default for the selected harness.
    ///
    /// A harness board-core does not know answers with an *empty* permission
    /// vocabulary — inventing another CLI's enum is never safe — so a
    /// config-defined harness only gets its real permission modes once
    /// `harness.capabilities` has answered. That is the daemon's call to make,
    /// not the TUI's.
    pub(super) fn effective_caps(&self) -> Cow<'_, HarnessCapabilities> {
        match &self.caps {
            Some(caps) => Cow::Borrowed(caps),
            None => Cow::Owned(default_capabilities(&self.current_harness())),
        }
    }

    pub(super) fn space_kind_is_new_workspace(&self) -> bool {
        self.fields
            .iter()
            .find(|f| f.id == FieldId::SpaceKind)
            .and_then(|f| f.choice_val())
            .map(|v| matches!(v, ChoiceVal::Str(s) if s == "new_workspace"))
            .unwrap_or(false)
    }

    /// Move focus to the next/previous visible field (wrapping).
    pub fn focus_step(&mut self, delta: isize) {
        let n = self.fields.len();
        if n == 0 {
            return;
        }
        let mut i = self.focus;
        for _ in 0..n {
            i = (i as isize + delta).rem_euclid(n as isize) as usize;
            if self.field_visible(i) {
                self.focus = i;
                return;
            }
        }
    }

    pub fn focused(&self) -> &Field {
        &self.fields[self.focus]
    }
    pub fn focused_mut(&mut self) -> &mut Field {
        &mut self.fields[self.focus]
    }

    pub fn focused_is_choice(&self) -> bool {
        self.fields[self.focus].is_choice()
    }
    pub fn focused_is_multiline(&self) -> bool {
        self.fields[self.focus].multiline
    }

    /// Read-only access to one field by id.
    pub fn field(&self, id: FieldId) -> Option<&Field> {
        self.fields.iter().find(|f| f.id == id)
    }

    /// Mutable access to one field by id (tests and the editor path).
    pub fn field_mut(&mut self, id: FieldId) -> Option<&mut Field> {
        self.fields.iter_mut().find(|f| f.id == id)
    }
}
