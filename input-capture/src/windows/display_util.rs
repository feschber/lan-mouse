use windows::Win32::Foundation::RECT;

use crate::Position;

fn is_within_dp_region(point: (i32, i32), display: &RECT) -> bool {
    [
        Position::Left,
        Position::Right,
        Position::Top,
        Position::Bottom,
    ]
    .iter()
    .all(|&pos| is_within_dp_boundary(point, display, pos))
}

fn is_within_dp_boundary(point: (i32, i32), display: &RECT, pos: Position) -> bool {
    let (x, y) = point;
    match pos {
        Position::Left => display.left <= x,
        Position::Right => display.right > x,
        Position::Top => display.top <= y,
        Position::Bottom => display.bottom > y,
    }
}

/// returns whether the given position is within the display bounds with respect to the given
/// barrier position
///
/// # Arguments
///
/// * `x`:
/// * `y`:
/// * `displays`:
/// * `pos`:
///
/// returns: bool
///
fn in_bounds(point: (i32, i32), displays: &[RECT], pos: Position) -> bool {
    displays
        .iter()
        .any(|d| is_within_dp_boundary(point, d, pos))
}

fn in_display_region(point: (i32, i32), displays: &[RECT]) -> bool {
    displays.iter().any(|d| is_within_dp_region(point, d))
}

fn moved_across_boundary(
    prev_pos: (i32, i32),
    curr_pos: (i32, i32),
    displays: &[RECT],
    pos: Position,
) -> bool {
    /* was within bounds, but is not anymore */
    in_display_region(prev_pos, displays) && !in_bounds(curr_pos, displays, pos)
}

pub(crate) fn entered_barrier(
    prev_pos: (i32, i32),
    curr_pos: (i32, i32),
    displays: &[RECT],
) -> Option<Position> {
    [
        Position::Left,
        Position::Right,
        Position::Top,
        Position::Bottom,
    ]
    .into_iter()
    .find(|&pos| moved_across_boundary(prev_pos, curr_pos, displays, pos))
}

///
/// clamp point to display bounds
///
/// # Arguments
///
/// * `prev_point`: coordinates, the cursor was before entering, within bounds of a display
/// * `entry_point`: point to clamp
///
/// returns: (i32, i32), the corrected entry point
///
pub(crate) fn clamp_to_display_bounds(
    display_regions: &[RECT],
    prev_point: (i32, i32),
    point: (i32, i32),
) -> (i32, i32) {
    /* find display where movement came from */
    let display = display_regions
        .iter()
        .find(|&d| is_within_dp_region(prev_point, d))
        .unwrap();

    /* clamp to bounds (inclusive) */
    let (x, y) = point;
    let (min_x, max_x) = (display.left, display.right - 1);
    let (min_y, max_y) = (display.top, display.bottom - 1);
    (x.clamp(min_x, max_x), y.clamp(min_y, max_y))
}

/// Normalizes `coord` to `0.0..=1.0` within `min..=max`, clamping if the
/// raw crossing point briefly ended up just outside the bounds. Falls
/// back to the midpoint if the bounds are degenerate, which should not
/// happen in practice — real display RECTs always have positive size.
fn normalized_cross_axis(coord: i32, min: i32, max: i32) -> f64 {
    if max <= min {
        return 0.5;
    }
    ((coord - min) as f64 / (max - min) as f64).clamp(0.0, 1.0)
}

/// Normalized (`0.0..=1.0`) position along the edge `pos` was crossed
/// at, within the bounds of the display `prev_point` was on — the same
/// display [`clamp_to_display_bounds`] uses for this crossing. Falls
/// back to the midpoint if that display can't be determined.
pub(crate) fn cross_axis_position(
    display_regions: &[RECT],
    prev_point: (i32, i32),
    point: (i32, i32),
    pos: Position,
) -> f64 {
    let Some(display) = display_regions
        .iter()
        .find(|&d| is_within_dp_region(prev_point, d))
    else {
        return 0.5;
    };
    let (x, y) = point;
    match pos {
        Position::Left | Position::Right => normalized_cross_axis(y, display.top, display.bottom),
        Position::Top | Position::Bottom => normalized_cross_axis(x, display.left, display.right),
    }
}

#[cfg(test)]
mod cross_axis_test {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn midpoint_of_top_edge() {
        let displays = [rect(0, 0, 1000, 800)];
        let t = cross_axis_position(&displays, (500, 400), (500, -5), Position::Top);
        assert_eq!(t, 0.5);
    }

    #[test]
    fn near_left_end_of_top_edge() {
        let displays = [rect(0, 0, 1000, 800)];
        let t = cross_axis_position(&displays, (10, 400), (10, -5), Position::Top);
        assert_eq!(t, 0.01);
    }

    #[test]
    fn cross_axis_for_left_right_edges_uses_y() {
        let displays = [rect(0, 0, 1000, 800)];
        let t = cross_axis_position(&displays, (5, 200), (-5, 200), Position::Left);
        assert_eq!(t, 0.25);
    }

    #[test]
    fn uses_bounds_of_the_display_movement_came_from() {
        // two displays side by side; the crossing display is the
        // second (offset) one, not the first
        let displays = [rect(0, 0, 1000, 800), rect(1000, 0, 2000, 800)];
        let t = cross_axis_position(&displays, (1500, 400), (1500, -5), Position::Top);
        assert_eq!(t, 0.5);
    }

    #[test]
    fn falls_back_to_midpoint_when_display_not_found() {
        let displays = [rect(0, 0, 1000, 800)];
        let t = cross_axis_position(&displays, (5000, 5000), (5000, -5), Position::Top);
        assert_eq!(t, 0.5);
    }
}
