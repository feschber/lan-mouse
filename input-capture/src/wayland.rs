use async_trait::async_trait;
use futures_core::Stream;
use std::{
    collections::VecDeque,
    env,
    io::{self, ErrorKind},
    os::fd::{AsFd, RawFd},
    pin::Pin,
    task::{ready, Context, Poll},
};
use tokio::io::unix::AsyncFd;

use std::{
    fs::File,
    io::{BufWriter, Write},
    os::unix::prelude::AsRawFd,
    sync::Arc,
};

use wayland_protocols::{
    wp::{
        keyboard_shortcuts_inhibit::zv1::client::{
            zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
            zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
        },
        pointer_constraints::zv1::client::{
            zwp_locked_pointer_v1::ZwpLockedPointerV1,
            zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
        },
        relative_pointer::zv1::client::{
            zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
            zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
        },
    },
    xdg::xdg_output::zv1::client::{
        zxdg_output_manager_v1::ZxdgOutputManagerV1,
        zxdg_output_v1::{self, ZxdgOutputV1},
    },
};

use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use wayland_client::{
    backend::{ReadEventsGuard, WaylandError},
    delegate_noop,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_buffer, wl_compositor,
        wl_keyboard::{self, WlKeyboard},
        wl_output::{self, WlOutput},
        wl_pointer::{self, WlPointer},
        wl_region, wl_registry, wl_seat, wl_shm, wl_shm_pool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, DispatchError, EventQueue, QueueHandle, WEnum,
};

use input_event::{Event, KeyboardEvent, PointerEvent};

use crate::{CaptureError, CaptureEvent};

use super::{
    error::{LayerShellCaptureCreationError, WaylandBindError},
    Capture, Position,
};

struct Globals {
    compositor: wl_compositor::WlCompositor,
    pointer_constraints: ZwpPointerConstraintsV1,
    relative_pointer_manager: ZwpRelativePointerManagerV1,
    shortcut_inhibit_manager: Option<ZwpKeyboardShortcutsInhibitManagerV1>,
    seat: wl_seat::WlSeat,
    shm: wl_shm::WlShm,
    layer_shell: ZwlrLayerShellV1,
    outputs: Vec<WlOutput>,
    xdg_output_manager: ZxdgOutputManagerV1,
}

#[derive(Debug, Clone)]
struct OutputInfo {
    name: String,
    position: (i32, i32),
    size: (i32, i32),
}

impl OutputInfo {
    fn new() -> Self {
        Self {
            name: "".to_string(),
            position: (0, 0),
            size: (0, 0),
        }
    }
}

struct State {
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    pointer_lock: Option<ZwpLockedPointerV1>,
    rel_pointer: Option<ZwpRelativePointerV1>,
    shortcut_inhibitor: Option<ZwpKeyboardShortcutsInhibitorV1>,
    active_windows: Vec<Arc<Window>>,
    focused: Option<Arc<Window>>,
    g: Globals,
    wayland_fd: RawFd,
    read_guard: Option<ReadEventsGuard>,
    qh: QueueHandle<Self>,
    pending_events: VecDeque<(Position, CaptureEvent)>,
    output_info: Vec<(WlOutput, OutputInfo)>,
    scroll_discrete_pending: bool,
}

struct Inner {
    state: State,
    queue: EventQueue<State>,
}

impl AsRawFd for Inner {
    fn as_raw_fd(&self) -> RawFd {
        self.state.wayland_fd
    }
}

pub struct LayerShellInputCapture(AsyncFd<Inner>);

struct Window {
    buffer: wl_buffer::WlBuffer,
    surface: WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    pos: Position,
}

impl Window {
    fn new(
        state: &State,
        qh: &QueueHandle<State>,
        output: &WlOutput,
        pos: Position,
        size: (i32, i32),
    ) -> Window {
        log::debug!("creating window output: {output:?}, size: {size:?}");
        let g = &state.g;

        let (width, height) = match pos {
            Position::Left | Position::Right => (1, size.1 as u32),
            Position::Top | Position::Bottom => (size.0 as u32, 1),
        };
        let mut file = tempfile::tempfile().unwrap();
        draw(&mut file, (width, height));
        let pool = g
            .shm
            .create_pool(file.as_fd(), (width * height * 4) as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            (width * 4) as i32,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );
        let surface = g.compositor.create_surface(qh, ());

        let layer_surface = g.layer_shell.get_layer_surface(
            &surface,
            Some(output),
            Layer::Overlay,
            "LAN Mouse Sharing".into(),
            qh,
            (),
        );
        let anchor = match pos {
            Position::Left => Anchor::Left,
            Position::Right => Anchor::Right,
            Position::Top => Anchor::Top,
            Position::Bottom => Anchor::Bottom,
        };

        layer_surface.set_anchor(anchor);
        layer_surface.set_size(width, height);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_margin(0, 0, 0, 0);
        surface.set_input_region(None);
        surface.commit();
        Window {
            pos,
            buffer,
            surface,
            layer_surface,
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        log::debug!("destroying window!");
        self.layer_surface.destroy();
        self.surface.destroy();
        self.buffer.destroy();
    }
}

fn get_edges(outputs: &[(WlOutput, OutputInfo)], pos: Position) -> Vec<(WlOutput, i32)> {
    outputs
        .iter()
        .map(|(o, i)| {
            (
                o.clone(),
                match pos {
                    Position::Left => i.position.0,
                    Position::Right => i.position.0 + i.size.0,
                    Position::Top => i.position.1,
                    Position::Bottom => i.position.1 + i.size.1,
                },
            )
        })
        .collect()
}

fn get_output_configuration(state: &State, pos: Position) -> Vec<(WlOutput, OutputInfo)> {
    // get all output edges corresponding to the position
    let edges = get_edges(&state.output_info, pos);
    log::debug!("edges: {edges:?}");
    let opposite_edges = get_edges(&state.output_info, pos.opposite());

    // remove those edges that are at the same position
    // as an opposite edge of a different output
    let outputs: Vec<WlOutput> = edges
        .iter()
        .filter(|(_, edge)| !opposite_edges.iter().map(|(_, e)| *e).any(|e| &e == edge))
        .map(|(o, _)| o.clone())
        .collect();
    state
        .output_info
        .iter()
        .filter(|(o, _)| outputs.contains(o))
        .map(|(o, i)| (o.clone(), i.clone()))
        .collect()
}

fn draw(f: &mut File, (width, height): (u32, u32)) {
    let mut buf = BufWriter::new(f);
    for _ in 0..height {
        for _ in 0..width {
            if env::var("LM_DEBUG_LAYER_SHELL").ok().is_some() {
                // AARRGGBB
                buf.write_all(&0xff11d116u32.to_ne_bytes()).unwrap();
            } else {
                // AARRGGBB
                buf.write_all(&0x00000000u32.to_ne_bytes()).unwrap();
            }
        }
    }
}

impl LayerShellInputCapture {
    pub fn new() -> std::result::Result<Self, LayerShellCaptureCreationError> {
        let conn = Connection::connect_to_env()?;
        let (g, mut queue) = registry_queue_init::<State>(&conn)?;

        let qh = queue.handle();

        let compositor: wl_compositor::WlCompositor = g
            .bind(&qh, 4..=5, ())
            .map_err(|e| WaylandBindError::new(e, "wl_compositor 4..=5"))?;
        let xdg_output_manager: ZxdgOutputManagerV1 = g
            .bind(&qh, 1..=3, ())
            .map_err(|e| WaylandBindError::new(e, "xdg_output_manager 1..=3"))?;
        let shm: wl_shm::WlShm = g
            .bind(&qh, 1..=1, ())
            .map_err(|e| WaylandBindError::new(e, "wl_shm"))?;
        let layer_shell: ZwlrLayerShellV1 = g
            .bind(&qh, 3..=4, ())
            .map_err(|e| WaylandBindError::new(e, "wlr_layer_shell 3..=4"))?;
        let seat: wl_seat::WlSeat = g
            .bind(&qh, 7..=8, ())
            .map_err(|e| WaylandBindError::new(e, "wl_seat 7..=8"))?;

        let pointer_constraints: ZwpPointerConstraintsV1 = g
            .bind(&qh, 1..=1, ())
            .map_err(|e| WaylandBindError::new(e, "zwp_pointer_constraints_v1"))?;
        let relative_pointer_manager: ZwpRelativePointerManagerV1 = g
            .bind(&qh, 1..=1, ())
            .map_err(|e| WaylandBindError::new(e, "zwp_relative_pointer_manager_v1"))?;
        let shortcut_inhibit_manager: Result<
            ZwpKeyboardShortcutsInhibitManagerV1,
            WaylandBindError,
        > = g
            .bind(&qh, 1..=1, ())
            .map_err(|e| WaylandBindError::new(e, "zwp_keyboard_shortcuts_inhibit_manager_v1"));
        // layer-shell backend still works without this protocol so we make it an optional dependency
        if let Err(e) = &shortcut_inhibit_manager {
            log::warn!("shortcut_inhibit_manager not supported: {e}\nkeybinds handled by the compositor will not be passed
                to the client");
        }
        let shortcut_inhibit_manager = shortcut_inhibit_manager.ok();
        let outputs = vec![];

        let g = Globals {
            compositor,
            shm,
            layer_shell,
            seat,
            pointer_constraints,
            relative_pointer_manager,
            shortcut_inhibit_manager,
            outputs,
            xdg_output_manager,
        };

        // flush outgoing events
        queue.flush()?;

        let wayland_fd = queue.as_fd().as_raw_fd();

        let mut state = State {
            pointer: None,
            keyboard: None,
            g,
            pointer_lock: None,
            rel_pointer: None,
            shortcut_inhibitor: None,
            active_windows: Vec::new(),
            focused: None,
            qh,
            wayland_fd,
            read_guard: None,
            pending_events: VecDeque::new(),
            output_info: vec![],
            scroll_discrete_pending: false,
        };

        // dispatch registry to () again, in order to read all wl_outputs
        conn.display().get_registry(&state.qh, ());
        log::debug!("==============> requested registry");

        // roundtrip to read wl_output globals
        queue.roundtrip(&mut state)?;
        log::debug!("==============> roundtrip 1 done");

        // read outputs
        for output in state.g.outputs.iter() {
            state
                .g
                .xdg_output_manager
                .get_xdg_output(output, &state.qh, output.clone());
        }

        // roundtrip to read xdg_output events
        queue.roundtrip(&mut state)?;

        log::debug!("==============> roundtrip 2 done");
        for i in &state.output_info {
            log::debug!("{:#?}", i.1);
        }

        let read_guard = loop {
            match queue.prepare_read() {
                Some(r) => break r,
                None => {
                    queue.dispatch_pending(&mut state)?;
                    continue;
                }
            }
        };
        state.read_guard = Some(read_guard);

        let inner = AsyncFd::new(Inner { queue, state })?;

        Ok(LayerShellInputCapture(inner))
    }

    fn add_client(&mut self, pos: Position) {
        self.0.get_mut().state.add_client(pos);
    }

    fn delete_client(&mut self, pos: Position) {
        let inner = self.0.get_mut();
        // remove all windows corresponding to this client
        while let Some(i) = inner.state.active_windows.iter().position(|w| w.pos == pos) {
            inner.state.active_windows.remove(i);
            inner.state.focused = None;
        }
    }
}

impl State {
    fn grab(
        &mut self,
        surface: &WlSurface,
        pointer: &WlPointer,
        serial: u32,
        qh: &QueueHandle<State>,
    ) {
        let window = self.focused.as_ref().unwrap();

        // hide the cursor
        pointer.set_cursor(serial, None, 0, 0);

        // capture input
        window
            .layer_surface
            .set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        window.surface.commit();

        // lock pointer
        if self.pointer_lock.is_none() {
            self.pointer_lock = Some(self.g.pointer_constraints.lock_pointer(
                surface,
                pointer,
                None,
                Lifetime::Persistent,
                qh,
                (),
            ));
        }

        // request relative input
        if self.rel_pointer.is_none() {
            self.rel_pointer = Some(self.g.relative_pointer_manager.get_relative_pointer(
                pointer,
                qh,
                (),
            ));
        }

        // capture modifier keys
        if let Some(shortcut_inhibit_manager) = &self.g.shortcut_inhibit_manager {
            if self.shortcut_inhibitor.is_none() {
                self.shortcut_inhibitor =
                    Some(shortcut_inhibit_manager.inhibit_shortcuts(surface, &self.g.seat, qh, ()));
            }
        }
    }

    fn ungrab(&mut self) {
        // get focused client
        let window = match self.focused.as_ref() {
            Some(focused) => focused,
            None => return,
        };

        // ungrab surface
        window
            .layer_surface
            .set_keyboard_interactivity(KeyboardInteractivity::None);
        window.surface.commit();

        // destroy pointer lock
        if let Some(pointer_lock) = &self.pointer_lock {
            pointer_lock.destroy();
            self.pointer_lock = None;
        }

        // destroy relative input
        if let Some(rel_pointer) = &self.rel_pointer {
            rel_pointer.destroy();
            self.rel_pointer = None;
        }

        // destroy shortcut inhibitor
        if let Some(shortcut_inhibitor) = &self.shortcut_inhibitor {
            shortcut_inhibitor.destroy();
            self.shortcut_inhibitor = None;
        }
    }

    fn add_client(&mut self, pos: Position) {
        let outputs = get_output_configuration(self, pos);

        log::debug!("outputs: {outputs:?}");
        outputs.iter().for_each(|(o, i)| {
            let window = Window::new(self, &self.qh, o, pos, i.size);
            let window = Arc::new(window);
            self.active_windows.push(window);
        });
    }

    fn update_windows(&mut self) {
        log::debug!("updating windows");
        log::debug!("output info: {:?}", self.output_info);
        let clients: Vec<_> = self.active_windows.drain(..).map(|w| w.pos).collect();
        for pos in clients {
            self.add_client(pos);
        }
    }
}

impl Inner {
    fn read(&mut self) -> bool {
        match self.state.read_guard.take().unwrap().read() {
            Ok(_) => true,
            Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => false,
            Err(WaylandError::Io(e)) => {
                log::error!("error reading from wayland socket: {e}");
                false
            }
            Err(WaylandError::Protocol(e)) => {
                panic!("wayland protocol violation: {e}")
            }
        }
    }

    fn prepare_read(&mut self) -> io::Result<()> {
        loop {
            match self.queue.prepare_read() {
                None => match self.queue.dispatch_pending(&mut self.state) {
                    Ok(_) => continue,
                    Err(DispatchError::Backend(WaylandError::Io(e))) => return Err(e),
                    Err(e) => panic!("failed to dispatch wayland events: {e}"),
                },
                Some(r) => {
                    self.state.read_guard = Some(r);
                    break Ok(());
                }
            }
        }
    }

    fn dispatch_events(&mut self) {
        match self.queue.dispatch_pending(&mut self.state) {
            Ok(_) => {}
            Err(DispatchError::Backend(WaylandError::Io(e))) => {
                log::error!("Wayland Error: {}", e);
            }
            Err(DispatchError::Backend(e)) => {
                panic!("backend error: {}", e);
            }
            Err(DispatchError::BadMessage {
                sender_id,
                interface,
                opcode,
            }) => {
                panic!("bad message {}, {} , {}", sender_id, interface, opcode);
            }
        }
    }

    fn flush_events(&mut self) -> io::Result<()> {
        // flush outgoing events
        match self.queue.flush() {
            Ok(_) => (),
            Err(e) => match e {
                WaylandError::Io(e) => {
                    return Err(e);
                }
                WaylandError::Protocol(e) => {
                    panic!("wayland protocol violation: {e}")
                }
            },
        }
        Ok(())
    }
}

#[async_trait]
impl Capture for LayerShellInputCapture {
    async fn create(&mut self, pos: Position) -> Result<(), CaptureError> {
        self.add_client(pos);
        let inner = self.0.get_mut();
        Ok(inner.flush_events()?)
    }

    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError> {
        self.delete_client(pos);
        let inner = self.0.get_mut();
        Ok(inner.flush_events()?)
    }

    async fn release(&mut self) -> Result<(), CaptureError> {
        log::debug!("releasing pointer");
        let inner = self.0.get_mut();
        inner.state.ungrab();
        Ok(inner.flush_events()?)
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }
}

impl Stream for LayerShellInputCapture {
    type Item = Result<(Position, CaptureEvent), CaptureError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(event) = self.0.get_mut().state.pending_events.pop_front() {
            return Poll::Ready(Some(Ok(event)));
        }

        loop {
            let mut guard = ready!(self.0.poll_read_ready_mut(cx))?;

            {
                let inner = guard.get_inner_mut();

                // read events
                while inner.read() {
                    // prepare next read
                    match inner.prepare_read() {
                        Ok(_) => {}
                        Err(e) => return Poll::Ready(Some(Err(e.into()))),
                    }
                }

                // dispatch the events
                inner.dispatch_events();

                // flush outgoing events
                if let Err(e) = inner.flush_events() {
                    if e.kind() != ErrorKind::WouldBlock {
                        return Poll::Ready(Some(Err(e.into())));
                    }
                }

                // prepare for the next read
                match inner.prepare_read() {
                    Ok(_) => {}
                    Err(e) => return Poll::Ready(Some(Err(e.into()))),
                }
            }

            // clear read readiness for tokio read guard
            // guard.clear_ready_matching(Ready::READABLE);
            guard.clear_ready();

            // if an event has been queued during dispatch_events() we return it
            match guard.get_inner_mut().state.pending_events.pop_front() {
                Some(event) => return Poll::Ready(Some(Ok(event))),
                None => continue,
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: <wl_seat::WlSeat as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Pointer) {
                if let Some(p) = state.pointer.take() {
                    p.release();
                }
                state.pointer.replace(seat.get_pointer(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                if let Some(k) = state.keyboard.take() {
                    k.release();
                }
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        app: &mut Self,
        pointer: &WlPointer,
        event: <WlPointer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x: _,
                surface_y: _,
            } => {
                // get client corresponding to the focused surface
                {
                    if let Some(window) = app.active_windows.iter().find(|w| w.surface == surface) {
                        app.focused = Some(window.clone());
                        app.grab(&surface, pointer, serial, qh);
                    } else {
                        return;
                    }
                }
                let pos = app
                    .active_windows
                    .iter()
                    .find(|w| w.surface == surface)
                    .map(|w| w.pos)
                    .unwrap();
                app.pending_events.push_back((pos, CaptureEvent::Begin));
            }
            wl_pointer::Event::Leave { .. } => {
                /* There are rare cases, where when a window is opened in
                 * just the wrong moment, the pointer is released, while
                 * still grabbed.
                 * In that case, the pointer must be ungrabbed, otherwise
                 * it is impossible to grab it again (since the pointer
                 * lock, relative pointer,... objects are still in place)
                 */
                if app.pointer_lock.is_some() {
                    log::warn!("compositor released mouse");
                }
                app.ungrab();
            }
            wl_pointer::Event::Button {
                serial: _,
                time,
                button,
                state,
            } => {
                let window = app.focused.as_ref().unwrap();
                app.pending_events.push_back((
                    window.pos,
                    CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                        time,
                        button,
                        state: u32::from(state),
                    })),
                ));
            }
            wl_pointer::Event::Axis { time, axis, value } => {
                let window = app.focused.as_ref().unwrap();
                if app.scroll_discrete_pending {
                    // each axisvalue120 event is coupled with
                    // a corresponding axis event, which needs to
                    // be ignored to not duplicate the scrolling
                    app.scroll_discrete_pending = false;
                } else {
                    app.pending_events.push_back((
                        window.pos,
                        CaptureEvent::Input(Event::Pointer(PointerEvent::Axis {
                            time,
                            axis: u32::from(axis) as u8,
                            value,
                        })),
                    ));
                }
            }
            wl_pointer::Event::AxisValue120 { axis, value120 } => {
                let window = app.focused.as_ref().unwrap();
                app.scroll_discrete_pending = true;
                app.pending_events.push_back((
                    window.pos,
                    CaptureEvent::Input(Event::Pointer(PointerEvent::AxisDiscrete120 {
                        axis: u32::from(axis) as u8,
                        value: value120,
                    })),
                ));
            }
            wl_pointer::Event::Frame {} => {
                // TODO properly handle frame events
                // we simply insert a frame event on the client side
                // after each event for now
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        app: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let window = &app.focused;
        match event {
            wl_keyboard::Event::Key {
                serial: _,
                time,
                key,
                state,
            } => {
                if let Some(window) = window {
                    app.pending_events.push_back((
                        window.pos,
                        CaptureEvent::Input(Event::Keyboard(KeyboardEvent::Key {
                            time,
                            key,
                            state: u32::from(state) as u8,
                        })),
                    ));
                }
            }
            wl_keyboard::Event::Modifiers {
                serial: _,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                if let Some(window) = window {
                    app.pending_events.push_back((
                        window.pos,
                        CaptureEvent::Input(Event::Keyboard(KeyboardEvent::Modifiers {
                            depressed: mods_depressed,
                            latched: mods_latched,
                            locked: mods_locked,
                            group,
                        })),
                    ));
                }
            }
            _ => (),
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for State {
    fn event(
        app: &mut Self,
        _: &ZwpRelativePointerV1,
        event: <ZwpRelativePointerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion {
            utime_hi,
            utime_lo,
            dx_unaccel: dx,
            dy_unaccel: dy,
            ..
        } = event
        {
            if let Some(window) = &app.focused {
                let time = (((utime_hi as u64) << 32 | utime_lo as u64) / 1000) as u32;
                app.pending_events.push_back((
                    window.pos,
                    CaptureEvent::Input(Event::Pointer(PointerEvent::Motion { time, dx, dy })),
                ));
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        app: &mut Self,
        layer_surface: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            if let Some(window) = app
                .active_windows
                .iter()
                .find(|w| &w.layer_surface == layer_surface)
            {
                // client corresponding to the layer_surface
                let surface = &window.surface;
                let buffer = &window.buffer;
                surface.attach(Some(buffer), 0, 0);
                layer_surface.ack_configure(serial);
                surface.commit();
            }
        }
    }
}

// delegate wl_registry events to App itself
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: <wl_registry::WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: <wl_registry::WlRegistry as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version: _,
            } => {
                if interface.as_str() == "wl_output" {
                    log::debug!("wl_output global");
                    state
                        .g
                        .outputs
                        .push(registry.bind::<WlOutput, _, _>(name, 4, qh, ()))
                }
            }
            wl_registry::Event::GlobalRemove { .. } => {}
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputV1, WlOutput> for State {
    fn event(
        state: &mut Self,
        _: &ZxdgOutputV1,
        event: <ZxdgOutputV1 as wayland_client::Proxy>::Event,
        wl_output: &WlOutput,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        log::debug!("xdg-output - {event:?}");
        let output_info = match state.output_info.iter_mut().find(|(o, _)| o == wl_output) {
            Some((_, c)) => c,
            None => {
                let output_info = OutputInfo::new();
                state.output_info.push((wl_output.clone(), output_info));
                &mut state.output_info.last_mut().unwrap().1
            }
        };

        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                output_info.position = (x, y);
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                output_info.size = (width, height);
            }
            zxdg_output_v1::Event::Done => {}
            zxdg_output_v1::Event::Name { name } => {
                output_info.name = name;
            }
            zxdg_output_v1::Event::Description { .. } => {}
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        _proxy: &WlOutput,
        event: <WlOutput as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Done = event {
            state.update_windows();
        }
    }
}

// don't emit any events
delegate_noop!(State: wl_region::WlRegion);
delegate_noop!(State: wl_shm_pool::WlShmPool);
delegate_noop!(State: wl_compositor::WlCompositor);
delegate_noop!(State: ZwlrLayerShellV1);
delegate_noop!(State: ZwpRelativePointerManagerV1);
delegate_noop!(State: ZwpKeyboardShortcutsInhibitManagerV1);
delegate_noop!(State: ZwpPointerConstraintsV1);

// ignore events
delegate_noop!(State: ignore ZxdgOutputManagerV1);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore ZwpKeyboardShortcutsInhibitorV1);
delegate_noop!(State: ignore ZwpLockedPointerV1);
