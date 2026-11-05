use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    mem::zeroed,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
};

use async_trait::async_trait;
use input_event::{
    BTN_BACK, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Event, KeyboardEvent, PointerEvent,
    scancode,
};

use super::{
    Emulation, EmulationHandle,
    error::{EmulationError, UinputEmulationCreationError},
};

const UINPUT_MAX_NAME_SIZE: usize = 80;
const DEVICE_NAME: &[u8] = b"lan-mouse-uinput";

// Event types (<linux/input-event-codes.h>)
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;

// Synchronization events
const SYN_REPORT: u16 = 0x00;

// Relative axes
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
const REL_WHEEL_HI_RES: u16 = 0x0b;
const REL_HWHEEL_HI_RES: u16 = 0x0c;

// Bus type (<linux/input.h>)
const BUS_USB: u16 = 0x03;

// uinput ioctl requests (<linux/uinput.h>)
const UI_DEV_CREATE: libc::Ioctl = 0x5501;
const UI_DEV_DESTROY: libc::Ioctl = 0x5502;
const UI_DEV_SETUP: libc::Ioctl = 0x405c5503;
const UI_SET_EVBIT: libc::Ioctl = 0x40045564;
const UI_SET_KEYBIT: libc::Ioctl = 0x40045565;
const UI_SET_RELBIT: libc::Ioctl = 0x40045566;

/// Wayland continuous scroll uses 15.0 surface units per wheel click (120 hi-res units).
const HI_RES_PER_SURFACE_UNIT: f64 = 120.0 / 15.0;

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [libc::c_char; UINPUT_MAX_NAME_SIZE],
    ff_effects_max: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawInputEvent {
    tv_sec: libc::time_t,
    tv_usec: libc::suseconds_t,
    type_: u16,
    code: u16,
    value: i32,
}

impl RawInputEvent {
    fn new(type_: u16, code: u16, value: i32) -> Self {
        Self {
            tv_sec: 0,
            tv_usec: 0,
            type_,
            code,
            value,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        // SAFETY: RawInputEvent is #[repr(C)] with no uninitialized padding bytes.
        unsafe {
            std::slice::from_raw_parts(
                (self as *const Self).cast::<u8>(),
                std::mem::size_of::<Self>(),
            )
        }
    }
}

#[derive(Debug, Default)]
struct MotionAccumulator {
    rem_x: f64,
    rem_y: f64,
}

impl MotionAccumulator {
    fn step(&mut self, dx: f64, dy: f64) -> (i32, i32) {
        self.rem_x += dx;
        self.rem_y += dy;
        let move_x = self.rem_x.trunc() as i32;
        let move_y = self.rem_y.trunc() as i32;
        self.rem_x -= move_x as f64;
        self.rem_y -= move_y as f64;
        (move_x, move_y)
    }
}

#[derive(Debug, Default)]
struct ScrollAccumulator {
    /// Fractional hi-res remainder for continuous vertical scroll
    continuous_rem_v: f64,
    /// Fractional hi-res remainder for continuous horizontal scroll
    continuous_rem_h: f64,
    /// Accumulated hi-res units toward a 120-unit discrete vertical wheel click
    discrete_rem_v: i32,
    /// Accumulated hi-res units toward a 120-unit discrete horizontal wheel click
    discrete_rem_h: i32,
}

impl ScrollAccumulator {
    /// Convert a discrete 120-based scroll delta into `(hi_res_delta, wheel_clicks)`
    /// using Linux evdev sign conventions (vertical: positive = up; horizontal: positive = right).
    fn step_discrete_120(&mut self, axis: u8, value: i32) -> (i32, i32) {
        if axis == 0 {
            // Wayland/lan-mouse vertical axis is positive down; evdev REL_WHEEL is positive up
            let hi_res = value.saturating_neg();
            self.discrete_rem_v = self.discrete_rem_v.saturating_add(hi_res);
            let clicks = self.discrete_rem_v / 120;
            self.discrete_rem_v %= 120;
            (hi_res, clicks)
        } else {
            let hi_res = value;
            self.discrete_rem_h = self.discrete_rem_h.saturating_add(hi_res);
            let clicks = self.discrete_rem_h / 120;
            self.discrete_rem_h %= 120;
            (hi_res, clicks)
        }
    }

    /// Convert continuous surface-unit scroll into `(hi_res_delta, wheel_clicks)`.
    fn step_continuous(&mut self, axis: u8, value: f64) -> (i32, i32) {
        let scaled = value * HI_RES_PER_SURFACE_UNIT;
        let hi_res = if axis == 0 {
            self.continuous_rem_v += scaled;
            let delta = self.continuous_rem_v.trunc() as i32;
            self.continuous_rem_v -= delta as f64;
            delta
        } else {
            self.continuous_rem_h += scaled;
            let delta = self.continuous_rem_h.trunc() as i32;
            self.continuous_rem_h -= delta as f64;
            delta
        };
        if hi_res == 0 {
            return (0, 0);
        }
        self.step_discrete_120(axis, hi_res)
    }
}

pub(crate) struct UinputEmulation {
    file: File,
    motion: MotionAccumulator,
    scroll: ScrollAccumulator,
    destroyed: bool,
}

fn ioctl_c_int(fd: i32, request: libc::Ioctl, value: libc::c_int) -> io::Result<()> {
    // SAFETY: UI_SET_*BIT ioctls take an integer bit value.
    let res = unsafe { libc::ioctl(fd, request, value) };
    if res < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn open_uinput_file() -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);

    opts.open("/dev/uinput")
        .or_else(|_| opts.open("/dev/input/uinput"))
}

impl UinputEmulation {
    pub(crate) fn new() -> Result<Self, UinputEmulationCreationError> {
        let file = open_uinput_file().map_err(UinputEmulationCreationError::OpenDevice)?;
        let fd = file.as_raw_fd();

        Self::setup_device(fd).map_err(UinputEmulationCreationError::SetupDevice)?;

        Ok(Self {
            file,
            motion: MotionAccumulator::default(),
            scroll: ScrollAccumulator::default(),
            destroyed: false,
        })
    }

    fn setup_device(fd: i32) -> io::Result<()> {
        ioctl_c_int(fd, UI_SET_EVBIT, EV_SYN as libc::c_int)?;
        ioctl_c_int(fd, UI_SET_EVBIT, EV_KEY as libc::c_int)?;
        ioctl_c_int(fd, UI_SET_EVBIT, EV_REL as libc::c_int)?;

        // Enable all Linux keyboard keycodes supported by input-event
        for key in 1..(scancode::Linux::KeyCount as libc::c_int) {
            ioctl_c_int(fd, UI_SET_KEYBIT, key)?;
        }

        // Enable mouse buttons
        for btn in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_BACK, BTN_FORWARD] {
            ioctl_c_int(fd, UI_SET_KEYBIT, btn as libc::c_int)?;
        }

        // Enable relative pointer axes and wheel axes
        for rel in [
            REL_X,
            REL_Y,
            REL_WHEEL,
            REL_HWHEEL,
            REL_WHEEL_HI_RES,
            REL_HWHEEL_HI_RES,
        ] {
            ioctl_c_int(fd, UI_SET_RELBIT, rel as libc::c_int)?;
        }

        // SAFETY: UinputSetup is a POD C struct; zero-initializing is valid.
        let mut setup: UinputSetup = unsafe { zeroed() };
        setup.id = InputId {
            bustype: BUS_USB,
            vendor: 0x0001,
            product: 0x0001,
            version: 1,
        };
        for (dst, &src) in setup.name.iter_mut().zip(DEVICE_NAME.iter()) {
            *dst = src as libc::c_char;
        }

        // SAFETY: UI_DEV_SETUP expects a pointer to uinput_setup.
        if unsafe { libc::ioctl(fd, UI_DEV_SETUP, &setup as *const UinputSetup) } < 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: UI_DEV_CREATE takes no extra arguments.
        if unsafe { libc::ioctl(fd, UI_DEV_CREATE) } < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }

    fn emit(&mut self, events: &[RawInputEvent]) -> io::Result<()> {
        for event in events {
            self.file.write_all(event.as_bytes())?;
        }
        Ok(())
    }

    fn emit_syn(&mut self, events: &mut Vec<RawInputEvent>) -> io::Result<()> {
        if !events.is_empty() {
            events.push(RawInputEvent::new(EV_SYN, SYN_REPORT, 0));
            self.emit(events)?;
        }
        Ok(())
    }

    fn emit_scroll(&mut self, axis: u8, hi_res: i32, clicks: i32) -> io::Result<()> {
        let mut events = Vec::with_capacity(3);
        let (hi_res_code, wheel_code) = if axis == 0 {
            (REL_WHEEL_HI_RES, REL_WHEEL)
        } else {
            (REL_HWHEEL_HI_RES, REL_HWHEEL)
        };
        if hi_res != 0 {
            events.push(RawInputEvent::new(EV_REL, hi_res_code, hi_res));
        }
        if clicks != 0 {
            events.push(RawInputEvent::new(EV_REL, wheel_code, clicks));
        }
        self.emit_syn(&mut events)
    }

    fn destroy_device(&mut self) {
        if !self.destroyed {
            self.destroyed = true;
            // SAFETY: UI_DEV_DESTROY destroys the uinput device associated with self.file.
            unsafe {
                libc::ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY);
            }
        }
    }
}

impl Drop for UinputEmulation {
    fn drop(&mut self) {
        self.destroy_device();
    }
}

#[async_trait]
impl Emulation for UinputEmulation {
    async fn consume(
        &mut self,
        event: Event,
        _handle: EmulationHandle,
    ) -> Result<(), EmulationError> {
        match event {
            Event::Pointer(p) => match p {
                PointerEvent::Motion { time: _, dx, dy } => {
                    let (move_x, move_y) = self.motion.step(dx, dy);
                    let mut events = Vec::with_capacity(3);
                    if move_x != 0 {
                        events.push(RawInputEvent::new(EV_REL, REL_X, move_x));
                    }
                    if move_y != 0 {
                        events.push(RawInputEvent::new(EV_REL, REL_Y, move_y));
                    }
                    self.emit_syn(&mut events)?;
                }
                PointerEvent::Button {
                    time: _,
                    button,
                    state,
                } => {
                    let mut events = vec![RawInputEvent::new(
                        EV_KEY,
                        button as u16,
                        i32::from(state != 0),
                    )];
                    self.emit_syn(&mut events)?;
                }
                PointerEvent::Axis {
                    time: _,
                    axis,
                    value,
                } => {
                    let (hi_res, clicks) = self.scroll.step_continuous(axis, value);
                    self.emit_scroll(axis, hi_res, clicks)?;
                }
                PointerEvent::AxisDiscrete120 { axis, value } => {
                    let (hi_res, clicks) = self.scroll.step_discrete_120(axis, value);
                    self.emit_scroll(axis, hi_res, clicks)?;
                }
            },
            Event::Keyboard(k) => match k {
                KeyboardEvent::Key {
                    time: _,
                    key,
                    state,
                } => {
                    let mut events = vec![RawInputEvent::new(
                        EV_KEY,
                        key as u16,
                        i32::from(state != 0),
                    )];
                    self.emit_syn(&mut events)?;
                }
                KeyboardEvent::Modifiers { .. } => {
                    // Compositor / libinput tracks modifiers from the evdev key events directly.
                }
            },
        }
        Ok(())
    }

    async fn create(&mut self, _handle: EmulationHandle) {}

    async fn destroy(&mut self, _handle: EmulationHandle) {}

    async fn terminate(&mut self) {
        self.destroy_device();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subpixel_motion_accumulates_without_loss() {
        let mut acc = MotionAccumulator::default();
        assert_eq!(acc.step(0.4, -0.4), (0, 0));
        assert_eq!(acc.step(0.4, -0.4), (0, 0));
        assert_eq!(acc.step(0.4, -0.4), (1, -1));
        assert!((acc.rem_x - 0.2).abs() < 1e-9);
        assert!((acc.rem_y - (-0.2)).abs() < 1e-9);
    }

    #[test]
    fn discrete_vertical_scroll_inverts_sign_and_emits_clicks() {
        let mut acc = ScrollAccumulator::default();
        // Positive Wayland vertical scroll (down) -> negative evdev REL_WHEEL (down)
        assert_eq!(acc.step_discrete_120(0, 60), (-60, 0));
        assert_eq!(acc.step_discrete_120(0, 60), (-60, -1));
        // Negative Wayland vertical scroll (up) -> positive evdev REL_WHEEL (up)
        assert_eq!(acc.step_discrete_120(0, -120), (120, 1));
    }

    #[test]
    fn discrete_horizontal_scroll_preserves_sign() {
        let mut acc = ScrollAccumulator::default();
        assert_eq!(acc.step_discrete_120(1, 120), (120, 1));
        assert_eq!(acc.step_discrete_120(1, -120), (-120, -1));
    }

    #[test]
    fn continuous_scroll_accumulates_to_wheel_clicks() {
        let mut acc = ScrollAccumulator::default();
        // 7.5 surface units = 60 hi-res units (half a notch)
        assert_eq!(acc.step_continuous(0, 7.5), (-60, 0));
        assert_eq!(acc.step_continuous(0, 7.5), (-60, -1));
    }
}
