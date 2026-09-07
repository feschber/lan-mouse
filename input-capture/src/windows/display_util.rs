use windows::Win32::Foundation::RECT;

use input_event::screen::{CrossedEdge, Rect, crossed_exposed_edge};

use crate::Position;

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
pub(crate) fn entered_barrier(
    prev_pos: (i32, i32),
    curr_pos: (i32, i32),
    displays: &[RECT],
) -> Option<(Position, (i32, i32))> {
    crossed_exposed_edge(
        prev_pos,
        curr_pos,
        displays.iter().map(|display| Rect {
            x: display.left,
            y: display.top,
            width: display.right.saturating_sub(display.left),
            height: display.bottom.saturating_sub(display.top),
        }),
    )
    .map(|CrossedEdge { edge, point }| {
        (
            match edge {
                input_event::screen::Edge::Left => Position::Left,
                input_event::screen::Edge::Right => Position::Right,
                input_event::screen::Edge::Top => Position::Top,
                input_event::screen::Edge::Bottom => Position::Bottom,
            },
            point,
        )
    })
}
