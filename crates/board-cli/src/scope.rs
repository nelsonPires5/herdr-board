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

pub(crate) fn open_current_board(c: &mut UnixClient) -> Result<BoardSnapshot> {
    c.board_open(&current_scope_path()?)
}

/// Resolve a column reference within one board snapshot.
pub(crate) fn resolve_column_in(snap: &BoardSnapshot, s: &str) -> Result<i64> {
    if let Ok(id) = s.parse::<i64>() {
        if snap.columns.iter().any(|col| col.id == id) {
            return Ok(id);
        }
    }
    let lower = s.to_lowercase();
    snap.columns
        .iter()
        .find(|col| col.name.to_lowercase() == lower)
        .map(|col| col.id)
        .ok_or_else(|| anyhow!("no column matching \"{s}\""))
}
