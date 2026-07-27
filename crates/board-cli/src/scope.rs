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

/// Resolve an ID-or-path board selector. Paths are normalized in the same way
/// as the daemon's current-scope fallback, so all board-aware commands share
/// one selection rule.
pub(crate) fn open_selected_board(
    c: &mut UnixClient,
    selector: Option<&str>,
) -> Result<BoardSnapshot> {
    match selector {
        Some(value) => {
            if let Ok(id) = value.parse::<i64>() {
                return c.board_get_by_id(id);
            }
            let path = resolve_scope_path(std::path::Path::new(value))?;
            let path = path
                .to_str()
                .ok_or_else(|| anyhow!("board path is not valid UTF-8: {}", path.display()))?;
            c.board_open(path)
        }
        None => c.board_open(&current_scope_path()?),
    }
}

/// Resolve a column reference within one board snapshot. The matching rule is
/// `board_core::engine::resolve_column`; only the error message is the CLI's.
pub(crate) fn resolve_column_in(snap: &BoardSnapshot, s: &str) -> Result<i64> {
    board_core::engine::resolve_column(&snap.columns, s)
        .ok_or_else(|| anyhow!("no column matching \"{s}\""))
}
