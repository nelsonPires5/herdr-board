//! `Spawner` implementations: `HerdrSpawner` (agent panes) and `LocalSpawner`
//! (plain child processes, used by tests with the fake harness).

use std::path::PathBuf;
use std::time::Duration;

mod card_tabs;
mod error;
mod herdr;
mod local;
mod placement;
mod rescue;
#[cfg(test)]
mod tests;

pub use card_tabs::CardTabRegistry;
pub use error::SpawnError;
pub use herdr::HerdrSpawner;
pub use local::LocalSpawner;
pub(crate) use placement::CardOwnership;
pub(crate) use rescue::{rescue_run_pane, RescueOutcome, RescuePlan};

/// One-shot bootstrap placement evidence from a workspace this dispatch just
/// created: the exact initial tab and root pane of the brand-new workspace.
/// Only the first card-tab allocation may adopt it (after strict verification);
/// reused/existing/user workspaces never produce a hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceBootstrapHint {
    pub tab_id: String,
    pub root_pane_id: String,
}

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
    /// Herdr tab label for the placement request. Card-tab ownership is never
    /// inferred from this non-unique label.
    pub tab_label: Option<String>,
    /// An exact board-owned tab id reconstructed from a durable run pane, if
    /// available. The optional value itself is not ownership proof: dispatch
    /// supplies it only after validating the pane/session/workspace boundary;
    /// the Herdr spawner uses only that exact id together with the expected
    /// label.
    pub owned_tab_id: Option<String>,
    /// Durable board-run pane ids, newest first. These prove the tab after a
    /// daemon restart and authorize recovery of a missing anchor without
    /// touching a foreign pane.
    pub durable_pane_ids: Vec<String>,
    /// Exact ended child ids that may be reclaimed before the next split.
    pub reclaimable_pane_ids: Vec<String>,
    /// Exact persisted anchor ids, newest first.
    pub durable_anchor_pane_ids: Vec<String>,
    /// A prior run-child pane id to reuse on a same-conversation resume hop
    /// (no fresh split, no `agent.start`): the live agent is re-prompted for
    /// the next stage. `None` keeps the historical always-split behavior.
    pub reuse_pane_id: Option<String>,
    /// Working directory (resolved workspace cwd, or `LocalSpawner`).
    pub cwd: Option<PathBuf>,
    /// herdr workspace id (for `workspace` / `new_workspace` spaces).
    pub workspace_ref: Option<String>,
    /// herdr socket to spawn on, resolved from the card's session. `None` =
    /// the spawner's default socket (`LocalSpawner` ignores it).
    pub herdr_socket: Option<PathBuf>,
    /// One-shot bootstrap hint from a workspace this dispatch just created.
    /// The first card-tab allocation verifies and adopts its exact tab/root;
    /// any mismatch falls back to a fresh `tab.create` without touching it.
    pub bootstrap: Option<WorkspaceBootstrapHint>,
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
    /// Exact persistent card-tab shell anchor, when placement used one.
    pub anchor_pane_id: Option<String>,
    pub pid: Option<u32>,
    /// herdr socket this pane lives on (its session), so kill/liveness target
    /// the right session after a daemon restart. `None` = default socket.
    pub herdr_socket: Option<PathBuf>,
    /// Harness-reported conversation/thread id captured after launch
    /// (self-minting integrations like codex report it only once the agent is
    /// up, via `agent.get.agent_session`). The daemon persists it atomically
    /// with run promotion + card running/session update; `None` means no
    /// capture was attempted (pi/claude, reuse) or none validated.
    pub captured_session_id: Option<String>,
}

/// Launch, kill, and liveness-check agent processes.
pub trait Spawner: Send + Sync {
    /// Launch one agent. The [`SpawnError`] discriminant keeps the Herdr
    /// failure mode (deadline / protocol refusal / dropped transport) legible
    /// at the dispatch boundary instead of flattening it into a string.
    fn spawn(&self, req: &HerdrLaunchPlan) -> Result<RuntimeHandle, SpawnError>;
    fn kill(&self, h: &RuntimeHandle) -> anyhow::Result<()>;
    fn is_alive(&self, h: &RuntimeHandle) -> anyhow::Result<bool>;
    /// The shared card-tab allocation registry, when this spawner places panes
    /// into Herdr card tabs. The `run.focus` rescue takes the same per-card lock
    /// through it, so a rescue and a dispatch cannot both create a `card-<id>`
    /// tab or both split a pane into one. `None` for spawners with no Herdr
    /// placement (`LocalSpawner`), which simply means there is nothing to
    /// serialize against.
    fn card_tabs(&self) -> Option<std::sync::Arc<CardTabRegistry>> {
        None
    }
}

pub(crate) const AGENT_START_TIMEOUT_MS: u64 = 30_000;
/// How many delayed retries `agent.start` may take on a freshly split owned
/// pane before the run fails. Herdr answers `agent_pane_busy` until the pane's
/// login shell reaches an interactive prompt; a slow shell (zsh + oh-my-zsh +
/// nvm) can need ~0.5s while the residual-agent-state window is only a few
/// hundred ms. The backoff doubles per retry (100/200/400/800/1600ms), so five
/// retries extend the window to roughly 3.1s after the initial attempt.
pub(crate) const AGENT_START_BUSY_RETRIES: usize = 5;
pub(crate) const AGENT_START_BUSY_BACKOFF: Duration = Duration::from_millis(100);
pub(crate) const READINESS_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const READINESS_BACKOFF: Duration = Duration::from_millis(100);
pub(crate) const IMMEDIATE_READINESS_PROBES: usize = 3;
/// Total `agent.get` probes of the post-launch session capture (self-minting
/// harnesses like codex). After the immediate probes the capture backs off
/// with [`READINESS_BACKOFF`], and [`SESSION_CAPTURE_TIMEOUT`] is the hard
/// wall-clock cap — the capture is bounded and degrades to `None` instead of
/// polling forever, so a late or missing thread report never stalls launch.
pub(crate) const SESSION_CAPTURE_PROBES: usize = 5;
pub(crate) const SESSION_CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);
