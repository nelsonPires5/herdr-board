use super::*;

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
