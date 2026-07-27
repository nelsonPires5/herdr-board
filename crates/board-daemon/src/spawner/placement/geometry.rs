//! Pure split geometry. No client, no I/O: given a Herdr [`Layout`] and a
//! target pane, decide which way to split and at what ratio.

use board_herdr::{Layout, LayoutPane, SplitDirection};

pub(crate) const ERR_ANCHOR_TOO_SMALL: &str = "anchor_too_small";

/// The root pane of a durable card tab is deliberately kept as a shell. These
/// dimensions describe the smallest useful anchor/agent split; Herdr itself
/// accepts the ratio but does not enforce a board-specific minimum.
pub(crate) const ANCHOR_MIN_WIDTH: u64 = 24;
pub(crate) const ANCHOR_MIN_HEIGHT: u64 = 6;
pub(crate) const AGENT_MIN_WIDTH: u64 = 12;
pub(crate) const AGENT_MIN_HEIGHT: u64 = 8;
pub(crate) const ANCHOR_RATIO: f64 = 0.40;
pub(crate) const RECOVERY_TARGET_RATIO: f64 = 0.75;

/// The target pane in `layout`, or an error naming the id that is missing.
fn layout_target<'a>(layout: &'a Layout, target_id: &str) -> anyhow::Result<&'a LayoutPane> {
    layout
        .panes
        .iter()
        .find(|pane| pane.pane_id == target_id)
        .ok_or_else(|| anyhow::anyhow!("layout has no target pane {target_id}"))
}

/// Prefer a wide pane's vertical divider, fall back to a horizontal one, and
/// refuse a pane too small to hold both an anchor and an agent. Shared by the
/// new-tab and the anchor-recovery split so the two can never drift apart.
fn choose_split_direction(target: &LayoutPane, target_id: &str) -> anyhow::Result<SplitDirection> {
    let right_ok = target.rect.width >= ANCHOR_MIN_WIDTH + AGENT_MIN_WIDTH
        && target.rect.height >= ANCHOR_MIN_HEIGHT.max(AGENT_MIN_HEIGHT);
    let down_ok = target.rect.height >= ANCHOR_MIN_HEIGHT + AGENT_MIN_HEIGHT
        && target.rect.width >= ANCHOR_MIN_WIDTH.max(AGENT_MIN_WIDTH);
    let prefer_right = target.rect.width >= 2_u64.saturating_mul(target.rect.height);
    if prefer_right && right_ok {
        Ok(SplitDirection::Right)
    } else if down_ok {
        Ok(SplitDirection::Down)
    } else if right_ok {
        Ok(SplitDirection::Right)
    } else {
        Err(anyhow::anyhow!(
            "{ERR_ANCHOR_TOO_SMALL}: pane {target_id} is {}x{}, need at least 36x14 for an anchor and agent",
            target.rect.width,
            target.rect.height
        ))
    }
}

pub(crate) fn initial_split_geometry(
    layout: &Layout,
    target_id: &str,
) -> anyhow::Result<(SplitDirection, f64)> {
    let target = layout_target(layout, target_id)?;
    let direction = choose_split_direction(target, target_id)?;
    let (split_axis, anchor_min, child_min) = match direction {
        SplitDirection::Right => (target.rect.width, ANCHOR_MIN_WIDTH, AGENT_MIN_WIDTH),
        SplitDirection::Down => (target.rect.height, ANCHOR_MIN_HEIGHT, AGENT_MIN_HEIGHT),
    };
    let min_ratio = anchor_min as f64 / split_axis as f64;
    let max_ratio = 1.0 - child_min as f64 / split_axis as f64;
    Ok((direction, ANCHOR_RATIO.clamp(min_ratio, max_ratio)))
}

pub(crate) fn recovery_target_ratio(
    layout: &Layout,
    target_id: &str,
    direction: SplitDirection,
) -> anyhow::Result<f64> {
    let target = layout_target(layout, target_id)?;
    let axis = match direction {
        SplitDirection::Right => target.rect.width,
        SplitDirection::Down => target.rect.height,
    };
    let target_min = match direction {
        SplitDirection::Right => AGENT_MIN_WIDTH,
        SplitDirection::Down => AGENT_MIN_HEIGHT,
    };
    let anchor_min = match direction {
        SplitDirection::Right => ANCHOR_MIN_WIDTH + AGENT_MIN_WIDTH,
        SplitDirection::Down => ANCHOR_MIN_HEIGHT + AGENT_MIN_HEIGHT,
    };
    let total_min = target_min.saturating_add(anchor_min);
    if axis < total_min {
        return Err(anyhow::anyhow!(
            "{ERR_ANCHOR_TOO_SMALL}: pane {target_id} is too small to recreate an anchor and split a child"
        ));
    }
    let cross_axis = match direction {
        SplitDirection::Right => target.rect.height,
        SplitDirection::Down => target.rect.width,
    };
    let cross_min = match direction {
        SplitDirection::Right => ANCHOR_MIN_HEIGHT.max(AGENT_MIN_HEIGHT),
        SplitDirection::Down => ANCHOR_MIN_WIDTH.max(AGENT_MIN_WIDTH),
    };
    if cross_axis < cross_min {
        return Err(anyhow::anyhow!(
            "{ERR_ANCHOR_TOO_SMALL}: pane {target_id} is too narrow to recreate an anchor"
        ));
    }
    let min_target_ratio = target_min as f64 / axis as f64;
    let max_target_ratio = 1.0 - anchor_min as f64 / axis as f64;
    Ok(RECOVERY_TARGET_RATIO.clamp(min_target_ratio, max_target_ratio))
}

pub(crate) fn split_geometry(
    layout: &Layout,
    target_id: &str,
) -> anyhow::Result<(SplitDirection, f64)> {
    let target = layout_target(layout, target_id)?;
    let direction = choose_split_direction(target, target_id)?;
    let (split_axis, cross_axis, anchor_min, child_min) = match direction {
        SplitDirection::Right => (
            target.rect.width,
            target.rect.height,
            ANCHOR_MIN_WIDTH,
            AGENT_MIN_WIDTH,
        ),
        SplitDirection::Down => (
            target.rect.height,
            target.rect.width,
            ANCHOR_MIN_HEIGHT,
            AGENT_MIN_HEIGHT,
        ),
    };
    let cross_min = match direction {
        SplitDirection::Right => ANCHOR_MIN_HEIGHT.max(AGENT_MIN_HEIGHT),
        SplitDirection::Down => ANCHOR_MIN_WIDTH.max(AGENT_MIN_WIDTH),
    };
    if split_axis < anchor_min.saturating_add(child_min) || cross_axis < cross_min {
        return Err(anyhow::anyhow!(
            "{ERR_ANCHOR_TOO_SMALL}: pane {target_id} is {}x{}, need at least {}x{} for an anchor and agent",
            target.rect.width,
            target.rect.height,
            if matches!(direction, SplitDirection::Right) {
                anchor_min + child_min
            } else {
                cross_min
            },
            if matches!(direction, SplitDirection::Right) {
                cross_min
            } else {
                anchor_min + child_min
            }
        ));
    }
    let min_ratio = anchor_min as f64 / split_axis as f64;
    let max_ratio = 1.0 - child_min as f64 / split_axis as f64;
    let ratio = ANCHOR_RATIO.clamp(min_ratio, max_ratio);
    Ok((direction, ratio))
}
