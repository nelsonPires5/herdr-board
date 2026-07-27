use std::io::{self, IsTerminal, Write};

use anyhow::{anyhow, bail, Result};
use board_core::capability::HarnessCapabilities;
use board_core::client::{BoardClient, UnixClient};
use board_core::protocol::{CardVisibility, Effort, RunOutcome, SpaceKind, Trigger};

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

pub(crate) fn actor_run_id() -> Result<Option<i64>> {
    match std::env::var("BOARD_RUN_ID") {
        Ok(value) => Ok(Some(value.parse::<i64>().map_err(|_| {
            anyhow!("invalid $BOARD_RUN_ID '{value}': expected an integer")
        })?)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("invalid $BOARD_RUN_ID: value is not valid UTF-8")
        }
    }
}

pub(crate) fn actor_author(run_id: Option<i64>) -> Option<String> {
    run_id.map(|id| format!("agent:{id}"))
}

/// One shape for every enum-valued option: `invalid <kind> '<value>' (expected:
/// a, b, c)`. Parsing itself always belongs to `board-core`'s `parse_str`; only
/// the message and the advertised value list are the CLI's business (the CLI
/// deliberately advertises fewer outcomes than the wire enum accepts).
fn parse_enum<T>(
    kind: &str,
    value: &str,
    expected: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<T> {
    parse(value).ok_or_else(|| anyhow!("invalid {kind} '{value}' (expected: {expected})"))
}

/// Parse a `--space-kind` CLI value. `new_workspace` is also accepted, but the
/// hyphenated spelling is the documented one.
pub(crate) fn parse_space_kind(s: &str) -> Result<SpaceKind> {
    parse_enum(
        "space-kind",
        s,
        "workspace, new-workspace",
        SpaceKind::parse_str,
    )
}

pub(crate) fn parse_effort(s: Option<String>) -> Result<Option<Effort>> {
    s.as_deref()
        .map(|value| {
            parse_enum(
                "effort",
                value,
                "off, minimal, low, medium, high, xhigh, max",
                Effort::parse_str,
            )
        })
        .transpose()
}

pub(crate) fn parse_trigger(s: Option<String>) -> Result<Option<Trigger>> {
    s.as_deref()
        .map(|value| parse_enum("trigger", value, "manual, auto", Trigger::parse_str))
        .transpose()
}

pub(crate) fn parse_outcome(s: &str) -> Result<RunOutcome> {
    parse_enum("outcome", s, "ok, fail", RunOutcome::parse_str)
}

pub(crate) fn parse_visibility(s: Option<String>) -> Result<Option<CardVisibility>> {
    s.as_deref()
        .map(|value| {
            parse_enum(
                "visibility",
                value,
                "active, all, archived",
                CardVisibility::parse_str,
            )
        })
        .transpose()
}

/// Require `--yes` in automation, while retaining a normal TTY prompt for
/// humans. This is deliberately performed before connecting to boardd.
pub(crate) fn confirm_action(kind: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        bail!("{kind} requires confirmation; pass --yes when stdin is not a TTY")
    }
    eprint!("Delete {kind}? [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        bail!("{kind} cancelled")
    }
}

pub(crate) fn origin_socket(explicit: Option<String>) -> Result<String> {
    if let Some(socket) = explicit {
        return Ok(socket);
    }
    std::env::var("HERDR_SOCKET_PATH")
        .or_else(|_| std::env::var("HERDR_SOCK"))
        .map_err(|_| anyhow!("--origin-socket is required outside a Herdr session"))
}
