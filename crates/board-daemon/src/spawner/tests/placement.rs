use super::super::placement::{
    initial_split_geometry, recovery_target_ratio, split_geometry, ANCHOR_RATIO,
    ERR_ANCHOR_TOO_SMALL,
};
use super::*;
use board_herdr::Layout;

fn layout(pane_id: &str, width: u64, height: u64) -> Layout {
    Layout {
        workspace_id: "w1".into(),
        tab_id: "w1:t1".into(),
        zoomed: false,
        area: Rect {
            x: 0,
            y: 0,
            width,
            height,
        },
        focused_pane_id: pane_id.into(),
        panes: vec![pane(pane_id, width, height)],
        splits: Vec::new(),
    }
}

#[test]
fn card_anchor_uses_stable_ratio_and_direction_for_wide_and_tall_layouts() {
    let (direction, ratio) = initial_split_geometry(&layout("root", 200, 40), "root").unwrap();
    assert_eq!(direction, SplitDirection::Right);
    assert_eq!(ratio, ANCHOR_RATIO);

    let (direction, ratio) = initial_split_geometry(&layout("root", 60, 50), "root").unwrap();
    assert_eq!(direction, SplitDirection::Down);
    assert_eq!(ratio, ANCHOR_RATIO);
}

#[test]
fn card_anchor_rejects_layouts_below_the_two_pane_minimum() {
    let error = initial_split_geometry(&layout("root", 23, 13), "root").unwrap_err();
    assert!(error.to_string().contains(ERR_ANCHOR_TOO_SMALL));

    let error = split_geometry(&layout("anchor", 23, 13), "anchor").unwrap_err();
    assert!(error.to_string().contains(ERR_ANCHOR_TOO_SMALL));
}

#[test]
fn anchor_recovery_reserves_space_for_the_existing_run_and_new_child() {
    let (direction, ratio) = split_geometry(&layout("anchor", 200, 40), "anchor").unwrap();
    assert_eq!(direction, SplitDirection::Right);
    assert_eq!(ratio, ANCHOR_RATIO);

    let ratio =
        recovery_target_ratio(&layout("run", 48, 22), "run", SplitDirection::Right).unwrap();
    // At the exact 48-cell recovery minimum, the new anchor needs 36 cells;
    // the durable run therefore retains the remaining 12-cell minimum.
    assert_eq!(ratio, 0.25);
    let error =
        recovery_target_ratio(&layout("run", 47, 22), "run", SplitDirection::Right).unwrap_err();
    assert!(error.to_string().contains(ERR_ANCHOR_TOO_SMALL));
}

#[test]
fn single_pane_is_the_split_target() {
    let panes = [pane("p1", 200, 40)];
    let (target, _) = grid_slot(&panes);
    assert_eq!(target, "p1");
}

#[test]
fn wide_pane_splits_right() {
    // width (200) >= 2 * height (40) → Right.
    let panes = [pane("p1", 200, 40)];
    let (_, dir) = grid_slot(&panes);
    assert_eq!(dir, SplitDirection::Right);
}

#[test]
fn tall_narrowish_pane_splits_down() {
    // width (60) < 2 * height (50) → Down.
    let panes = [pane("p1", 60, 50)];
    let (target, dir) = grid_slot(&panes);
    assert_eq!(target, "p1");
    assert_eq!(dir, SplitDirection::Down);
}

#[test]
fn largest_area_pane_wins() {
    let panes = [
        pane("small", 50, 10),
        pane("biggest", 200, 40),
        pane("medium", 30, 30),
    ];
    let (target, dir) = grid_slot(&panes);
    assert_eq!(target, "biggest");
    assert_eq!(dir, SplitDirection::Right);
}
