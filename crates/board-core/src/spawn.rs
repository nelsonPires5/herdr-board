//! How the daemon launches agent processes. Types + trait only; Phase D
//! implements `HerdrSpawner` (via board-herdr) and `LocalSpawner` (plain child).

use std::path::PathBuf;

/// A request to launch one agent process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnReq {
    /// herdr agent/pane label, e.g. `board-card-42`.
    pub name: String,
    /// Working directory (for `cwd`/`worktree` spaces, or `LocalSpawner`).
    pub cwd: Option<PathBuf>,
    /// herdr workspace id (for `workspace` spaces).
    pub workspace_ref: Option<String>,
    /// Environment pairs to set on the child.
    pub env: Vec<(String, String)>,
    /// The command line.
    pub argv: Vec<String>,
}

/// A handle to a launched process: herdr ids and/or a local pid.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpawnHandle {
    pub pane_id: Option<String>,
    pub workspace_id: Option<String>,
    pub pid: Option<u32>,
}

/// Launch, kill, and liveness-check agent processes.
pub trait Spawner: Send + Sync {
    fn spawn(&self, req: &SpawnReq) -> anyhow::Result<SpawnHandle>;
    fn kill(&self, h: &SpawnHandle) -> anyhow::Result<()>;
    fn is_alive(&self, h: &SpawnHandle) -> anyhow::Result<bool>;
}
