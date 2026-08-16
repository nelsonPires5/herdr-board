use anyhow::{anyhow, Context, Result};
use board_core::client::{BoardClient, UnixClient};
use board_core::protocol::BoardSnapshot;
use board_core::scope::{resolve_scope_path, select_scope_candidate};

pub(crate) fn current_scope_path() -> Result<String> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let override_path = std::env::var("BOARD_SCOPE_PATH").ok();
    let plugin_context = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok();
    let candidate =
        select_scope_candidate(override_path.as_deref(), plugin_context.as_deref(), &cwd)?;
    let resolved = resolve_scope_path(&candidate)?;
    resolved.to_str().map(str::to_string).ok_or_else(|| {
        anyhow!(
            "board scope path is not valid UTF-8: {}",
            resolved.display()
        )
    })
}

/// Canonicalize a path argument to the daemon's canonical scope form.
pub(crate) fn resolved_scope_path(path: &str) -> Result<String> {
    let resolved = resolve_scope_path(std::path::Path::new(path))?;
    resolved
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("scope path is not valid UTF-8: {}", resolved.display()))
}

/// The CLI/TUI context board: explicit flags select (persisted); otherwise the
/// daemon's persisted selection prevails; when no selection exists yet
/// (post-migration), the current directory's project is opened (bootstrap).
pub(crate) fn context_board(
    c: &mut UnixClient,
    project: Option<&str>,
    selector: Option<&str>,
) -> Result<BoardSnapshot> {
    let resolved_path = |s: &str| -> Result<String> {
        let p = resolve_scope_path(std::path::Path::new(s))?;
        p.to_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("board path is not valid UTF-8: {}", p.display()))
    };
    if let Some(path) = project {
        return Ok(c.project_select(&resolved_path(path)?, None)?.board);
    }
    match selector {
        Some(value) => {
            if let Ok(id) = value.parse::<i64>() {
                return c.board_select(id);
            }
            Ok(c.project_open(&resolved_path(value)?)?.board)
        }
        None => {
            let sel = c.project_selected()?;
            match (sel.project, sel.board) {
                (Some(_), Some(board)) => Ok(board),
                _ => Ok(c.project_open(&current_scope_path()?)?.board),
            }
        }
    }
}

/// Resolve a column reference within one board snapshot. The matching rule is
/// `board_core::engine::resolve_column`; only the error message is the CLI's.
pub(crate) fn resolve_column_in(snap: &BoardSnapshot, s: &str) -> Result<i64> {
    board_core::engine::resolve_column(&snap.columns, s)
        .ok_or_else(|| anyhow!("no column matching \"{s}\""))
}
