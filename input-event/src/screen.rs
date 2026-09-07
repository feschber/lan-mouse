/// A display rectangle in compositor or desktop pixel coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    fn contains_point(self, (x, y): (i32, i32)) -> bool {
        x >= self.x
            && x < self.x.saturating_add(self.width)
            && y >= self.y
            && y < self.y.saturating_add(self.height)
    }
    fn contains_cross_axis(self, edge: Edge, coordinate: i32) -> bool {
        let (start, end) = self.cross_axis_range(edge);
        coordinate >= start && coordinate < end
    }

    fn cross_axis_range(self, edge: Edge) -> (i32, i32) {
        match edge {
            Edge::Left | Edge::Right => (self.y, self.y.saturating_add(self.height)),
            Edge::Top | Edge::Bottom => (self.x, self.x.saturating_add(self.width)),
        }
    }

    fn edge_coordinate(self, edge: Edge) -> i32 {
        match edge {
            Edge::Left => self.x,
            Edge::Right => self.x.saturating_add(self.width).saturating_sub(1),
            Edge::Top => self.y,
            Edge::Bottom => self.y.saturating_add(self.height).saturating_sub(1),
        }
    }

    fn is_valid(self) -> bool {
        self.width > 0 && self.height > 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CrossedEdge {
    pub edge: Edge,
    pub point: (i32, i32),
}

pub fn crossed_exposed_edge(
    previous: (i32, i32),
    current: (i32, i32),
    rectangles: impl IntoIterator<Item = Rect>,
) -> Option<CrossedEdge> {
    let rectangles: Vec<_> = rectangles
        .into_iter()
        .filter(|rect| rect.is_valid())
        .collect();
    let source = rectangles
        .iter()
        .copied()
        .find(|rect| rect.contains_point(previous))?;
    [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom]
        .into_iter()
        .enumerate()
        .filter_map(|(order, edge)| {
            let (
                edge_coordinate,
                boundary_adjustment,
                cross_start,
                cross_end,
                previous_axis,
                previous_cross,
                axis_delta,
                cross_delta,
            ) = match edge {
                Edge::Left | Edge::Right => (
                    source.edge_coordinate(edge),
                    if matches!(edge, Edge::Left) { -1 } else { 1 },
                    source.y,
                    source.y + source.height - 1,
                    previous.0,
                    previous.1,
                    i128::from(current.0) - i128::from(previous.0),
                    i128::from(current.1) - i128::from(previous.1),
                ),
                Edge::Top | Edge::Bottom => (
                    source.edge_coordinate(edge),
                    if matches!(edge, Edge::Top) { -1 } else { 1 },
                    source.x,
                    source.x + source.width - 1,
                    previous.1,
                    previous.0,
                    i128::from(current.1) - i128::from(previous.1),
                    i128::from(current.0) - i128::from(previous.0),
                ),
            };
            if axis_delta == 0 {
                return None;
            }
            let mut t_num = i128::from(edge_coordinate) * 2 + i128::from(boundary_adjustment)
                - i128::from(previous_axis) * 2;
            let mut t_den = axis_delta * 2;
            if t_den < 0 {
                t_num = -t_num;
                t_den = -t_den;
            }
            // `t_num == t_den` (t == 1) cannot occur for i32 pixel centers:
            // physical boundaries are N ± 0.5, so t_num is always odd and
            // t_den is always even. Keep `<=` so a sample that did land on
            // the boundary would still count as a crossing.
            if !(t_num > 0 && t_num <= t_den) {
                return None;
            }
            let cross_num = i128::from(previous_cross) * t_den + t_num * cross_delta;
            if 2 * cross_num < (i128::from(cross_start) * 2 - 1) * t_den
                || 2 * cross_num > (i128::from(cross_end) * 2 + 1) * t_den
            {
                return None;
            }
            let rounded = round_ratio_away_from_zero(cross_num, t_den)?;
            endpoint_owners(cross_num, t_den)
                .unwrap_or([rounded, rounded])
                .into_iter()
                .map(|coordinate| coordinate.clamp(cross_start, cross_end))
                .find(|&cross_coordinate| {
                    let outside = match edge {
                        Edge::Left => (edge_coordinate - 1, cross_coordinate),
                        Edge::Right => (edge_coordinate + 1, cross_coordinate),
                        Edge::Top => (cross_coordinate, edge_coordinate - 1),
                        Edge::Bottom => (cross_coordinate, edge_coordinate + 1),
                    };
                    !rectangles.iter().any(|rect| rect.contains_point(outside))
                })
                .map(|cross_coordinate| {
                    (
                        t_num,
                        t_den,
                        order,
                        CrossedEdge {
                            edge,
                            point: match edge {
                                Edge::Left | Edge::Right => (edge_coordinate, cross_coordinate),
                                Edge::Top | Edge::Bottom => (cross_coordinate, edge_coordinate),
                            },
                        },
                    )
                })
        })
        .min_by(|left, right| {
            (left.0 * right.1)
                .cmp(&(right.0 * left.1))
                .then(left.2.cmp(&right.2))
        })
        .map(|(_, _, _, crossed)| crossed)
}

/// A physical desktop edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

fn round_ratio_away_from_zero(numerator: i128, denominator: i128) -> Option<i32> {
    if denominator <= 0 {
        return None;
    }
    let floor = numerator.div_euclid(denominator);
    let remainder = numerator.rem_euclid(denominator);
    let rounded = match (remainder * 2).cmp(&denominator) {
        std::cmp::Ordering::Less => floor,
        std::cmp::Ordering::Greater => floor.checked_add(1)?,
        std::cmp::Ordering::Equal if numerator < 0 => floor,
        std::cmp::Ordering::Equal => floor.checked_add(1)?,
    };
    i32::try_from(rounded).ok()
}

fn endpoint_owners(cross_num: i128, cross_den: i128) -> Option<[i32; 2]> {
    if cross_den <= 0 || (cross_num * 2) % cross_den != 0 {
        return None;
    }
    let doubled_cross = cross_num * 2 / cross_den;
    if doubled_cross % 2 == 0 {
        return None;
    }
    let floor = i32::try_from(doubled_cross.div_euclid(2)).ok()?;
    let ceiling = floor.checked_add(1)?;
    Some(if doubled_cross > 0 {
        [ceiling, floor]
    } else {
        [floor, ceiling]
    })
}

/// A contiguous, usable portion of one desktop edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EdgeSegment {
    pub edge_coordinate: i32,
    pub cross_start: i32,
    pub cross_end: i32,
}

impl EdgeSegment {
    fn len(self) -> u32 {
        self.cross_end.saturating_sub(self.cross_start) as u32
    }

    fn contains(self, edge_coordinate: i32, cross_coordinate: i32) -> bool {
        self.edge_coordinate == edge_coordinate
            && cross_coordinate >= self.cross_start
            && cross_coordinate < self.cross_end
    }
}

/// The exposed outer edge of a non-rectangular desktop.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EdgeSegments(Vec<EdgeSegment>);

impl EdgeSegments {
    pub fn from_segments(segments: impl IntoIterator<Item = EdgeSegment>) -> Self {
        let mut segments: Vec<_> = segments
            .into_iter()
            .filter(|segment| segment.cross_start < segment.cross_end)
            .collect();
        segments.sort_unstable_by_key(|segment| (segment.cross_start, segment.edge_coordinate));
        Self(segments)
    }

    /// Finds only the physical portions that face outside the desktop. Gaps are
    /// intentionally omitted from the normalized coordinate space.
    pub fn from_rectangles(edge: Edge, rectangles: impl IntoIterator<Item = Rect>) -> Self {
        let rectangles: Vec<_> = rectangles
            .into_iter()
            .filter(|rect| rect.is_valid())
            .collect();
        let mut boundaries: Vec<_> = rectangles
            .iter()
            .flat_map(|rect| {
                let (start, end) = rect.cross_axis_range(edge);
                [start, end]
            })
            .collect();
        boundaries.sort_unstable();
        boundaries.dedup();

        let mut segments: Vec<EdgeSegment> = Vec::new();
        for window in boundaries.windows(2) {
            let (start, end) = (window[0], window[1]);
            if start == end {
                continue;
            }
            let exposed = rectangles
                .iter()
                .filter(|rect| rect.contains_cross_axis(edge, start))
                .map(|rect| rect.edge_coordinate(edge))
                .reduce(|current, candidate| match edge {
                    Edge::Left | Edge::Top => current.min(candidate),
                    Edge::Right | Edge::Bottom => current.max(candidate),
                });
            let Some(edge_coordinate) = exposed else {
                continue;
            };
            if let Some(previous) = segments.last_mut() {
                if previous.edge_coordinate == edge_coordinate && previous.cross_end == start {
                    previous.cross_end = end;
                    continue;
                }
            }
            {
                segments.push(EdgeSegment {
                    edge_coordinate,
                    cross_start: start,
                    cross_end: end,
                });
            }
        }
        Self::from_segments(segments)
    }

    pub fn normalize(&self, edge_coordinate: i32, cross_coordinate: i32) -> Option<f32> {
        let total = self.0.iter().map(|segment| segment.len()).sum::<u32>();
        if total == 0 {
            return None;
        }
        let mut offset = 0u32;
        for segment in &self.0 {
            if segment.contains(edge_coordinate, cross_coordinate) {
                offset += (cross_coordinate - segment.cross_start) as u32;
                return Some(if total == 1 {
                    0.0
                } else {
                    offset as f32 / (total - 1) as f32
                });
            }
            offset += segment.len();
        }
        None
    }

    pub fn denormalize(&self, normalized: f32) -> Option<(i32, i32)> {
        if !normalized.is_finite() {
            return None;
        }
        let total = self.0.iter().map(|segment| segment.len()).sum::<u32>();
        if total == 0 {
            return None;
        }
        let target = if total == 1 {
            0
        } else {
            (normalized.clamp(0.0, 1.0) * (total - 1) as f32).round() as u32
        };
        let mut offset = 0u32;
        for segment in &self.0 {
            let end = offset + segment.len();
            if target < end {
                return Some((
                    segment.edge_coordinate,
                    segment.cross_start + (target - offset) as i32,
                ));
            }
            offset = end;
        }
        None
    }

    pub fn segments(&self) -> impl Iterator<Item = EdgeSegment> + '_ {
        self.0.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::{CrossedEdge, Edge, EdgeSegments, Rect, crossed_exposed_edge};
    use std::collections::HashSet;

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn single_monitor_preserves_every_pixel() {
        let segments = EdgeSegments::from_rectangles(Edge::Left, [rect(0, 0, 1920, 1080)]);
        assert_eq!(segments.normalize(0, 0), Some(0.0));
        assert_eq!(segments.normalize(0, 1079), Some(1.0));
        assert_eq!(segments.denormalize(0.5), Some((0, 540)));
    }

    #[test]
    fn stacked_monitors_share_one_continuous_edge() {
        let segments = EdgeSegments::from_rectangles(
            Edge::Left,
            [rect(0, 0, 1920, 1080), rect(0, 1080, 1920, 1080)],
        );
        assert_eq!(segments.normalize(0, 1620), Some(1620.0 / 2159.0));
        assert_eq!(segments.denormalize(1620.0 / 2159.0), Some((0, 1620)));
    }

    #[test]
    fn side_by_side_monitors_keep_only_outer_edges() {
        let segments = EdgeSegments::from_rectangles(
            Edge::Left,
            [rect(0, 0, 1920, 1080), rect(1920, 0, 1920, 1080)],
        );
        assert_eq!(segments.normalize(0, 540), Some(540.0 / 1079.0));
        assert_eq!(segments.normalize(1920, 540), None);
    }

    #[test]
    fn different_sizes_contribute_only_the_exposed_portions() {
        let segments = EdgeSegments::from_rectangles(
            Edge::Right,
            [rect(0, 0, 1920, 1080), rect(1920, 0, 2560, 1440)],
        );
        assert_eq!(segments.normalize(4479, 1439), Some(1.0));
    }

    #[test]
    fn gaps_are_compacted_out_of_the_edge_coordinate_space() {
        let segments = EdgeSegments::from_rectangles(
            Edge::Left,
            [rect(0, 0, 1920, 100), rect(0, 200, 1920, 100)],
        );
        assert_eq!(segments.normalize(0, 99), Some(99.0 / 199.0));
        assert_eq!(segments.denormalize(100.0 / 199.0), Some((0, 200)));
    }

    #[test]
    fn stepped_edge_uses_each_outer_segment() {
        let segments = EdgeSegments::from_rectangles(
            Edge::Left,
            [rect(100, 0, 1920, 100), rect(0, 100, 1920, 100)],
        );
        assert_eq!(segments.normalize(100, 50), Some(50.0 / 199.0));
        assert_eq!(segments.normalize(0, 150), Some(150.0 / 199.0));
    }

    #[test]
    fn normalization_and_denormalization_are_symmetric_for_every_pixel() {
        let segments =
            EdgeSegments::from_rectangles(Edge::Left, [rect(0, 0, 1920, 3), rect(0, 5, 1920, 2)]);
        for coordinate in [0, 1, 2, 5, 6] {
            let normalized = segments.normalize(0, coordinate).unwrap();
            assert_eq!(segments.denormalize(normalized), Some((0, coordinate)));
        }
    }

    #[test]
    fn crossed_edge_uses_only_exposed_segments() {
        let rectangles = [rect(0, 0, 100, 100), rect(100, 100, 100, 100)];
        assert_eq!(
            crossed_exposed_edge((99, 50), (100, 50), rectangles).map(|crossed| crossed.edge),
            Some(Edge::Right)
        );
        let overlapping = [rect(0, 0, 100, 100), rect(100, 50, 100, 100)];
        assert_eq!(crossed_exposed_edge((99, 75), (100, 75), overlapping), None);
    }

    #[test]
    fn crossed_edge_rejects_internal_and_gap_transitions() {
        let aligned = [rect(0, 0, 100, 100), rect(100, 0, 100, 100)];
        assert_eq!(crossed_exposed_edge((99, 50), (100, 50), aligned), None);

        let gap = [rect(0, 0, 100, 100), rect(200, 0, 100, 100)];
        assert_eq!(
            crossed_exposed_edge((99, 50), (100, 50), gap).map(|crossed| crossed.edge),
            Some(Edge::Right)
        );
    }

    #[test]
    fn crossed_edge_uses_the_first_diagonal_intersection() {
        let rectangles = [rect(0, 0, 100, 100)];
        assert_eq!(
            crossed_exposed_edge((50, 50), (150, -100), rectangles),
            Some(CrossedEdge {
                edge: Edge::Top,
                point: (84, 0)
            })
        );
        assert_eq!(
            crossed_exposed_edge((50, 50), (150, -25), rectangles),
            Some(CrossedEdge {
                edge: Edge::Right,
                point: (99, 13)
            })
        );
    }

    #[test]
    fn crossed_edge_resolves_every_exact_corner_deterministically() {
        let rectangles = [rect(0, 0, 100, 100)];
        for (previous, current) in [
            ((99, 0), (100, -1)),
            ((0, 0), (-1, -1)),
            ((99, 99), (100, 100)),
            ((0, 99), (-1, 100)),
        ] {
            let crossed = crossed_exposed_edge(previous, current, rectangles).unwrap();
            assert!(crossed.point.0 >= 0 && crossed.point.0 < 100);
            assert!(crossed.point.1 >= 0 && crossed.point.1 < 100);
        }
    }

    #[test]
    fn crossed_edge_chooses_an_exposed_endpoint_owner() {
        let positive = [rect(0, 0, 3, 4), rect(3, 2, 2, 3)];
        assert_eq!(
            crossed_exposed_edge((0, 0), (5, 3), positive),
            Some(CrossedEdge {
                edge: Edge::Right,
                point: (2, 1),
            })
        );

        let negative = [rect(0, -4, 3, 4), rect(3, -3, 2, 1)];
        assert_eq!(
            crossed_exposed_edge((0, -4), (5, -1), negative),
            Some(CrossedEdge {
                edge: Edge::Right,
                point: (2, -2),
            })
        );
    }

    #[test]
    fn crossed_edge_handles_endpoint_after_inexact_division() {
        let rectangles = [rect(0, 0, 8, 12), rect(8, 7, 1, 1)];

        assert_eq!(
            crossed_exposed_edge((0, 0), (11, 11), rectangles),
            Some(CrossedEdge {
                edge: Edge::Right,
                point: (7, 8),
            })
        );

        let negative = [rect(0, -12, 8, 12), rect(8, -5, 1, 1)];
        assert_eq!(
            crossed_exposed_edge((0, -12), (11, -1), negative),
            Some(CrossedEdge {
                edge: Edge::Right,
                point: (7, -4),
            })
        );
    }

    #[test]
    fn crossed_edge_handles_inexact_exact_corner() {
        assert_eq!(
            crossed_exposed_edge((0, 3), (25, -22), [rect(0, 0, 4, 4)]),
            Some(CrossedEdge {
                edge: Edge::Right,
                point: (3, 0),
            })
        );
    }

    type CornerCase = (Rect, (i32, i32), (i32, i32), CrossedEdge);

    fn nontrivial_corner_cases() -> [CornerCase; 4] {
        [
            (
                rect(0, 0, 1, 2),
                (0, 1),
                (7, -20),
                CrossedEdge {
                    edge: Edge::Right,
                    point: (0, 0),
                },
            ),
            (
                rect(0, 0, 2, 2),
                (1, 1),
                (-10, -10),
                CrossedEdge {
                    edge: Edge::Left,
                    point: (0, 0),
                },
            ),
            (
                rect(0, 0, 2, 2),
                (0, 0),
                (13, 13),
                CrossedEdge {
                    edge: Edge::Right,
                    point: (1, 1),
                },
            ),
            (
                rect(0, 0, 2, 2),
                (1, 0),
                (-24, 25),
                CrossedEdge {
                    edge: Edge::Left,
                    point: (0, 1),
                },
            ),
        ]
    }

    #[test]
    fn crossed_edge_handles_all_corners_with_nontrivial_divisions() {
        for (display, previous, current, expected) in nontrivial_corner_cases() {
            assert_eq!(
                crossed_exposed_edge(previous, current, [display]),
                Some(expected),
                "display={display:?}, previous={previous:?}, current={current:?}"
            );
        }
    }

    #[test]
    fn crossed_edge_corner_crossings_are_translation_invariant() {
        for offset in [(10_000, 10_000), (-10_000, -10_000), (30_000, -20_000)] {
            for (display, previous, current, expected) in nontrivial_corner_cases() {
                let translated = Rect {
                    x: display.x + offset.0,
                    y: display.y + offset.1,
                    ..display
                };
                assert_eq!(
                    crossed_exposed_edge(
                        (previous.0 + offset.0, previous.1 + offset.1),
                        (current.0 + offset.0, current.1 + offset.1),
                        [translated],
                    ),
                    Some(CrossedEdge {
                        edge: expected.edge,
                        point: (expected.point.0 + offset.0, expected.point.1 + offset.1),
                    }),
                    "offset={offset:?}, display={display:?}, previous={previous:?}, current={current:?}"
                );
            }
        }
    }

    #[test]
    fn crossed_edge_handles_very_shallow_and_steep_diagonals() {
        let display = rect(0, 0, 10, 10);
        assert_eq!(
            crossed_exposed_edge((5, 5), (1_005, 6), [display]),
            Some(CrossedEdge {
                edge: Edge::Right,
                point: (9, 5),
            })
        );
        assert_eq!(
            crossed_exposed_edge((5, 5), (6, 1_005), [display]),
            Some(CrossedEdge {
                edge: Edge::Bottom,
                point: (5, 9),
            })
        );
    }

    #[test]
    fn crossed_edge_handles_exact_endpoints_for_positive_and_negative_coordinates() {
        for denominator in [7, 11, 13, 25] {
            assert_eq!(
                crossed_exposed_edge(
                    (0, 0),
                    (denominator, denominator),
                    [rect(0, 0, 1, 2), rect(1, 1, 1, 1)],
                ),
                Some(CrossedEdge {
                    edge: Edge::Right,
                    point: (0, 0),
                }),
                "positive denominator={denominator}"
            );
            assert_eq!(
                crossed_exposed_edge(
                    (0, -2),
                    (denominator, denominator - 2),
                    [rect(0, -2, 1, 2), rect(1, -2, 1, 1)],
                ),
                Some(CrossedEdge {
                    edge: Edge::Right,
                    point: (0, -1),
                }),
                "negative denominator={denominator}"
            );
        }
    }

    #[test]
    fn crossed_edge_endpoint_ownership_is_translation_invariant() {
        let rectangles = [rect(0, 0, 8, 12), rect(8, 7, 1, 1)];
        for offset in [(10_000, 10_000), (-10_000, -10_000), (30_000, -20_000)] {
            let translated = rectangles.map(|display| Rect {
                x: display.x + offset.0,
                y: display.y + offset.1,
                ..display
            });
            assert_eq!(
                crossed_exposed_edge(
                    (offset.0, offset.1),
                    (offset.0 + 11, offset.1 + 11),
                    translated
                ),
                Some(CrossedEdge {
                    edge: Edge::Right,
                    point: (offset.0 + 7, offset.1 + 8),
                }),
                "offset={offset:?}"
            );
        }
    }

    fn contains(rect: Rect, point: (i32, i32)) -> bool {
        point.0 >= rect.x
            && point.0 < rect.x + rect.width
            && point.1 >= rect.y
            && point.1 < rect.y + rect.height
    }

    #[derive(Clone, Copy)]
    struct ReferenceEdgeSegment {
        edge: Edge,
        pixel: (i32, i32),
    }

    fn occupied_pixels(rectangles: &[Rect]) -> HashSet<(i32, i32)> {
        rectangles
            .iter()
            .flat_map(|rect| {
                (rect.x..rect.x + rect.width)
                    .flat_map(move |x| (rect.y..rect.y + rect.height).map(move |y| (x, y)))
            })
            .collect()
    }

    fn reference_segments(
        source: Rect,
        occupied: &HashSet<(i32, i32)>,
    ) -> Vec<ReferenceEdgeSegment> {
        let mut segments = Vec::new();
        for x in source.x..source.x + source.width {
            for y in source.y..source.y + source.height {
                for (edge, neighbor) in [
                    (Edge::Left, (x - 1, y)),
                    (Edge::Right, (x + 1, y)),
                    (Edge::Top, (x, y - 1)),
                    (Edge::Bottom, (x, y + 1)),
                ] {
                    if !occupied.contains(&neighbor) {
                        segments.push(ReferenceEdgeSegment {
                            edge,
                            pixel: (x, y),
                        });
                    }
                }
            }
        }
        segments
    }

    fn reference_crossed_edge(
        previous: (i32, i32),
        current: (i32, i32),
        rectangles: &[Rect],
    ) -> Option<CrossedEdge> {
        let source = rectangles
            .iter()
            .copied()
            .find(|rect| contains(*rect, previous))?;
        let occupied = occupied_pixels(rectangles);
        reference_segments(source, &occupied)
            .into_iter()
            .filter_map(|segment| {
                let (
                    boundary2,
                    previous_axis,
                    previous_cross,
                    axis_delta,
                    cross_delta,
                    low2,
                    high2,
                    pixel_cross,
                ) = match segment.edge {
                    Edge::Left => (
                        i128::from(segment.pixel.0) * 2 - 1,
                        previous.0,
                        previous.1,
                        i128::from(current.0) - i128::from(previous.0),
                        i128::from(current.1) - i128::from(previous.1),
                        i128::from(segment.pixel.1) * 2 - 1,
                        i128::from(segment.pixel.1) * 2 + 1,
                        segment.pixel.1,
                    ),
                    Edge::Right => (
                        i128::from(segment.pixel.0) * 2 + 1,
                        previous.0,
                        previous.1,
                        i128::from(current.0) - i128::from(previous.0),
                        i128::from(current.1) - i128::from(previous.1),
                        i128::from(segment.pixel.1) * 2 - 1,
                        i128::from(segment.pixel.1) * 2 + 1,
                        segment.pixel.1,
                    ),
                    Edge::Top => (
                        i128::from(segment.pixel.1) * 2 - 1,
                        previous.1,
                        previous.0,
                        i128::from(current.1) - i128::from(previous.1),
                        i128::from(current.0) - i128::from(previous.0),
                        i128::from(segment.pixel.0) * 2 - 1,
                        i128::from(segment.pixel.0) * 2 + 1,
                        segment.pixel.0,
                    ),
                    Edge::Bottom => (
                        i128::from(segment.pixel.1) * 2 + 1,
                        previous.1,
                        previous.0,
                        i128::from(current.1) - i128::from(previous.1),
                        i128::from(current.0) - i128::from(previous.0),
                        i128::from(segment.pixel.0) * 2 - 1,
                        i128::from(segment.pixel.0) * 2 + 1,
                        segment.pixel.0,
                    ),
                };
                if axis_delta == 0 {
                    return None;
                }
                let mut t_num = boundary2 - i128::from(previous_axis) * 2;
                let mut t_den = axis_delta * 2;
                if t_den < 0 {
                    t_num = -t_num;
                    t_den = -t_den;
                }
                if !(t_num > 0 && t_num <= t_den) {
                    return None;
                }
                let cross_num = i128::from(previous_cross) * t_den + t_num * cross_delta;
                if 2 * cross_num < low2 * t_den || 2 * cross_num > high2 * t_den {
                    return None;
                }
                let order = match segment.edge {
                    Edge::Left => 0,
                    Edge::Right => 1,
                    Edge::Top => 2,
                    Edge::Bottom => 3,
                };
                let owner_rank = usize::from(
                    pixel_cross != reference_round_ratio_away_from_zero(cross_num, t_den)?,
                );
                Some((
                    t_num,
                    t_den,
                    order,
                    owner_rank,
                    CrossedEdge {
                        edge: segment.edge,
                        point: segment.pixel,
                    },
                ))
            })
            .min_by(|left, right| {
                (left.0 * right.1)
                    .cmp(&(right.0 * left.1))
                    .then(left.2.cmp(&right.2))
                    .then(left.3.cmp(&right.3))
            })
            .map(|(_, _, _, _, crossed)| crossed)
    }

    fn reference_round_ratio_away_from_zero(numerator: i128, denominator: i128) -> Option<i32> {
        if denominator <= 0 {
            return None;
        }
        let floor = numerator.div_euclid(denominator);
        let remainder = numerator.rem_euclid(denominator);
        let rounded = match (remainder * 2).cmp(&denominator) {
            std::cmp::Ordering::Less => floor,
            std::cmp::Ordering::Greater => floor.checked_add(1)?,
            std::cmp::Ordering::Equal if numerator < 0 => floor,
            std::cmp::Ordering::Equal => floor.checked_add(1)?,
        };
        i32::try_from(rounded).ok()
    }

    fn verification_layouts() -> Vec<Vec<Rect>> {
        vec![
            vec![rect(0, 0, 1, 1)],
            vec![rect(-2, -1, 3, 4)],
            vec![rect(0, 0, 3, 3), rect(3, 0, 2, 4)],
            vec![rect(0, 0, 3, 3), rect(0, 3, 4, 2)],
            vec![rect(0, 0, 3, 4), rect(3, 2, 2, 3)],
            vec![rect(0, 0, 3, 3), rect(5, 0, 2, 2)],
            vec![rect(-2, -2, 4, 2), rect(2, 0, 3, 4)],
            vec![rect(0, 0, 3, 3), rect(3, 3, 2, 2)],
            vec![rect(0, 0, 4, 4), rect(2, 1, 3, 2)],
            vec![rect(0, 0, 2, 3), rect(2, 0, 2, 3), rect(4, 0, 2, 3)],
            vec![rect(0, 0, 3, 2), rect(0, 2, 3, 2), rect(0, 4, 3, 2)],
            vec![rect(0, 0, 2, 2), rect(2, 1, 2, 2), rect(4, 2, 2, 2)],
            vec![rect(0, 1, 2, 2), rect(2, 0, 2, 4), rect(4, 1, 2, 2)],
            vec![rect(-3, 0, 2, 2), rect(-1, 0, 3, 3), rect(2, 1, 2, 2)],
        ]
    }

    fn generated_two_monitor_layouts() -> Vec<Vec<Rect>> {
        const OFFSETS: [i32; 4] = [-2, 0, 2, 4];

        let mut layouts = Vec::new();
        for primary_width in 1..=3 {
            for primary_height in 1..=3 {
                let primary = rect(0, 0, primary_width, primary_height);
                for secondary_width in 1..=3 {
                    for secondary_height in 1..=3 {
                        for offset_x in OFFSETS {
                            for offset_y in OFFSETS {
                                layouts.push(vec![
                                    primary,
                                    rect(offset_x, offset_y, secondary_width, secondary_height),
                                ]);
                            }
                        }
                    }
                }
            }
        }
        layouts
    }

    fn desktop_bounds(rectangles: &[Rect], padding: i32) -> (i32, i32, i32, i32) {
        (
            rectangles.iter().map(|rect| rect.x).min().unwrap() - padding,
            rectangles
                .iter()
                .map(|rect| rect.x + rect.width)
                .max()
                .unwrap()
                + padding,
            rectangles.iter().map(|rect| rect.y).min().unwrap() - padding,
            rectangles
                .iter()
                .map(|rect| rect.y + rect.height)
                .max()
                .unwrap()
                + padding,
        )
    }

    #[test]
    fn crossing_matches_the_cell_oracle_for_small_layouts() {
        let layouts = verification_layouts();
        for rectangles in &layouts {
            let min_x = rectangles.iter().map(|rect| rect.x).min().unwrap() - 3;
            let max_x = rectangles
                .iter()
                .map(|rect| rect.x + rect.width)
                .max()
                .unwrap()
                + 3;
            let min_y = rectangles.iter().map(|rect| rect.y).min().unwrap() - 3;
            let max_y = rectangles
                .iter()
                .map(|rect| rect.y + rect.height)
                .max()
                .unwrap()
                + 3;
            for previous in (min_x..max_x).flat_map(|x| (min_y..max_y).map(move |y| (x, y))) {
                if !rectangles.iter().any(|rect| contains(*rect, previous)) {
                    continue;
                }
                for current in (min_x..max_x).flat_map(|x| (min_y..max_y).map(move |y| (x, y))) {
                    let production = crossed_exposed_edge(previous, current, rectangles.clone());
                    let oracle = reference_crossed_edge(previous, current, rectangles);
                    assert_eq!(
                        production, oracle,
                        "rectangles={rectangles:?}, previous={previous:?}, current={current:?}, production={production:?}, oracle={oracle:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn generated_crossings_match_the_cell_oracle() {
        let layouts = generated_two_monitor_layouts();
        for rectangles in &layouts {
            let (min_x, max_x, min_y, max_y) = desktop_bounds(rectangles, 1);
            for previous in (min_x..max_x).flat_map(|x| (min_y..max_y).map(move |y| (x, y))) {
                if !rectangles.iter().any(|rect| contains(*rect, previous)) {
                    continue;
                }
                for current in (min_x..max_x).flat_map(|x| (min_y..max_y).map(move |y| (x, y))) {
                    let production = crossed_exposed_edge(previous, current, rectangles.clone());
                    let oracle = reference_crossed_edge(previous, current, rectangles);
                    assert_eq!(
                        production, oracle,
                        "rectangles={rectangles:?}, previous={previous:?}, current={current:?}, production={production:?}, oracle={oracle:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn normalization_round_trips_every_exposed_pixel_and_ignores_rectangle_order() {
        for rectangles in verification_layouts() {
            let mut reversed = rectangles.clone();
            reversed.reverse();
            for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
                let segments = EdgeSegments::from_rectangles(edge, rectangles.clone());
                let reversed_segments = EdgeSegments::from_rectangles(edge, reversed.clone());
                assert_eq!(
                    segments, reversed_segments,
                    "rectangles={rectangles:?}, edge={edge:?}"
                );
                let pixels = segments
                    .segments()
                    .flat_map(|segment| {
                        (segment.cross_start..segment.cross_end)
                            .map(move |cross| (segment.edge_coordinate, cross))
                    })
                    .collect::<Vec<_>>();
                for (index, pixel) in pixels.iter().copied().enumerate() {
                    let normalized = segments.normalize(pixel.0, pixel.1).unwrap();
                    assert!((0.0..=1.0).contains(&normalized));
                    assert_eq!(segments.denormalize(normalized), Some(pixel));
                    if pixels.len() > 1 && index == 0 {
                        assert_eq!(normalized, 0.0);
                    }
                    if pixels.len() > 1 && index + 1 == pixels.len() {
                        assert_eq!(normalized, 1.0);
                    }
                }
            }
        }
    }

    #[test]
    fn edge_segments_match_the_outer_pixels_of_the_grid() {
        for rectangles in verification_layouts()
            .into_iter()
            .chain(generated_two_monitor_layouts())
        {
            let occupied = occupied_pixels(&rectangles);
            for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
                let mut expected = Vec::new();
                let cross_values = match edge {
                    Edge::Left | Edge::Right => {
                        occupied.iter().map(|(_, y)| *y).collect::<HashSet<_>>()
                    }
                    Edge::Top | Edge::Bottom => {
                        occupied.iter().map(|(x, _)| *x).collect::<HashSet<_>>()
                    }
                };
                for cross in cross_values {
                    let edge_coordinate = match edge {
                        Edge::Left => occupied
                            .iter()
                            .filter(|(_, y)| *y == cross)
                            .map(|(x, _)| *x)
                            .min(),
                        Edge::Right => occupied
                            .iter()
                            .filter(|(_, y)| *y == cross)
                            .map(|(x, _)| *x)
                            .max(),
                        Edge::Top => occupied
                            .iter()
                            .filter(|(x, _)| *x == cross)
                            .map(|(_, y)| *y)
                            .min(),
                        Edge::Bottom => occupied
                            .iter()
                            .filter(|(x, _)| *x == cross)
                            .map(|(_, y)| *y)
                            .max(),
                    }
                    .unwrap();
                    expected.push((edge_coordinate, cross));
                }
                expected.sort_unstable_by_key(|(_, cross)| *cross);
                let actual = EdgeSegments::from_rectangles(edge, rectangles.clone())
                    .segments()
                    .flat_map(|segment| {
                        (segment.cross_start..segment.cross_end)
                            .map(move |cross| (segment.edge_coordinate, cross))
                    })
                    .collect::<Vec<_>>();
                assert_eq!(actual, expected, "rectangles={rectangles:?}, edge={edge:?}");
            }
        }
    }

    #[test]
    fn crossing_is_invariant_under_rectangle_permutations_when_source_is_unique() {
        for rectangles in verification_layouts()
            .into_iter()
            .filter(|layout| layout.len() >= 2)
        {
            let mut orders = vec![rectangles.clone()];
            let mut reversed = rectangles.clone();
            reversed.reverse();
            orders.push(reversed);
            if rectangles.len() == 3 {
                for order in [[0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
                    orders.push(order.into_iter().map(|index| rectangles[index]).collect());
                }
            }
            let min_x = rectangles.iter().map(|rect| rect.x).min().unwrap() - 1;
            let max_x = rectangles
                .iter()
                .map(|rect| rect.x + rect.width)
                .max()
                .unwrap()
                + 1;
            let min_y = rectangles.iter().map(|rect| rect.y).min().unwrap() - 1;
            let max_y = rectangles
                .iter()
                .map(|rect| rect.y + rect.height)
                .max()
                .unwrap()
                + 1;
            for previous in (min_x..max_x).flat_map(|x| (min_y..max_y).map(move |y| (x, y))) {
                if rectangles
                    .iter()
                    .filter(|rect| contains(**rect, previous))
                    .count()
                    != 1
                {
                    continue;
                }
                for current in (min_x..max_x).flat_map(|x| (min_y..max_y).map(move |y| (x, y))) {
                    let expected = crossed_exposed_edge(previous, current, rectangles.clone());
                    for order in &orders {
                        assert_eq!(
                            crossed_exposed_edge(previous, current, order.clone()),
                            expected,
                            "rectangles={rectangles:?}, previous={previous:?}, current={current:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn generated_crossings_are_invariant_under_rectangle_order_when_source_is_unique() {
        let layouts = generated_two_monitor_layouts();
        for rectangles in &layouts {
            let mut reversed = rectangles.clone();
            reversed.reverse();
            let (min_x, max_x, min_y, max_y) = desktop_bounds(rectangles, 1);
            for previous in (min_x..max_x).flat_map(|x| (min_y..max_y).map(move |y| (x, y))) {
                if rectangles
                    .iter()
                    .filter(|rect| contains(**rect, previous))
                    .count()
                    != 1
                {
                    continue;
                }
                for current in (min_x..max_x).flat_map(|x| (min_y..max_y).map(move |y| (x, y))) {
                    let expected = crossed_exposed_edge(previous, current, rectangles.clone());
                    assert_eq!(
                        crossed_exposed_edge(previous, current, reversed.clone()),
                        expected,
                        "rectangles={rectangles:?}, previous={previous:?}, current={current:?}"
                    );
                }
            }
        }
    }
}
