//! How the daemon launches agent processes. Types + trait only; Phase D
//! implements `HerdrSpawner` (via board-herdr) and `LocalSpawner` (plain child).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Harness-neutral, fully materialized execution inputs. Values are persisted
/// at enqueue time so later card/column/config edits cannot change a launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionSpec {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub agent_kind: Option<String>,
    pub initial_prompt: Option<String>,
    pub system_prompt: Option<String>,
}

/// Durable launch description with an independent format version. Placement
/// remains daemon-owned; unsupported versions are rejected rather than guessed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunLaunchSpec {
    version: u32,
    execution: ExecutionSpec,
}

impl RunLaunchSpec {
    pub const VERSION: u32 = 1;

    pub fn v1(execution: ExecutionSpec) -> Self {
        Self {
            version: Self::VERSION,
            execution,
        }
    }

    pub fn execution(&self) -> &ExecutionSpec {
        &self.execution
    }
}

impl<'de> Deserialize<'de> for RunLaunchSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct DurableSpec {
            version: u32,
            execution: ExecutionSpec,
        }

        let spec = DurableSpec::deserialize(deserializer)?;
        if spec.version != Self::VERSION {
            return Err(serde::de::Error::custom(format!(
                "unsupported launch spec version {}",
                spec.version
            )));
        }
        Ok(Self::v1(spec.execution))
    }
}

/// A request to launch one agent process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnReq {
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
