use std::ops::{Add, Mul, Sub};
use crate::client::Position;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Point<T> {
    pub x: T,
    pub y: T,
}

impl<T: Sub<Output = T>> Sub for Point<T> {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self::Output {
        let x: T = self.x - rhs.x;
        let y: T = self.y - rhs.y;
        Self { x, y }
    }
}

impl<T: Copy + Eq + Ord + Sub<Output = T> + Mul<Output = T> + Add<Output = T>> Point<T> {

    pub fn display_in_bounds<'a>(&self, displays: &'a[Display<T>]) -> Option<&'a Display<T>> {
        displays.iter().find(|&d| self.in_display_bounds(d))
    }

    pub fn in_display_bounds(&self, display: &Display<T>) -> bool {
        self.clamp_to_display(display) == *self
    }

    pub fn clamp_to_display(&self, display: &Display<T>) -> Self {
        let x = self.x.clamp(display.left, display.right);
        let y = self.y.clamp(display.top, display.bottom);
        Self { x, y }
    }

    /// Calculates the direction of maximum change between this point and the point given by other
    ///
    /// # Arguments
    ///
    /// * `other`: the point to calculate the distance
    ///
    /// returns: Position -> The direction in which the distance is largest
    ///
    /// # Examples
    ///
    /// ```
    /// use lan_mouse::client::Position;
    /// use lan_mouse::display_util::Point;
    /// let a = Point { x: 0, y: 0 };
    /// let b = Point { x: 1, y: 2 };
    /// assert_eq!(a.direction_of_maximum_change(b), Position::Bottom)
    /// ```
    ///
    /// ```
    /// use lan_mouse::client::Position;
    /// use lan_mouse::display_util::Point;
    /// let a = Point { x: 0, y: 0 };
    /// let b = Point { x: 1, y: -2 };
    /// assert_eq!(a.direction_of_maximum_change(b), Position::Top)
    /// ```
    /// ```
    /// use lan_mouse::client::Position;
    /// use lan_mouse::display_util::Point;
    /// let a = Point { x: 0, y: 0 };
    /// let b = Point { x: -2, y: -1 };
    /// assert_eq!(a.direction_of_maximum_change(b), Position::Left)
    /// ```
    /// ```
    /// use lan_mouse::client::Position;
    /// use lan_mouse::display_util::Point;
    /// let a = Point { x: 0, y: 0 };
    /// let b = Point { x: 2, y: -1 };
    /// assert_eq!(a.direction_of_maximum_change(b), Position::Right)
    /// ```
    pub fn direction_of_maximum_change(self, other: Self) -> Position {
        let distances = [
            (Position::Left, self.x - other.x),
            (Position::Right, other.x - self.x),
            (Position::Top, self.y - other.y),
            (Position::Bottom, other.y - self.y),
        ];
        distances.into_iter().max_by_key(|(_, d)| *d).map(|(p, _)| p).expect("no position")
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Display<T> {
    pub left: T,
    pub right: T,
    pub top: T,
    pub bottom: T,
}

#[derive(Clone, Copy)]
pub struct DirectedLine<T> {
    pub start: Point<T>,
    pub end: Point<T>,
}

impl<T: Copy + Eq + Ord + Sub<Output = T> + Mul<Output = T> + Add<Output = T>> DirectedLine<T> {
    pub fn crossed_display_bounds<'a>(&self, displays: &'a [Display<T>]) -> Option<(&'a Display<T>, Position)> {
        // was in bounds
        let Some(display) = self.start.display_in_bounds(displays) else {
            return None;
        };
        // still in bounds
        if self.end.display_in_bounds(displays).is_some() {
            return None;
        }
        // was in bounds of `display`, now out of bounds
        let clamped = self.end.clamp_to_display(&display);
        let dir = clamped.direction_of_maximum_change(self.end);
        Some((display, dir))
    }
}
