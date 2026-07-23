use board_core::protocol::SpaceKind;
use board_herdr::{HerdrClient, WorkspaceCreateParams, WorkspaceInfo};

use crate::HERDR_PROTOCOL;

/// Resolve a card's space within its session to `(workspace_id, cwd)`.
///
/// - [`SpaceKind::Workspace`]: `space_ref` is an existing workspace id or a
///   case-insensitive label; cwd comes from the workspace's pane snapshot.
/// - [`SpaceKind::NewWorkspace`]: reuse an open workspace whose label matches
///   `space_ref`, else `workspace.create {label, cwd}`; in either case cwd is
///   verified from the resulting workspace's live pane snapshot.
pub(crate) fn resolve_space(
    client: &mut HerdrClient,
    kind: SpaceKind,
    space_ref: Option<&str>,
    space_cwd: Option<&str>,
) -> anyhow::Result<(String, String)> {
    // Dispatch performs workspace discovery before handing off to the spawner,
    // so the selected socket must be gated here as well as in HerdrSpawner.
    client.require_protocol(HERDR_PROTOCOL).map_err(|error| {
        let message = error.to_string();
        anyhow::Error::new(error).context(format!(
            "checking Herdr protocol before workspace resolution: {message}"
        ))
    })?;
    let workspaces = client.workspace_list()?;
    match kind {
        SpaceKind::Workspace => {
            let ws_ref =
                space_ref.ok_or_else(|| anyhow::anyhow!("workspace space requires a space_ref"))?;
            let id = resolve_workspace_ref(&workspaces, ws_ref).map_err(|m| anyhow::anyhow!(m))?;
            let cwd = workspace_cwd(client, &id)?;
            Ok((id, cwd))
        }
        SpaceKind::NewWorkspace => {
            let label = space_ref.filter(|s| !s.trim().is_empty()).ok_or_else(|| {
                anyhow::anyhow!("new_workspace space requires a label (space_ref)")
            })?;
            let cwd = space_cwd
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("new_workspace space requires space_cwd"))?;
            match find_workspace_by_label(&workspaces, label) {
                // A reused workspace must use a cwd from one of its live
                // panes. Protocol 17 does not inherit workspace cwd, so the
                // card's original create cwd is not a safe fallback here.
                Some(id) => {
                    let live = workspace_cwd(client, &id)?;
                    Ok((id, live))
                }
                None => {
                    let created = client.workspace_create(&WorkspaceCreateParams {
                        label: Some(label.to_string()),
                        cwd: Some(cwd.to_string()),
                        focus: false,
                        ..Default::default()
                    })?;
                    let id = created.workspace_id().to_string();
                    let live = workspace_cwd(client, &id)?;
                    Ok((id, live))
                }
            }
        }
    }
}

/// Look up a workspace's cwd via one of its live panes in the session snapshot.
///
/// Protocol 17 placement is pane-first and never inherits a workspace cwd, so
/// failure to read this value must stop dispatch rather than launch from an
/// implicit daemon/Herdr fallback directory.
fn workspace_cwd(client: &mut HerdrClient, workspace_id: &str) -> anyhow::Result<String> {
    let snapshot = client.session_snapshot().map_err(|error| {
        // `anyhow::Error`'s Display shows only the outermost context. Include
        // the rendered cause in that context so a dispatch failure tells the
        // operator both which cwd lookup failed and why the snapshot failed,
        // while retaining the original error chain for callers using `{:#}`.
        let cause = format!("{error:#}");
        anyhow::Error::new(error).context(format!(
            "session snapshot unavailable while reading cwd for workspace '{workspace_id}': {cause}"
        ))
    })?;
    snapshot
        .panes
        .iter()
        .find(|pane| pane.workspace_id == workspace_id)
        .and_then(|pane| pane.cwd.as_deref())
        .filter(|cwd| !cwd.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("workspace '{workspace_id}' has no live pane cwd"))
}

/// Resolve a `workspace` space_ref (id, else case-insensitive label) to a
/// workspace id among the open `workspaces`. Err message lists the known ones.
pub(crate) fn resolve_workspace_ref(
    workspaces: &[WorkspaceInfo],
    ws_ref: &str,
) -> std::result::Result<String, String> {
    workspaces
        .iter()
        .find(|w| w.workspace_id == ws_ref)
        .or_else(|| {
            workspaces
                .iter()
                .find(|w| w.label.eq_ignore_ascii_case(ws_ref))
        })
        .map(|w| w.workspace_id.clone())
        .ok_or_else(|| {
            let known: Vec<String> = workspaces
                .iter()
                .map(|w| format!("{} ({})", w.workspace_id, w.label))
                .collect();
            format!(
                "herdr workspace '{ws_ref}' not found by id or label; known: {}",
                known.join(", ")
            )
        })
}

/// Find an open workspace whose label case-insensitively matches `label`.
pub(crate) fn find_workspace_by_label(workspaces: &[WorkspaceInfo], label: &str) -> Option<String> {
    workspaces
        .iter()
        .find(|w| w.label.eq_ignore_ascii_case(label))
        .map(|w| w.workspace_id.clone())
}
