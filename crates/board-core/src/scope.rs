//! Resolve the board scope from CLI/plugin context and a filesystem path.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::Result;

#[derive(Debug, Default, Deserialize)]
struct PluginContext {
    focused_pane_cwd: Option<String>,
    workspace_cwd: Option<String>,
}

/// Select the unnormalized scope candidate without reading process-global state.
///
/// Precedence: explicit non-empty override, focused pane cwd, workspace cwd,
/// then the supplied current directory. Invalid plugin JSON is treated as an
/// absent context so callers can safely fall back.
pub fn select_scope_candidate(
    override_path: Option<&str>,
    plugin_context_json: Option<&str>,
    current_dir: &Path,
) -> Result<PathBuf> {
    if let Some(path) = non_empty(override_path) {
        return Ok(PathBuf::from(path));
    }

    let context = plugin_context_json
        .and_then(|json| serde_json::from_str::<PluginContext>(json).ok())
        .unwrap_or_default();
    if let Some(path) = non_empty(context.focused_pane_cwd.as_deref()) {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = non_empty(context.workspace_cwd.as_deref()) {
        return Ok(PathBuf::from(path));
    }
    Ok(current_dir.to_path_buf())
}

/// Canonicalize a candidate and use its Git root when it belongs to a repo.
/// A missing/failing Git command deliberately falls back to the canonical cwd.
pub fn resolve_scope_path(candidate: &Path) -> Result<PathBuf> {
    let canonical = candidate.canonicalize()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&canonical)
        .args(["rev-parse", "--show-toplevel"])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let root = String::from_utf8_lossy(&output.stdout);
            let root = root.trim();
            if !root.is_empty() {
                if let Ok(root) = Path::new(root).canonicalize() {
                    return Ok(root);
                }
            }
        }
    }
    Ok(canonical)
}

/// A project must point at an existing directory on disk: creating a project
/// registers its canonical path but never creates directories itself. Shared
/// by the daemon op and the fake client so protocol callers see one rule.
pub fn validate_existing_directory(path: &str) -> Result<()> {
    let p = Path::new(path);
    if !p.is_dir() {
        return Err(crate::Error::BadRequest(format!(
            "project path is not an existing directory: {path}"
        )));
    }
    Ok(())
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
