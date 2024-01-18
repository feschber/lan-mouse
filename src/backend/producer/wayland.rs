use crate::{
    client::{ClientEvent, ClientHandle, Position},
    producer::EventProducer,
};

use anyhow::{anyhow, Result};
use futures_core::Stream;
use memmap::MmapOptions;
use std::{
    collections::VecDeque,
    env,
    io::{self, ErrorKind},
    os::fd::{AsFd, OwnedFd, RawFd},
    pin::Pin,
    task::{ready, Context, Poll},
};
use tokio::io::unix::AsyncFd;

use std::{
    fs::File,
    io::{BufWriter, Write},
    os::unix::prelude::{AsRawFd, FromRawFd},
    rc::Rc,
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
        wl_buffer, wl_compositor, wl_keyboard,
        wl_output::{self, WlOutput},
        wl_pointer, wl_region, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
    },
    Connection, Dispatch, DispatchError, EventQueue, QueueHandle, WEnum,
};

use tempfile;

use crate::event::{Event, KeyboardEvent, PointerEvent};

struct Globals {
    compositor: wl_compositor::WlCompositor,
    pointer_constraints: ZwpPointerConstraintsV1,
    relative_pointer_manager: ZwpRelativePointerManagerV1,
    shortcut_inhibit_manager: ZwpKeyboardShortcutsInhibitManagerV1,
    seat: wl_seat::WlSeat,
    shm: wl_shm::WlShm,
    layer_shell: ZwlrLayerShellV1,
    outputs: Vec<wl_output::WlOutput>,
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
    pointer_lock: Option<ZwpLockedPointerV1>,
    rel_pointer: Option<ZwpRelativePointerV1>,
    shortcut_inhibitor: Option<ZwpKeyboardShortcutsInhibitorV1>,
    client_for_window: Vec<(Rc<Window>, ClientHandle)>,
    focused: Option<(Rc<Window>, ClientHandle)>,
    g: Globals,
    wayland_fd: OwnedFd,
    read_guard: Option<ReadEventsGuard>,
    qh: QueueHandle<Self>,
    pending_events: VecDeque<(ClientHandle, Event)>,
    output_info: Vec<(WlOutput, OutputInfo)>,
}

struct Inner {
    state: State,
    queue: EventQueue<State>,
}

impl AsRawFd for Inner {
    fn as_raw_fd(&self) -> RawFd {
        self.state.wayland_fd.as_raw_fd()
    }
}

pub struct WaylandEventProducer(AsyncFd<Inner>);

struct Window {
    buffer: wl_buffer::WlBuffer,
    surface: wl_surface::WlSurface,
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

impl WaylandEventProducer {
    pub fn new() -> Result<Self> {
        let conn = match Connection::connect_to_env() {
            Ok(c) => c,
            Err(e) => return Err(anyhow!("could not connect to wayland compositor: {e:?}")),
        };

        let (g, mut queue) = match registry_queue_init::<State>(&conn) {
            Ok(q) => q,
            Err(e) => return Err(anyhow!("failed to initialize wl_registry: {e:?}")),
        };

        let qh = queue.handle();

        let compositor: wl_compositor::WlCompositor = match g.bind(&qh, 4..=5, ()) {
            Ok(compositor) => compositor,
            Err(_) => return Err(anyhow!("wl_compositor >= v4 not supported")),
        };

        let xdg_output_manager: ZxdgOutputManagerV1 = match g.bind(&qh, 1..=3, ()) {
            Ok(xdg_output_manager) => xdg_output_manager,
            Err(_) => return Err(anyhow!("xdg_output not supported!")),
        };

        let shm: wl_shm::WlShm = match g.bind(&qh, 1..=1, ()) {
            Ok(wl_shm) => wl_shm,
            Err(_) => return Err(anyhow!("wl_shm v1 not supported")),
        };

        let layer_shell: ZwlrLayerShellV1 = match g.bind(&qh, 3..=4, ()) {
            Ok(layer_shell) => layer_shell,
            Err(_) => return Err(anyhow!("zwlr_layer_shell_v1 >= v3 not supported - required to display a surface at the edge of the screen")),
        };

        let seat: wl_seat::WlSeat = match g.bind(&qh, 7..=8, ()) {
            Ok(wl_seat) => wl_seat,
            Err(_) => return Err(anyhow!("wl_seat >= v7 not supported")),
        };

        let pointer_constraints: ZwpPointerConstraintsV1 = match g.bind(&qh, 1..=1, ()) {
            Ok(pointer_constraints) => pointer_constraints,
            Err(_) => return Err(anyhow!("zwp_pointer_constraints_v1 not supported")),
        };

        let relative_pointer_manager: ZwpRelativePointerManagerV1 = match g.bind(&qh, 1..=1, ()) {
            Ok(relative_pointer_manager) => relative_pointer_manager,
            Err(_) => return Err(anyhow!("zwp_relative_pointer_manager_v1 not supported")),
        };

        let shortcut_inhibit_manager: ZwpKeyboardShortcutsInhibitManagerV1 =
            match g.bind(&qh, 1..=1, ()) {
                Ok(shortcut_inhibit_manager) => shortcut_inhibit_manager,
                Err(_) => {
                    return Err(anyhow!(
                        "zwp_keyboard_shortcuts_inhibit_manager_v1 not supported"
                    ))
                }
            };

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

        // prepare reading wayland events
        let read_guard = queue.prepare_read().unwrap(); // there can not yet be events to dispatch
        let wayland_fd = read_guard.connection_fd().try_clone_to_owned().unwrap();
        std::mem::drop(read_guard);

        let mut state = State {
            g,
            pointer_lock: None,
            rel_pointer: None,
            shortcut_inhibitor: None,
            client_for_window: Vec::new(),
            focused: None,
            qh,
            wayland_fd,
            read_guard: None,
            pending_events: VecDeque::new(),
            output_info: vec![],
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

        Ok(WaylandEventProducer(inner))
    }

    fn add_client(&mut self, handle: ClientHandle, pos: Position) {
        self.0.get_mut().state.add_client(handle, pos);
    }

    fn delete_client(&mut self, handle: ClientHandle) {
        let inner = self.0.get_mut();
        // remove all windows corresponding to this client
        while let Some(i) = inner
            .state
            .client_for_window
            .iter()
            .position(|(_, c)| *c == handle)
        {
            inner.state.client_for_window.remove(i);
            inner.state.focused = None;
        }
    }
}

impl State {
    fn grab(
        &mut self,
        surface: &wl_surface::WlSurface,
        pointer: &wl_pointer::WlPointer,
        serial: u32,
        qh: &QueueHandle<State>,
    ) {
        let (window, _) = self.focused.as_ref().unwrap();

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
        if self.shortcut_inhibitor.is_none() {
            self.shortcut_inhibitor = Some(self.g.shortcut_inhibit_manager.inhibit_shortcuts(
                surface,
                &self.g.seat,
                qh,
                (),
            ));
        }
    }

    fn ungrab(&mut self) {
        // get focused client
        let (window, _client) = match self.focused.as_ref() {
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

    fn add_client(&mut self, client: ClientHandle, pos: Position) {
        let outputs = get_output_configuration(self, pos);

        outputs.iter().for_each(|(o, i)| {
            let window = Window::new(self, &self.qh, o, pos, i.size);
            let window = Rc::new(window);
            self.client_for_window.push((window, client));
        });
    }

    fn update_windows(&mut self) {
        log::debug!("updating windows");
        log::debug!("output info: {:?}", self.output_info);
        let clients: Vec<_> = self
            .client_for_window
            .drain(..)
            .map(|(w, c)| (c, w.pos))
            .collect();
        for (client, pos) in clients {
            self.add_client(client, pos);
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

impl EventProducer for WaylandEventProducer {
    fn notify(&mut self, client_event: ClientEvent) -> io::Result<()> {
        match client_event {
            ClientEvent::Create(handle, pos) => {
                self.add_client(handle, pos);
            }
            ClientEvent::Destroy(handle) => {
                self.delete_client(handle);
            }
        }
        let inner = self.0.get_mut();
        inner.flush_events()
    }

    fn release(&mut self) -> io::Result<()> {
        log::debug!("releasing pointer");
        let inner = self.0.get_mut();
        inner.state.ungrab();
        inner.flush_events()
    }
}

impl Stream for WaylandEventProducer {
    type Item = io::Result<(ClientHandle, Event)>;

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
                        Err(e) => return Poll::Ready(Some(Err(e))),
                    }
                }

                // dispatch the events
                inner.dispatch_events();

                // flush outgoing events
                if let Err(e) = inner.flush_events() {
                    if e.kind() != ErrorKind::WouldBlock {
                        return Poll::Ready(Some(Err(e)));
                    }
                }

                // prepare for the next read
                match inner.prepare_read() {
                    Ok(_) => {}
                    Err(e) => return Poll::Ready(Some(Err(e))),
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
        _: &mut Self,
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
                seat.get_pointer(qh, ());
            }
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        app: &mut Self,
        pointer: &wl_pointer::WlPointer,
        event: <wl_pointer::WlPointer as wayland_client::Proxy>::Event,
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
                    if let Some((window, client)) = app
                        .client_for_window
                        .iter()
                        .find(|(w, _c)| w.surface == surface)
                    {
                        app.focused = Some((window.clone(), *client));
                        app.grab(&surface, pointer, serial, qh);
                    } else {
                        return;
                    }
                }
                let (_, client) = app
                    .client_for_window
                    .iter()
                    .find(|(w, _c)| w.surface == surface)
                    .unwrap();
                app.pending_events.push_back((*client, Event::Enter()));
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
                let (_, client) = app.focused.as_ref().unwrap();
                app.pending_events.push_back((
                    *client,
                    Event::Pointer(PointerEvent::Button {
                        time,
                        button,
                        state: u32::from(state),
                    }),
                ));
            }
            wl_pointer::Event::Axis { time, axis, value } => {
                let (_, client) = app.focused.as_ref().unwrap();
                app.pending_events.push_back((
                    *client,
                    Event::Pointer(PointerEvent::Axis {
                        time,
                        axis: u32::from(axis) as u8,
                        value,
                    }),
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

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        app: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let (_window, client) = match &app.focused {
            Some(focused) => (Some(&focused.0), Some(&focused.1)),
            None => (None, None),
        };
        match event {
            wl_keyboard::Event::Key {
                serial: _,
                time,
                key,
                state,
            } => {
                if let Some(client) = client {
                    app.pending_events.push_back((
                        *client,
                        Event::Keyboard(KeyboardEvent::Key {
                            time,
                            key,
                            state: u32::from(state) as u8,
                        }),
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
                if let Some(client) = client {
                    app.pending_events.push_back((
                        *client,
                        Event::Keyboard(KeyboardEvent::Modifiers {
                            mods_depressed,
                            mods_latched,
                            mods_locked,
                            group,
                        }),
                    ));
                }
            }
            wl_keyboard::Event::Keymap {
                format: _,
                fd,
                size: _,
            } => {
                let fd = unsafe { &File::from_raw_fd(fd.as_raw_fd()) };
                let _mmap = unsafe { MmapOptions::new().map_copy(fd).unwrap() };
                // TODO keymap
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
            dx: _,
            dy: _,
            dx_unaccel: surface_x,
            dy_unaccel: surface_y,
        } = event
        {
            if let Some((_window, client)) = &app.focused {
                let time = (((utime_hi as u64) << 32 | utime_lo as u64) / 1000) as u32;
                app.pending_events.push_back((
                    *client,
                    Event::Pointer(PointerEvent::Motion {
                        time,
                        relative_x: surface_x,
                        relative_y: surface_y,
                    }),
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
            if let Some((window, _client)) = app
                .client_for_window
                .iter()
                .find(|(w, _c)| &w.layer_surface == layer_surface)
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
                        .push(registry.bind::<wl_output::WlOutput, _, _>(name, 4, qh, ()))
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

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        _proxy: &wl_output::WlOutput,
        event: <wl_output::WlOutput as wayland_client::Proxy>::Event,
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
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore ZwpKeyboardShortcutsInhibitorV1);
delegate_noop!(State: ignore ZwpLockedPointerV1);
