use input_event::{Event, PointerEvent};

/// axis id carried by [`PointerEvent::Axis`] / [`PointerEvent::AxisDiscrete120`]
/// for vertical scrolling; anything else is horizontal.
const VERTICAL: u8 = 0;

/// Inverts scroll direction on its way to another device.
///
/// The motivating case is macOS' "natural scrolling" reaching a Windows
/// or Linux peer that scrolls the traditional way: the two disagree on
/// which way the wheel should move the content, so scrolling feels
/// backwards on the peer even though both machines are working as
/// designed.
///
/// Like [`crate::remap::KeyRemap`], this happens on the *sending* side,
/// right before events go on the wire, so the local machine's own
/// scrolling is unaffected.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct ScrollInvert {
    vertical: bool,
    horizontal: bool,
}

impl ScrollInvert {
    pub(crate) fn new(vertical: bool, horizontal: bool) -> Self {
        Self {
            vertical,
            horizontal,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        !self.vertical && !self.horizontal
    }

    fn inverts(&self, axis: u8) -> bool {
        if axis == VERTICAL {
            self.vertical
        } else {
            self.horizontal
        }
    }

    /// Invert an outgoing event. Anything that isn't a scroll axis
    /// passes through untouched.
    pub(crate) fn apply(&self, event: Event) -> Event {
        if self.is_empty() {
            return event;
        }
        match event {
            Event::Pointer(PointerEvent::Axis { time, axis, value }) if self.inverts(axis) => {
                Event::Pointer(PointerEvent::Axis {
                    time,
                    axis,
                    value: -value,
                })
            }
            Event::Pointer(PointerEvent::AxisDiscrete120 { axis, value }) if self.inverts(axis) => {
                Event::Pointer(PointerEvent::AxisDiscrete120 {
                    axis,
                    value: -value,
                })
            }
            e => e,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const HORIZONTAL: u8 = 1;

    fn axis(axis: u8, value: f64) -> Event {
        Event::Pointer(PointerEvent::Axis {
            time: 0,
            axis,
            value,
        })
    }

    fn axis120(axis: u8, value: i32) -> Event {
        Event::Pointer(PointerEvent::AxisDiscrete120 { axis, value })
    }

    #[test]
    fn inverts_vertical_scroll() {
        let s = ScrollInvert::new(true, false);
        assert_eq!(s.apply(axis(VERTICAL, 1.5)), axis(VERTICAL, -1.5));
        assert_eq!(s.apply(axis120(VERTICAL, 120)), axis120(VERTICAL, -120));
    }

    #[test]
    fn leaves_horizontal_alone_when_only_vertical_is_inverted() {
        let s = ScrollInvert::new(true, false);
        assert_eq!(s.apply(axis(HORIZONTAL, 1.5)), axis(HORIZONTAL, 1.5));
    }

    #[test]
    fn inverts_horizontal_scroll() {
        let s = ScrollInvert::new(false, true);
        assert_eq!(s.apply(axis(HORIZONTAL, 1.5)), axis(HORIZONTAL, -1.5));
    }

    #[test]
    fn leaves_vertical_alone_when_only_horizontal_is_inverted() {
        let s = ScrollInvert::new(false, true);
        assert_eq!(s.apply(axis(VERTICAL, 1.5)), axis(VERTICAL, 1.5));
    }

    #[test]
    fn inverts_both_axes() {
        let s = ScrollInvert::new(true, true);
        assert_eq!(s.apply(axis(VERTICAL, 1.0)), axis(VERTICAL, -1.0));
        assert_eq!(s.apply(axis(HORIZONTAL, 1.0)), axis(HORIZONTAL, -1.0));
    }

    #[test]
    fn empty_invert_is_a_passthrough() {
        let s = ScrollInvert::default();
        assert!(s.is_empty());
        assert_eq!(s.apply(axis(VERTICAL, 1.0)), axis(VERTICAL, 1.0));
    }

    #[test]
    fn leaves_non_scroll_events_alone() {
        let s = ScrollInvert::new(true, true);
        let motion = Event::Pointer(PointerEvent::Motion {
            time: 0,
            dx: 1.0,
            dy: 1.0,
        });
        assert_eq!(s.apply(motion), motion);
    }
}
