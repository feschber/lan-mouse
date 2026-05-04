use crate::error::EmulationError;

use super::{Emulation, error::WlrootsEmulationCreationError};
use async_trait::async_trait;
use bitflags::bitflags;
use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use wayland_client::Proxy;
use wayland_client::WEnum;
use wayland_client::backend::WaylandError;

use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_pointer::{Axis, AxisSource, ButtonState};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1 as VpManager,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1 as Vp,
};

use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1 as VkManager,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1 as Vk,
};

use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry, wl_seat},
};

use input_event::{Event, KeyboardEvent, PointerEvent, scancode};

use super::EmulationHandle;
use super::error::WaylandBindError;

struct State {
    keymap: Option<(u32, OwnedFd, u32)>,
    input_for_client: HashMap<EmulationHandle, VirtualInput>,
    seat: wl_seat::WlSeat,
    qh: QueueHandle<Self>,
    vpm: VpManager,
    vkm: VkManager,
    /// All wl_outputs the compositor advertises, keyed by their
    /// proxy id. Updated via Geometry/Mode events. We keep the
    /// `WlOutput` proxy alive in the value so events keep flowing.
    outputs: HashMap<u32, (WlOutput, OutputInfo, Option<ZxdgOutputV1>)>,
    /// Dedicated virtual pointer used only for absolute-position
    /// warps on `Enter`. Separate from per-handle pointers so warp
    /// works regardless of which client is active.
    warp_pointer: Vp,
}

#[derive(Default, Clone, Copy)]
struct OutputInfo {
    /// Position in the compositor's global coordinate space, from
    /// wl_output::Event::Geometry. Raw-pixel coordinates.
    x: i32,
    y: i32,
    /// Pixel dimensions of the active mode, from wl_output::Event::Mode.
    width: i32,
    height: i32,
    /// Logical position in the compositor's coordinate space, from
    /// zxdg_output_v1::Event::LogicalPosition. Reflects software
    /// scaling (e.g. fractional or HiDPI). Falls back to (x, y) when
    /// xdg-output isn't available.
    logical_x: Option<i32>,
    logical_y: Option<i32>,
    /// Logical dimensions, from zxdg_output_v1::Event::LogicalSize.
    /// This is the coordinate space the compositor uses for cursor
    /// positions and the same one the capture side uses, so we
    /// prefer it for `display_bounds()` to keep both sides in sync.
    /// Falls back to (width, height) when xdg-output isn't available.
    logical_width: Option<i32>,
    logical_height: Option<i32>,
}

// App State, implements Dispatch event handlers
pub(crate) struct WlrootsEmulation {
    last_flush_failed: bool,
    state: State,
    queue: EventQueue<State>,
    /// Whether forwarded scroll deltas should be sign-inverted.
    /// Set via Emulation::set_natural_scroll. Default false.
    natural_scroll: bool,
}

impl WlrootsEmulation {
    pub(crate) fn new() -> Result<Self, WlrootsEmulationCreationError> {
        let conn = Connection::connect_to_env()?;
        let (globals, queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();

        let seat: wl_seat::WlSeat = globals
            .bind(&qh, 7..=8, ())
            .map_err(|e| WaylandBindError::new(e, "wl_seat 7..=8"))?;

        let vpm: VpManager = globals
            .bind(&qh, 1..=1, ())
            .map_err(|e| WaylandBindError::new(e, "wlr-virtual-pointer-unstable-v1"))?;
        let vkm: VkManager = globals
            .bind(&qh, 1..=1, ())
            .map_err(|e| WaylandBindError::new(e, "virtual-keyboard-unstable-v1"))?;
        // xdg-output gives us LogicalSize/LogicalPosition — the
        // coordinate space the compositor actually uses (with
        // software/fractional scaling applied). The capture side
        // already reports bounds in this space, so emulation needs
        // it too or warps land on different proportions than the
        // sender computed. Optional: if the compositor doesn't
        // advertise xdg_output_manager we fall back to wl_output's
        // raw mode dimensions.
        let xdg_output_manager: Option<ZxdgOutputManagerV1> = globals.bind(&qh, 1..=3, ()).ok();

        // Bind every advertised wl_output so we receive Geometry +
        // Mode events for each one. Used to compute display_bounds.
        let mut outputs: HashMap<u32, (WlOutput, OutputInfo, Option<ZxdgOutputV1>)> =
            HashMap::new();
        for global in globals.contents().clone_list() {
            if global.interface == "wl_output" {
                // version 2 is enough for Geometry + Mode events.
                let output: WlOutput =
                    globals
                        .registry()
                        .bind(global.name, global.version.min(2), &qh, ());
                let id = output.id().protocol_id();
                let xdg_output = xdg_output_manager
                    .as_ref()
                    .map(|mgr| mgr.get_xdg_output(&output, &qh, id));
                outputs.insert(id, (output, OutputInfo::default(), xdg_output));
            }
        }

        // Dedicated warp pointer — used only for motion_absolute on
        // Enter, so warp works even when no per-handle virtual
        // pointer is currently active.
        let warp_pointer: Vp = vpm.create_virtual_pointer(None, &qh, ());

        let input_for_client: HashMap<EmulationHandle, VirtualInput> = HashMap::new();

        let mut emulate = WlrootsEmulation {
            last_flush_failed: false,
            state: State {
                keymap: None,
                input_for_client,
                seat,
                vpm,
                vkm,
                qh,
                outputs,
                warp_pointer,
            },
            queue,
            natural_scroll: false,
        };
        while emulate.state.keymap.is_none() {
            emulate.queue.blocking_dispatch(&mut emulate.state)?;
        }
        // let fd = unsafe { &File::from_raw_fd(emulate.state.keymap.unwrap().1.as_raw_fd()) };
        // let mmap = unsafe { MmapOptions::new().map_copy(fd).unwrap() };
        // log::debug!("{:?}", &mmap[..100]);
        Ok(emulate)
    }
}

impl State {
    fn add_client(&mut self, client: EmulationHandle) {
        let pointer: Vp = self.vpm.create_virtual_pointer(None, &self.qh, ());
        let keyboard: Vk = self.vkm.create_virtual_keyboard(&self.seat, &self.qh, ());

        // TODO: use server side keymap
        if let Some((format, fd, size)) = self.keymap.as_ref() {
            keyboard.keymap(*format, fd.as_fd(), *size);
        } else {
            panic!("no keymap");
        }

        let vinput = VirtualInput {
            pointer,
            keyboard,
            modifiers: Arc::new(Mutex::new(XMods::empty())),
        };

        self.input_for_client.insert(client, vinput);
    }

    fn destroy_client(&mut self, handle: EmulationHandle) {
        if let Some(input) = self.input_for_client.remove(&handle) {
            input.pointer.destroy();
            input.keyboard.destroy();
        }
    }

    /// Bounding rectangle of every active wl_output in the
    /// compositor's logical coordinate space (with software /
    /// fractional scaling applied). Falls back per-output to raw
    /// mode dimensions when xdg-output is unavailable. Returns
    /// None if no output has reported usable size info yet.
    fn union_bounds(&self) -> Option<(u32, u32)> {
        let mut xmin = i32::MAX;
        let mut ymin = i32::MAX;
        let mut xmax = i32::MIN;
        let mut ymax = i32::MIN;
        let mut any = false;
        for (_, o, _) in self.outputs.values() {
            let w = o.logical_width.unwrap_or(o.width);
            let h = o.logical_height.unwrap_or(o.height);
            if w <= 0 || h <= 0 {
                continue;
            }
            let ox = o.logical_x.unwrap_or(o.x);
            let oy = o.logical_y.unwrap_or(o.y);
            any = true;
            xmin = xmin.min(ox);
            ymin = ymin.min(oy);
            xmax = xmax.max(ox + w);
            ymax = ymax.max(oy + h);
        }
        if !any {
            return None;
        }
        let w = (xmax - xmin) as u32;
        let h = (ymax - ymin) as u32;
        Some((w, h))
    }
}

#[async_trait]
impl Emulation for WlrootsEmulation {
    async fn consume(
        &mut self,
        event: Event,
        handle: EmulationHandle,
    ) -> Result<(), EmulationError> {
        if let Some(virtual_input) = self.state.input_for_client.get(&handle) {
            if self.last_flush_failed {
                match self.queue.flush() {
                    Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {
                        /*
                         * outgoing buffer is full - sending more events
                         * will overwhelm the output buffer and leave the
                         * wayland connection in a broken state
                         */
                        log::warn!("can't keep up, discarding event: ({handle}) - {event:?}");
                        return Ok(());
                    }
                    _ => {}
                }
            }
            virtual_input
                .consume_event(event, self.natural_scroll)
                .unwrap_or_else(|_| panic!("failed to convert event: {event:?}"));
            match self.queue.flush() {
                Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.last_flush_failed = true;
                    log::warn!("can't keep up, discarding event: ({handle}) - {event:?}");
                }
                Err(WaylandError::Protocol(e)) => panic!("wayland protocol violation: {e}"),
                Ok(()) => self.last_flush_failed = false,
                Err(e) => Err(e)?,
            }
        }
        Ok(())
    }

    async fn create(&mut self, handle: EmulationHandle) {
        self.state.add_client(handle);
        if let Err(e) = self.queue.flush() {
            log::error!("{e}");
        }
    }
    async fn destroy(&mut self, handle: EmulationHandle) {
        self.state.destroy_client(handle);
        if let Err(e) = self.queue.flush() {
            log::error!("{e}");
        }
    }
    async fn terminate(&mut self) {
        self.state.warp_pointer.destroy();
    }

    fn display_bounds(&self) -> Option<(u32, u32)> {
        self.state.union_bounds()
    }

    fn set_natural_scroll(&mut self, natural_scroll: bool) {
        self.natural_scroll = natural_scroll;
    }

    async fn warp_cursor(&mut self, x: i32, y: i32) -> Result<(), EmulationError> {
        let Some((width, height)) = self.state.union_bounds() else {
            return Ok(());
        };
        let now: u32 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u32;
        let cx = x.clamp(0, width as i32) as u32;
        let cy = y.clamp(0, height as i32) as u32;
        self.state
            .warp_pointer
            .motion_absolute(now, cx, cy, width, height);
        self.state.warp_pointer.frame();
        if let Err(e) = self.queue.flush() {
            log::warn!("warp_cursor flush failed: {e}");
        }
        Ok(())
    }
}

struct VirtualInput {
    pointer: Vp,
    keyboard: Vk,
    modifiers: Arc<Mutex<XMods>>,
}

impl VirtualInput {
    fn consume_event(&self, event: Event, natural_scroll: bool) -> Result<(), ()> {
        let scroll_sign = if natural_scroll { -1.0 } else { 1.0 };
        let now: u32 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u32;

        match event {
            Event::Pointer(e) => {
                match e {
                    PointerEvent::Motion { time, dx, dy } => self.pointer.motion(time, dx, dy),
                    PointerEvent::Button {
                        time,
                        button,
                        state,
                    } => {
                        let state: ButtonState = state.try_into()?;
                        self.pointer.button(time, button, state);
                    }
                    PointerEvent::Axis {
                        time: _,
                        axis,
                        value,
                    } => {
                        // wl_pointer requires `axis_source` to be sent
                        // alongside the axis event; without it many
                        // compositors (Hyprland, Sway, …) silently
                        // drop continuous scroll. AxisSource::Finger
                        // matches a Mac trackpad gesture, which is the
                        // typical source for continuous scroll
                        // forwarded by Lan Mouse. We also use the
                        // local `now` timestamp because the upstream
                        // CGEventTap path passes time=0 and some
                        // compositors filter zero-time events.
                        let axis_n: u8 = axis;
                        let axis: Axis = (axis as u32).try_into()?;
                        let injected = scroll_sign * value;
                        log::info!(
                            "[SCROLL-DEBUG emit Axis] axis={} wire_value={:.3} scroll_sign={:.1} injected={:.3}",
                            axis_n, value, scroll_sign, injected,
                        );
                        self.pointer.axis(now, axis, injected);
                        self.pointer.axis_source(AxisSource::Finger);
                        self.pointer.frame();
                    }
                    PointerEvent::AxisDiscrete120 { axis, value } => {
                        let axis_n: u8 = axis;
                        let wire_value = value;
                        let axis: Axis = (axis as u32).try_into()?;
                        let value = (scroll_sign as i32) * value;
                        log::info!(
                            "[SCROLL-DEBUG emit AxisDiscrete120] axis={} wire_value={} scroll_sign={:.1} after_sign={} cont_f64={:.3} discrete_i32={}",
                            axis_n, wire_value, scroll_sign, value, value as f64 / 8., value / 120,
                        );
                        self.pointer
                            .axis_discrete(now, axis, value as f64 / 8., value / 120);
                        self.pointer.axis_source(AxisSource::Wheel);
                        self.pointer.frame();
                    }
                }
                self.pointer.frame();
            }
            Event::Keyboard(e) => match e {
                KeyboardEvent::Key { time, key, state } => {
                    self.keyboard.key(time, key, state as u32);
                    if let Ok(mut mods) = self.modifiers.lock() {
                        if mods.update_by_key_event(key, state) {
                            log::trace!("Key triggers modifier change: {mods:?}");
                            self.keyboard.modifiers(
                                mods.mask_pressed().bits(),
                                0,
                                mods.mask_locks().bits(),
                                0,
                            );
                        }
                    }
                }
                KeyboardEvent::Modifiers {
                    depressed: mods_depressed,
                    latched: mods_latched,
                    locked: mods_locked,
                    group,
                } => {
                    // Synchronize internal modifier state, assuming server is authoritative
                    if let Ok(mut mods) = self.modifiers.lock() {
                        mods.update_by_mods_event(e);
                    }
                    self.keyboard
                        .modifiers(mods_depressed, mods_latched, mods_locked, group);
                }
            },
        }
        Ok(())
    }
}

delegate_noop!(State: Vp);
delegate_noop!(State: Vk);
delegate_noop!(State: VpManager);
delegate_noop!(State: VkManager);
delegate_noop!(State: ZxdgOutputManagerV1);

impl Dispatch<ZxdgOutputV1, u32> for State {
    fn event(
        state: &mut Self,
        _: &ZxdgOutputV1,
        event: <ZxdgOutputV1 as wayland_client::Proxy>::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some((_, info, _)) = state.outputs.get_mut(id) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                info.logical_x = Some(x);
                info.logical_y = Some(y);
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                info.logical_width = Some(width);
                info.logical_height = Some(height);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut State,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: <WlKeyboard as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap { format, fd, size } = event {
            state.keymap = Some((u32::from(format), fd, size));
        }
    }
}

impl Dispatch<WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &WlOutput,
        event: <WlOutput as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = output.id().protocol_id();
        let Some((_, info, _)) = state.outputs.get_mut(&id) else {
            return;
        };
        match event {
            wl_output::Event::Geometry { x, y, .. } => {
                info.x = x;
                info.y = y;
            }
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                ..
            } if flags.contains(wl_output::Mode::Current) => {
                info.width = width;
                info.height = height;
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: <WlSeat as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qhandle, ());
            }
        }
    }
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

impl XMods {
    fn update_by_mods_event(&mut self, evt: KeyboardEvent) {
        if let KeyboardEvent::Modifiers {
            depressed, locked, ..
        } = evt
        {
            *self = XMods::from_bits_truncate(depressed) | XMods::from_bits_truncate(locked);
        }
    }

    fn update_by_key_event(&mut self, key: u32, state: u8) -> bool {
        if let Ok(key) = scancode::Linux::try_from(key) {
            log::trace!("Attempting to process modifier from: {key:#?}");
            let pressed_mask = match key {
                scancode::Linux::KeyLeftShift | scancode::Linux::KeyRightShift => XMods::ShiftMask,
                scancode::Linux::KeyLeftCtrl | scancode::Linux::KeyRightCtrl => XMods::ControlMask,
                scancode::Linux::KeyLeftAlt | scancode::Linux::KeyRightalt => XMods::Mod1Mask,
                scancode::Linux::KeyLeftMeta | scancode::Linux::KeyRightmeta => XMods::Mod4Mask,
                _ => XMods::empty(),
            };

            let locked_mask = match key {
                scancode::Linux::KeyCapsLock => XMods::LockMask,
                scancode::Linux::KeyNumlock => XMods::Mod2Mask,
                scancode::Linux::KeyScrollLock => XMods::Mod3Mask,
                _ => XMods::empty(),
            };

            // unchanged
            if pressed_mask.is_empty() && locked_mask.is_empty() {
                log::trace!("{key:#?} is not a modifier key");
                return false;
            }
            match state {
                1 => self.insert(pressed_mask),
                _ => {
                    self.remove(pressed_mask);
                    self.toggle(locked_mask);
                }
            }
            true
        } else {
            false
        }
    }

    fn mask_locks(&self) -> XMods {
        *self & (XMods::LockMask | XMods::Mod2Mask | XMods::Mod3Mask)
    }

    fn mask_pressed(&self) -> XMods {
        *self & (XMods::ShiftMask | XMods::ControlMask | XMods::Mod1Mask | XMods::Mod4Mask)
    }
}
