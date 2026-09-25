/// Accumulates high-resolution scroll deltas into whole 120-unit steps.
///
/// A classic mouse wheel emits exactly +/-120 per detent, and several emulation
/// backends assume that: they divide by 120, or hand the raw value to a receiver
/// that only acts on whole steps. High-resolution wheels break the assumption --
/// a Logitech MX Master reports fractions of a detent (16, 24, 32, ...), so every
/// event truncates toward zero and scrolling silently does nothing at all.
///
/// Accumulating the remainder restores one emitted step per 120 units received,
/// which is exactly one detent's worth of scrolling.
#[derive(Default, Debug)]
pub(crate) struct Scroll120Accumulator {
    vertical: i32,
    horizontal: i32,
}

impl Scroll120Accumulator {
    /// Add `value` to the running total for `axis` (0 = vertical, else
    /// horizontal) and return the number of whole 120-unit steps that should be
    /// emitted now, keeping the sub-step remainder for the next event.
    ///
    /// Truncation is toward zero, so the remainder always keeps the sign of the
    /// accumulated scroll and direction changes cancel out rather than latch.
    pub(crate) fn accumulate(&mut self, axis: u8, value: i32) -> i32 {
        let acc = if axis == 0 {
            &mut self.vertical
        } else {
            &mut self.horizontal
        };
        *acc += value;
        let steps = *acc / 120;
        *acc -= steps * 120;
        steps
    }
}

#[cfg(test)]
mod test {
    use super::Scroll120Accumulator;

    #[test]
    fn whole_steps_pass_straight_through() {
        let mut a = Scroll120Accumulator::default();
        assert_eq!(a.accumulate(0, 120), 1);
        assert_eq!(a.accumulate(0, -120), -1);
    }

    #[test]
    fn high_resolution_fractions_accumulate_instead_of_vanishing() {
        let mut a = Scroll120Accumulator::default();
        // An MX Master reports 24 at a time: five of them make one detent.
        for _ in 0..4 {
            assert_eq!(a.accumulate(0, 24), 0);
        }
        assert_eq!(a.accumulate(0, 24), 1);
    }

    #[test]
    fn remainder_is_kept_across_events() {
        let mut a = Scroll120Accumulator::default();
        assert_eq!(a.accumulate(0, 100), 0);
        assert_eq!(a.accumulate(0, 100), 1); // 200 -> 1 step, 80 remains
        assert_eq!(a.accumulate(0, 40), 1); // 120 -> 1 step, 0 remains
        assert_eq!(a.accumulate(0, 119), 0);
    }

    #[test]
    fn axes_are_independent() {
        let mut a = Scroll120Accumulator::default();
        assert_eq!(a.accumulate(0, 60), 0);
        assert_eq!(a.accumulate(1, 60), 0);
        assert_eq!(a.accumulate(0, 60), 1);
        assert_eq!(a.accumulate(1, 60), 1);
    }

    #[test]
    fn direction_change_cancels_rather_than_latching() {
        let mut a = Scroll120Accumulator::default();
        assert_eq!(a.accumulate(0, 60), 0);
        assert_eq!(a.accumulate(0, -60), 0);
        assert_eq!(a.accumulate(0, -120), -1);
    }
}
