use anyhow::{anyhow, bail, Result};
use board_core::capability::HarnessCapabilities;
use board_core::client::{BoardClient, UnixClient};
use board_core::protocol::{Effort, SpaceKind};

/// Fetch a harness's capability catalog (`harness.capabilities`).
pub(crate) fn harness_capabilities(
    c: &mut UnixClient,
    harness: &str,
) -> Result<HarnessCapabilities> {
    c.harness_capabilities(harness)
}

/// Render an effort list space-separated (e.g. `low medium high xhigh max`).
pub(crate) fn efforts_str(efforts: &[Effort]) -> String {
    efforts
        .iter()
        .map(|e| e.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Deduplicated default/free-form efforts followed by every model's efforts,
/// preserving first-seen order.
pub(crate) fn union_efforts(caps: &HarnessCapabilities) -> Vec<Effort> {
    let mut out = caps.default_efforts.clone();
    for m in &caps.models {
        for e in &m.efforts {
            if !out.contains(e) {
                out.push(*e);
            }
        }
    }
    out
}

pub(crate) fn env_card_id() -> Result<i64> {
    std::env::var("BOARD_CARD_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("no card id given and $BOARD_CARD_ID is unset"))
}

/// Parse a `--space-kind` CLI value. Accepts `workspace` and `new-workspace`
/// (the wire form `new_workspace` is also tolerated); anything else is an error.
pub(crate) fn parse_space_kind(s: &str) -> Result<SpaceKind> {
    match s {
        "workspace" => Ok(SpaceKind::Workspace),
        "new-workspace" | "new_workspace" => Ok(SpaceKind::NewWorkspace),
        other => bail!("invalid space-kind '{other}' (expected: workspace, new-workspace)"),
    }
}

pub(crate) fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}
