//! `Spawner` implementations: `HerdrSpawner` (agent panes) and `LocalSpawner`
//! (plain child processes, used by tests with the fake harness).

use std::path::PathBuf;
use std::time::Duration;

mod herdr;
mod local;
mod placement;
#[cfg(test)]
mod tests;

pub use herdr::HerdrSpawner;
pub use local::LocalSpawner;

/// A request to launch one agent process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HerdrLaunchPlan {
    /// herdr agent/pane label, e.g. `card-42-execute`.
    pub name: String,
    /// Explicit Herdr managed-agent kind (`pi` or `claude`). `None` means a
    /// configured, unmanaged command; callers must never infer this from argv.
    pub agent_kind: Option<String>,
    /// Card task to submit after a managed agent becomes interactive. `None`
    /// for configured harnesses, which receive `BOARD_PROMPT` in `env`.
    pub initial_prompt: Option<String>,
    /// Authoritative system instructions for a managed agent, transported
    /// separately from startup argv. `None` for configured harnesses, which
    /// receive `BOARD_SYSTEM_PROMPT` in `env`.
    pub system_prompt: Option<String>,
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
pub struct RuntimeHandle {
    pub pane_id: Option<String>,
    pub workspace_id: Option<String>,
    pub pid: Option<u32>,
    /// herdr socket this pane lives on (its session), so kill/liveness target
    /// the right session after a daemon restart. `None` = default socket.
    pub herdr_socket: Option<PathBuf>,
}

/// Launch, kill, and liveness-check agent processes.
pub trait Spawner: Send + Sync {
    fn spawn(&self, req: &HerdrLaunchPlan) -> anyhow::Result<RuntimeHandle>;
    fn kill(&self, h: &RuntimeHandle) -> anyhow::Result<()>;
    fn is_alive(&self, h: &RuntimeHandle) -> anyhow::Result<bool>;
}

pub(crate) const AGENT_START_TIMEOUT_MS: u64 = 30_000;
pub(crate) const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const READINESS_BACKOFF: Duration = Duration::from_millis(100);
pub(crate) const IMMEDIATE_READINESS_PROBES: usize = 3;
