//! How the daemon launches agent processes. Types + trait only; Phase D
//! implements `HerdrSpawner` (via board-herdr) and `LocalSpawner` (plain child).

use std::path::PathBuf;

/// A request to launch one agent process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnReq {
    /// herdr agent/pane label, e.g. `card-42-execute`.
    pub name: String,
    /// Fallback agent name to retry with when `name` is already taken (herdr
    /// agent names are exclusive while a pane using one is open). `None` skips
    /// the retry.
    pub name_fallback: Option<String>,
    /// herdr tab label to place the agent pane in (find-or-create + grid
    /// layout). `None` = no tab placement (cwd spaces / `LocalSpawner`, which
    /// ignore it). Only honored when `workspace_ref` is also set.
    pub tab_label: Option<String>,
    /// Working directory (resolved workspace cwd, or `LocalSpawner`).
    pub cwd: Option<PathBuf>,
    /// herdr workspace id (for `workspace` / `new_workspace` spaces).
    pub workspace_ref: Option<String>,
    /// herdr socket to spawn on, resolved from the card's session. `None` =
    /// the spawner's default socket (`LocalSpawner` ignores it).
    pub herdr_socket: Option<PathBuf>,
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
    /// herdr socket this pane lives on (its session), so kill/liveness target
    /// the right session after a daemon restart. `None` = default socket.
    pub herdr_socket: Option<PathBuf>,
}

/// Launch, kill, and liveness-check agent processes.
pub trait Spawner: Send + Sync {
    fn spawn(&self, req: &SpawnReq) -> anyhow::Result<SpawnHandle>;
    fn kill(&self, h: &SpawnHandle) -> anyhow::Result<()>;
    fn is_alive(&self, h: &SpawnHandle) -> anyhow::Result<bool>;
}
