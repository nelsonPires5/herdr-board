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

#[test]
fn geometry_reports_the_missing_target_pane_by_id() {
    // Every entry point resolves the target the same way, so a layout that
    // does not contain it must name it identically rather than guess a pane.
    for error in [
        initial_split_geometry(&layout("root", 200, 40), "ghost").unwrap_err(),
        split_geometry(&layout("root", 200, 40), "ghost").unwrap_err(),
        recovery_target_ratio(&layout("root", 200, 40), "ghost", SplitDirection::Right)
            .unwrap_err(),
    ] {
        assert_eq!(error.to_string(), "layout has no target pane ghost");
    }
}

#[test]
fn initial_and_recovery_splits_agree_on_direction_for_the_same_pane() {
    // The two entry points share one direction-choice helper; a pane that is
    // too narrow to prefer Right must never disagree between them.
    for (width, height, expected) in [
        (200_u64, 40_u64, SplitDirection::Right),
        (60, 50, SplitDirection::Down),
        // Wide enough to prefer Right, but too short for a Down split.
        (80, 14, SplitDirection::Right),
        // Prefers Right on aspect, but too narrow for two panes side by side.
        (35, 16, SplitDirection::Down),
    ] {
        let (initial, _) = initial_split_geometry(&layout("root", width, height), "root").unwrap();
        let (split, _) = split_geometry(&layout("root", width, height), "root").unwrap();
        assert_eq!(initial, expected, "initial for {width}x{height}");
        assert_eq!(split, expected, "split for {width}x{height}");
    }
}

#[test]
fn split_ratio_is_clamped_so_both_panes_keep_their_minimum() {
    // 60 cells wide is exactly where the 0.40 anchor ratio still clears the
    // 24-cell anchor floor, so the stable ratio survives unclamped.
    let (_, ratio) = split_geometry(&layout("anchor", 60, 20), "anchor").unwrap();
    assert_eq!(ratio, ANCHOR_RATIO);

    // Narrower than that, the anchor is clamped *up* to its 24-cell floor
    // rather than being handed the stable 0.40 share.
    let (_, ratio) = split_geometry(&layout("anchor", 40, 20), "anchor").unwrap();
    assert_eq!(ratio, 24.0 / 40.0);

    // A Down split clamps on the height axis instead (6-cell anchor floor).
    let (direction, ratio) = split_geometry(&layout("anchor", 30, 14), "anchor").unwrap();
    assert_eq!(direction, SplitDirection::Down);
    assert_eq!(ratio, 6.0 / 14.0);
}

#[test]
fn recovery_ratio_refuses_a_pane_too_narrow_on_the_cross_axis() {
    // Wide enough on the split axis, but only 5 rows: recreating an anchor
    // beside the run would leave both below the 8-row agent minimum.
    let error =
        recovery_target_ratio(&layout("run", 200, 5), "run", SplitDirection::Right).unwrap_err();
    let message = error.to_string();
    assert!(message.contains(ERR_ANCHOR_TOO_SMALL), "{message}");
    assert!(message.contains("too narrow"), "{message}");
}
