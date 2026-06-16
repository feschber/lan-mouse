use super::{Emulation, EmulationHandle, error::EmulationError};
use async_trait::async_trait;
use bitflags::bitflags;
use core_graphics::base::CGFloat;
use core_graphics::display::{
    CGDirectDisplayID, CGDisplayBounds, CGGetDisplaysWithRect, CGPoint, CGRect, CGSize,
};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton, EventField,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use input_event::{
    BTN_BACK, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Event, KeyboardEvent, PointerEvent,
    scancode,
};
use keycode::{KeyMap, KeyMapping};
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::{sync::Notify, task::JoinHandle};

use super::error::MacOSEmulationCreationError;

const DEFAULT_REPEAT_DELAY: Duration = Duration::from_millis(500);
const DEFAULT_REPEAT_INTERVAL: Duration = Duration::from_millis(32);
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);

pub(crate) struct MacOSEmulation {
    /// global event source for all events
    event_source: CGEventSource,
    /// task handle for key repeats
    repeat_task: Option<JoinHandle<()>>,
    /// current state of the mouse buttons (tracked by evdev button code)
    pressed_buttons: HashSet<u32>,
    /// button previously pressed (evdev button code)
    previous_button: Option<u32>,
    /// timestamp of previous click (button down)
    previous_button_click: Option<Instant>,
    /// click state, i.e. number of clicks in quick succession
    button_click_state: i64,
    /// current modifier state
    modifier_state: Rc<Cell<XMods>>,
    /// notify to cancel key repeats
    notify_repeat_task: Arc<Notify>,
}

/// Maps an evdev button code to the CGEventType used for drag events.
fn drag_event_type(button: u32) -> CGEventType {
    match button {
        BTN_LEFT => CGEventType::LeftMouseDragged,
        BTN_RIGHT => CGEventType::RightMouseDragged,
        // middle, back, forward, and any other button all use OtherMouseDragged
        _ => CGEventType::OtherMouseDragged,
    }
}

unsafe impl Send for MacOSEmulation {}

impl MacOSEmulation {
    pub(crate) fn new() -> Result<Self, MacOSEmulationCreationError> {
        request_macos_emulation_permissions()?;

        let event_source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .map_err(|_| MacOSEmulationCreationError::EventSourceCreation)?;
        Ok(Self {
            event_source,
            pressed_buttons: HashSet::new(),
            previous_button: None,
            previous_button_click: None,
            button_click_state: 0,
            repeat_task: None,
            notify_repeat_task: Arc::new(Notify::new()),
            modifier_state: Rc::new(Cell::new(XMods::empty())),
        })
    }

    fn get_mouse_location(&self) -> Option<CGPoint> {
        let event: CGEvent = CGEvent::new(self.event_source.clone()).ok()?;
        Some(event.location())
    }

    async fn spawn_repeat_task(&mut self, key: u16) {
        // there can only be one repeating key and it's
        // always the last to be pressed
        self.cancel_repeat_task().await;
        // initial key event
        key_event(self.event_source.clone(), key, 1, self.modifier_state.get());
        // repeat task
        let event_source = self.event_source.clone();
        let notify = self.notify_repeat_task.clone();
        let modifiers = self.modifier_state.clone();
        let repeat_task = tokio::task::spawn_local(async move {
            let stop = tokio::select! {
                _ = tokio::time::sleep(DEFAULT_REPEAT_DELAY) => false,
                _ = notify.notified() => true,
            };
            if !stop {
                loop {
                    key_event(event_source.clone(), key, 1, modifiers.get());
                    tokio::select! {
                        _ = tokio::time::sleep(DEFAULT_REPEAT_INTERVAL) => {},
                        _ = notify.notified() => break,
                    }
                }
            }
            // Always release the key with the correct CGKeyCode, regardless of
            // whether the repeat loop ran. This matches @feschber's review
            // request: "still release the key repeat task but with the correct
            // code."
            //
            // Do NOT call update_modifiers here: `key` is a Mac CGKeyCode but
            // update_modifiers expects a Linux evdev scancode, and the two
            // codespaces collide (e.g. Mac LeftShift=56 == Linux KeyLeftAlt=56,
            // Mac Down=125 == Linux KeyLeftMeta=125), corrupting modifier
            // state for chords like Shift+Option+X or Cmd+Down. Modifier state
            // is owned by the main consume() loop, which already calls
            // update_modifiers with the correct Linux scancode on the real key
            // release event from the client.
            key_event(event_source.clone(), key, 0, modifiers.get());
        });
        self.repeat_task = Some(repeat_task);
    }

    async fn cancel_repeat_task(&mut self) {
        if let Some(task) = self.repeat_task.take() {
            self.notify_repeat_task.notify_waiters();
            let _ = task.await;
        }
    }
}

fn request_macos_emulation_permissions() -> Result<(), MacOSEmulationCreationError> {
    // Request both permissions up front so the user sees both TCC prompts
    // on the first launch. See the matching comment in input-capture/src/
    // macos.rs::request_macos_capture_permissions for the rationale.
    let accessibility = request_accessibility_permission();
    let input_control = request_input_control_permission();

    if !accessibility {
        return Err(MacOSEmulationCreationError::AccessibilityPermission);
    }
    if !input_control {
        return Err(MacOSEmulationCreationError::InputControlPermission);
    }
    Ok(())
}

fn request_accessibility_permission() -> bool {
    // Silent check. The GUI owns the one-time user-visible prompt at
    // startup (see lan_mouse_gtk::macos_privacy).
    unsafe { AXIsProcessTrusted() }
}

fn request_input_control_permission() -> bool {
    unsafe { CGPreflightPostEventAccess() }
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightPostEventAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// Mac virtual key codes for the four arrow keys.
const MAC_KEY_LEFT: u16 = 0x7B;
const MAC_KEY_RIGHT: u16 = 0x7C;
const MAC_KEY_DOWN: u16 = 0x7D;
const MAC_KEY_UP: u16 = 0x7E;

fn is_arrow_key(key: u16) -> bool {
    matches!(
        key,
        MAC_KEY_LEFT | MAC_KEY_RIGHT | MAC_KEY_DOWN | MAC_KEY_UP
    )
}

fn key_event(event_source: CGEventSource, key: u16, state: u8, modifiers: XMods) {
    let event = match CGEvent::new_keyboard_event(event_source, key, state != 0) {
        Ok(e) => e,
        Err(_) => {
            log::warn!("unable to create key event");
            return;
        }
    };
    let mut flags = to_cgevent_flags(modifiers);
    // Hardware-generated arrow keys on macOS carry NumericPad + SecondaryFn.
    // CGEventTap-based hotkey matchers (e.g. tiling window managers) check
    // these flags to recognize navigation keys; without them synthesized
    // arrow chords fall through to the focused app.
    if is_arrow_key(key) {
        flags |= CGEventFlags::CGEventFlagNumericPad | CGEventFlags::CGEventFlagSecondaryFn;
    }
    event.set_flags(flags);
    event.post(CGEventTapLocation::HID);
    log::trace!("key event: {key} {state}");
}

/// Posts a `FlagsChanged` event for a modifier key.
///
/// The event MUST carry the modifier's real virtual keycode. A bare
/// `CGEvent::new()` defaults to keycode 0 (`kVK_ANSI_A`), so every modifier
/// change arrived in apps as a phantom "A" key event — holding Ctrl registered
/// as Ctrl+A and shortcut recorders captured "A" (issue #450).
///
/// Carrying the real keycode also matters for consumers that track *physical*
/// modifier transitions through AppKit's `flagsChanged(with:)` rather than the
/// flags on the key-down event — notably Apple Virtualization.framework guest
/// views (`VZVirtualMachineView`), which derive guest modifier state from these
/// `FlagsChanged` events. The event is built as a key-down so it gets a valid
/// keycode; the type is then overridden to `FlagsChanged` and the *current*
/// modifier flags (already updated by the caller) describe the new state.
fn modifier_key_event(event_source: CGEventSource, key: u16, depressed: XMods) {
    let Ok(event) = CGEvent::new_keyboard_event(event_source, key, true) else {
        log::warn!("could not create modifier key event");
        return;
    };
    event.set_type(CGEventType::FlagsChanged);
    event.set_flags(modifier_flags_changed_flags(depressed));
    event.post(CGEventTapLocation::HID);
    log::trace!("modifier key event: {key} {depressed:?}");
}

/// Builds the flag set for a modifier `FlagsChanged` event.
///
/// Combines the device-INDEPENDENT modifier word (what ordinary AppKit apps
/// read) with the device-DEPENDENT low-word bits (IOKit `NX_DEVICE*KEYMASK` from
/// `IOKit/hidsystem/IOLLEvent.h`) that a real hardware keyboard sets.
///
/// This matters for Apple Virtualization.framework guest views
/// (`VZVirtualMachineView`, used by UTM's Apple backend, Parallels' macOS
/// guests, VirtualBuddy, etc.): they derive the guest's modifier state from the
/// `flagsChanged(with:)` responder method and read the device-dependent low
/// word, not the per-key flags. macOS does NOT synthesize the low-word bits for
/// posted (synthetic) events, so until we set them ourselves Cmd/Ctrl/Shift/Opt
/// chords reached the guest unmodified (Shift+2 → "2", Cmd+C did nothing).
///
/// Ordinary apps mask incoming events to the device-independent word
/// (`deviceIndependentFlagsMask`) and ignore these bits, so emitting them is
/// hardware-faithful and safe for every app — no VM/bundle-id detection is
/// required. See issue #450 and https://developer.apple.com/forums/thread/766014
fn modifier_flags_changed_flags(depressed: XMods) -> CGEventFlags {
    // Device-dependent left/right modifier bits (IOLLEvent.h). lan-mouse collapses
    // left and right modifiers into a single mask, so we emit the left-hand device
    // bit; AltGr (Mod5 / ISO_Level3_Shift) is physically the right Alt, so it maps
    // to the right Option bit.
    const NX_DEVICE_L_CTRL: u64 = 0x0000_0001;
    const NX_DEVICE_L_SHIFT: u64 = 0x0000_0002;
    const NX_DEVICE_L_CMD: u64 = 0x0000_0008;
    const NX_DEVICE_L_ALT: u64 = 0x0000_0020;
    const NX_DEVICE_R_ALT: u64 = 0x0000_0040;

    let mut device_bits: u64 = 0;
    if depressed.contains(XMods::ShiftMask) {
        device_bits |= NX_DEVICE_L_SHIFT;
    }
    if depressed.contains(XMods::ControlMask) {
        device_bits |= NX_DEVICE_L_CTRL;
    }
    if depressed.contains(XMods::Mod1Mask) {
        device_bits |= NX_DEVICE_L_ALT;
    }
    if depressed.contains(XMods::Mod5Mask) {
        device_bits |= NX_DEVICE_R_ALT;
    }
    if depressed.contains(XMods::Mod4Mask) {
        device_bits |= NX_DEVICE_L_CMD;
    }

    // CGEventFlagNonCoalesced is bit 8 (0x100), the marker a real hardware
    // FlagsChanged carries on both press and release; VZ expects it present.
    let flags = to_cgevent_flags(depressed) | CGEventFlags::CGEventFlagNonCoalesced;
    CGEventFlags::from_bits_retain(flags.bits() | device_bits)
}

fn get_display_at_point(x: CGFloat, y: CGFloat) -> Option<CGDirectDisplayID> {
    let mut displays: [CGDirectDisplayID; 16] = [0; 16];
    let mut display_count: u32 = 0;
    let rect = CGRect::new(&CGPoint::new(x, y), &CGSize::new(0.0, 0.0));

    let error = unsafe {
        CGGetDisplaysWithRect(
            rect,
            1,
            displays.as_mut_ptr(),
            &mut display_count as *mut u32,
        )
    };

    if error != 0 {
        log::warn!("error getting displays at point ({x}, {y}): {error}");
        return Option::None;
    }

    if display_count == 0 {
        log::debug!("no displays found at point ({x}, {y})");
        return Option::None;
    }

    displays.first().copied()
}

fn get_display_bounds(display: CGDirectDisplayID) -> (CGFloat, CGFloat, CGFloat, CGFloat) {
    unsafe {
        let bounds = CGDisplayBounds(display);
        let min_x = bounds.origin.x;
        let max_x = bounds.origin.x + bounds.size.width;
        let min_y = bounds.origin.y;
        let max_y = bounds.origin.y + bounds.size.height;
        (min_x as f64, min_y as f64, max_x as f64, max_y as f64)
    }
}

fn clamp_to_screen_space(
    current_x: CGFloat,
    current_y: CGFloat,
    dx: CGFloat,
    dy: CGFloat,
) -> (CGFloat, CGFloat) {
    // Check which display the mouse is currently on
    // Determine what the location of the mouse would be after applying the move
    // Get the display at the new location
    // If the point is not on a display
    //   Clamp the mouse to the current display
    // Else If the point is on a display
    //   Clamp the mouse to the new display
    let current_display = match get_display_at_point(current_x, current_y) {
        Some(display) => display,
        None => {
            log::warn!("could not get current display!");
            return (current_x, current_y);
        }
    };

    let new_x = current_x + dx;
    let new_y = current_y + dy;

    let final_display = get_display_at_point(new_x, new_y).unwrap_or(current_display);
    let (min_x, min_y, max_x, max_y) = get_display_bounds(final_display);

    (
        new_x.clamp(min_x, max_x - 1.),
        new_y.clamp(min_y, max_y - 1.),
    )
}

#[async_trait]
impl Emulation for MacOSEmulation {
    async fn consume(
        &mut self,
        event: Event,
        _handle: EmulationHandle,
    ) -> Result<(), EmulationError> {
        log::trace!("{event:?}");
        match event {
            Event::Pointer(pointer_event) => {
                match pointer_event {
                    PointerEvent::Motion { time: _, dx, dy } => {
                        let mut mouse_location = match self.get_mouse_location() {
                            Some(l) => l,
                            None => {
                                log::warn!("could not get mouse location!");
                                return Ok(());
                            }
                        };

                        let (new_mouse_x, new_mouse_y) =
                            clamp_to_screen_space(mouse_location.x, mouse_location.y, dx, dy);

                        mouse_location.x = new_mouse_x;
                        mouse_location.y = new_mouse_y;

                        // If any button is held, emit a drag event for it;
                        // otherwise emit a normal mouse-moved event.
                        let event_type = self
                            .pressed_buttons
                            .iter()
                            .next()
                            .map(|&btn| drag_event_type(btn))
                            .unwrap_or(CGEventType::MouseMoved);
                        let event = match CGEvent::new_mouse_event(
                            self.event_source.clone(),
                            event_type,
                            mouse_location,
                            CGMouseButton::Left,
                        ) {
                            Ok(e) => e,
                            Err(_) => {
                                log::warn!("mouse event creation failed!");
                                return Ok(());
                            }
                        };
                        event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_X, dx as i64);
                        event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y, dy as i64);
                        event.post(CGEventTapLocation::HID);
                    }
                    PointerEvent::Button {
                        time: _,
                        button,
                        state,
                    } => {
                        // button number for OtherMouse events (3 = back, 4 = forward, etc.)
                        let cg_button_number: Option<i64> = match button {
                            BTN_BACK => Some(3),
                            BTN_FORWARD => Some(4),
                            _ => None,
                        };
                        let (event_type, mouse_button) = match (button, state) {
                            (BTN_LEFT, 1) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
                            (BTN_LEFT, 0) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
                            (BTN_RIGHT, 1) => (CGEventType::RightMouseDown, CGMouseButton::Right),
                            (BTN_RIGHT, 0) => (CGEventType::RightMouseUp, CGMouseButton::Right),
                            (BTN_MIDDLE, 1) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
                            (BTN_MIDDLE, 0) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
                            (BTN_BACK, 1) | (BTN_FORWARD, 1) => {
                                (CGEventType::OtherMouseDown, CGMouseButton::Center)
                            }
                            (BTN_BACK, 0) | (BTN_FORWARD, 0) => {
                                (CGEventType::OtherMouseUp, CGMouseButton::Center)
                            }
                            _ => {
                                log::warn!("invalid button event: {button},{state}");
                                return Ok(());
                            }
                        };
                        // store button state using the evdev button code so
                        // back, forward, and middle are tracked independently
                        if state == 1 {
                            self.pressed_buttons.insert(button);
                        } else {
                            self.pressed_buttons.remove(&button);
                        }

                        // update double-click tracking using the evdev button
                        // code so that back/forward don't alias with middle
                        if state == 1 {
                            if self.previous_button == Some(button)
                                && self
                                    .previous_button_click
                                    .is_some_and(|i| i.elapsed() < DOUBLE_CLICK_INTERVAL)
                            {
                                self.button_click_state += 1;
                            } else {
                                self.button_click_state = 1;
                            }
                            self.previous_button = Some(button);
                            self.previous_button_click = Some(Instant::now());
                        }

                        log::debug!("click_state: {}", self.button_click_state);
                        let location = self.get_mouse_location().unwrap();
                        let event = match CGEvent::new_mouse_event(
                            self.event_source.clone(),
                            event_type,
                            location,
                            mouse_button,
                        ) {
                            Ok(e) => e,
                            Err(()) => {
                                log::warn!("mouse event creation failed!");
                                return Ok(());
                            }
                        };
                        event.set_integer_value_field(
                            EventField::MOUSE_EVENT_CLICK_STATE,
                            self.button_click_state,
                        );
                        // Set the button number for extra buttons (back=3, forward=4)
                        if let Some(btn_num) = cg_button_number {
                            event.set_integer_value_field(
                                EventField::MOUSE_EVENT_BUTTON_NUMBER,
                                btn_num,
                            );
                        }
                        event.post(CGEventTapLocation::HID);
                    }
                    PointerEvent::Axis {
                        time: _,
                        axis,
                        value,
                    } => {
                        let value = value as i32;
                        let (count, wheel1, wheel2, wheel3) = match axis {
                            0 => (1, value, 0, 0), // 0 = vertical => 1 scroll wheel device (y axis)
                            1 => (2, 0, value, 0), // 1 = horizontal => 2 scroll wheel devices (y, x) -> (0, x)
                            _ => {
                                log::warn!("invalid scroll event: {axis}, {value}");
                                return Ok(());
                            }
                        };
                        let event = match CGEvent::new_scroll_event(
                            self.event_source.clone(),
                            ScrollEventUnit::PIXEL,
                            count,
                            wheel1,
                            wheel2,
                            wheel3,
                        ) {
                            Ok(e) => e,
                            Err(()) => {
                                log::warn!("scroll event creation failed!");
                                return Ok(());
                            }
                        };
                        event.post(CGEventTapLocation::HID);
                    }
                    PointerEvent::AxisDiscrete120 { axis, value } => {
                        const LINES_PER_STEP: i32 = 3;
                        let (count, wheel1, wheel2, wheel3) = match axis {
                            0 => (1, value / (120 / LINES_PER_STEP), 0, 0), // 0 = vertical => 1 scroll wheel device (y axis)
                            1 => (2, 0, value / (120 / LINES_PER_STEP), 0), // 1 = horizontal => 2 scroll wheel devices (y, x) -> (0, x)
                            _ => {
                                log::warn!("invalid scroll event: {axis}, {value}");
                                return Ok(());
                            }
                        };
                        let event = match CGEvent::new_scroll_event(
                            self.event_source.clone(),
                            ScrollEventUnit::LINE,
                            count,
                            wheel1,
                            wheel2,
                            wheel3,
                        ) {
                            Ok(e) => e,
                            Err(()) => {
                                log::warn!("scroll event creation failed!");
                                return Ok(());
                            }
                        };
                        event.post(CGEventTapLocation::HID);
                    }
                }

                // reset button click state in case it's not a button event
                if !matches!(pointer_event, PointerEvent::Button { .. }) {
                    self.button_click_state = 0;
                }
            }
            Event::Keyboard(keyboard_event) => match keyboard_event {
                KeyboardEvent::Key {
                    time: _,
                    key,
                    state,
                } => {
                    let code = match KeyMap::from_key_mapping(KeyMapping::Evdev(key as u16)) {
                        Ok(k) => k.mac as CGKeyCode,
                        Err(_) => {
                            log::warn!("unable to map key event");
                            return Ok(());
                        }
                    };
                    let is_modifier = update_modifiers(&self.modifier_state, key, state);
                    if is_modifier {
                        // Modifier keys are posted as FlagsChanged events carrying
                        // their real keycode (see modifier_key_event). They must NOT
                        // enter the key-repeat machinery: there is only one repeat
                        // slot, so pressing a second modifier would cancel the first
                        // modifier's repeat task and post a keyUp while it is still
                        // physically held, tearing chords apart (issue #450, #357).
                        modifier_key_event(
                            self.event_source.clone(),
                            code,
                            self.modifier_state.get(),
                        );
                    } else {
                        match state {
                            // pressed
                            1 => self.spawn_repeat_task(code).await,
                            _ => self.cancel_repeat_task().await,
                        }
                    }
                }
                KeyboardEvent::Modifiers {
                    depressed,
                    latched,
                    locked,
                    group,
                } => {
                    // Only update internal modifier state here. The per-key handler
                    // above already posts a FlagsChanged event (with the real
                    // keycode) for each modifier Key event the client sends
                    // alongside this state update. Posting one here as well would
                    // duplicate it — and with the old bare CGEvent it injected a
                    // phantom keycode-0 ("A") key on every modifier change (#450).
                    set_modifiers(&self.modifier_state, depressed, latched, locked, group);
                }
            },
        }
        // FIXME
        Ok(())
    }

    async fn create(&mut self, _handle: EmulationHandle) {}

    async fn destroy(&mut self, _handle: EmulationHandle) {}

    async fn terminate(&mut self) {}
}

fn update_modifiers(modifiers: &Cell<XMods>, key: u32, state: u8) -> bool {
    if let Ok(key) = scancode::Linux::try_from(key) {
        let mask = match key {
            scancode::Linux::KeyLeftShift | scancode::Linux::KeyRightShift => XMods::ShiftMask,
            scancode::Linux::KeyCapsLock => XMods::LockMask,
            scancode::Linux::KeyLeftCtrl | scancode::Linux::KeyRightCtrl => XMods::ControlMask,
            scancode::Linux::KeyLeftAlt | scancode::Linux::KeyRightalt => XMods::Mod1Mask,
            scancode::Linux::KeyLeftMeta | scancode::Linux::KeyRightmeta => XMods::Mod4Mask,
            _ => XMods::empty(),
        };
        // unchanged
        if mask.is_empty() {
            return false;
        }
        let mut mods = modifiers.get();
        match state {
            1 => mods.insert(mask),
            _ => mods.remove(mask),
        }
        modifiers.set(mods);
        true
    } else {
        false
    }
}

fn set_modifiers(
    active_modifiers: &Cell<XMods>,
    depressed: u32,
    latched: u32,
    locked: u32,
    group: u32,
) {
    let depressed = XMods::from_bits(depressed).unwrap_or_default();
    let _latched = XMods::from_bits(latched).unwrap_or_default();
    let _locked = XMods::from_bits(locked).unwrap_or_default();
    let _group = XMods::from_bits(group).unwrap_or_default();

    // we only care about the depressed modifiers for now
    active_modifiers.replace(depressed);
}

fn to_cgevent_flags(depressed: XMods) -> CGEventFlags {
    let mut flags = CGEventFlags::empty();
    if depressed.contains(XMods::ShiftMask) {
        flags |= CGEventFlags::CGEventFlagShift;
    }
    if depressed.contains(XMods::LockMask) {
        flags |= CGEventFlags::CGEventFlagAlphaShift;
    }
    if depressed.contains(XMods::ControlMask) {
        flags |= CGEventFlags::CGEventFlagControl;
    }
    // Mod1 is Alt. Mod5 is ISO_Level3_Shift (AltGr), which is how the Alt key is
    // reported on many xkb keymaps (including COSMIC's default) in the wholesale
    // Modifiers state events. Map both to Option so Alt/Option chords are not
    // silently dropped (issue #450).
    if depressed.contains(XMods::Mod1Mask) || depressed.contains(XMods::Mod5Mask) {
        flags |= CGEventFlags::CGEventFlagAlternate;
    }
    if depressed.contains(XMods::Mod4Mask) {
        flags |= CGEventFlags::CGEventFlagCommand;
    }
    flags
}

// From X11/X.h
bitflags! {
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
    struct XMods: u32 {
        const ShiftMask = (1<<0);
        const LockMask = (1<<1);
        const ControlMask = (1<<2);
        const Mod1Mask = (1<<3);
        const Mod2Mask = (1<<4);
        const Mod3Mask = (1<<5);
        const Mod4Mask = (1<<6);
        const Mod5Mask = (1<<7);
    }
}
