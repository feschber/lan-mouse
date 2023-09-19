use crate::{client::{ClientHandle, Position, ClientEvent}, producer::EventProducer};
use mio::{event::Source, unix::SourceFd};

use std::{os::fd::RawFd, vec::Drain, io::ErrorKind};
use memmap::MmapOptions;
use anyhow::{anyhow, Result};

use std::{
    fs::File,
    io::{BufWriter, Write},
    os::unix::prelude::{AsRawFd, FromRawFd},
    rc::Rc,
};

use wayland_protocols::wp::{
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
};

use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

use wayland_client::{
    backend::{WaylandError, ReadEventsGuard},
    delegate_noop,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_region, wl_registry, wl_seat, wl_shm,
        wl_shm_pool, wl_surface,
    },
    Connection, Dispatch, DispatchError, QueueHandle, WEnum, EventQueue,
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
}

struct State {
    pointer_lock: Option<ZwpLockedPointerV1>,
    rel_pointer: Option<ZwpRelativePointerV1>,
    shortcut_inhibitor: Option<ZwpKeyboardShortcutsInhibitorV1>,
    client_for_window: Vec<(Rc<Window>, ClientHandle)>,
    focused: Option<(Rc<Window>, ClientHandle)>,
    g: Globals,
    wayland_fd: RawFd,
    read_guard: Option<ReadEventsGuard>,
    qh: QueueHandle<Self>,
    pending_events: Vec<(ClientHandle, Event)>,
}

pub struct WaylandEventProducer {
    state: State,
    queue: EventQueue<State>,
}

struct Window {
    buffer: wl_buffer::WlBuffer,
    surface: wl_surface::WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
}

impl Window {
    fn new(g: &Globals, qh: &QueueHandle<State>, pos: Position) -> Window {
        let (width, height) = (1, 1440);
        let mut file = tempfile::tempfile().unwrap();
        draw(&mut file, (width, height));
        let pool = g
            .shm
            .create_pool(file.as_raw_fd(), (width * height * 4) as i32, qh, ());
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
            None,
            Layer::Top,
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
        layer_surface.set_size(1, 1440);
        layer_surface.set_exclusive_zone(0);
        layer_surface.set_margin(0, 0, 0, 0);
        surface.set_input_region(None);
        surface.commit();
        Window {
            buffer,
            surface,
            layer_surface,
        }
    }
}

fn draw(f: &mut File, (width, height): (u32, u32)) {
    let mut buf = BufWriter::new(f);
    for _ in 0..height {
        for _ in 0..width {
            buf.write_all(&0x44FbF1C7u32.to_ne_bytes()).unwrap();
        }
    }
}

impl WaylandEventProducer {
    pub fn new() -> Result<Self> {
        let conn = Connection::connect_to_env().expect("could not connect to wayland compositor");
        let (g, queue) =
            registry_queue_init::<State>(&conn).expect("failed to initialize wl_registry");
        let qh = queue.handle();

        let compositor: wl_compositor::WlCompositor = match g.bind(&qh, 4..=5, ()) {
            Ok(compositor) => compositor,
            Err(_) => return Err(anyhow!("wl_compositor >= v4 not supported")),
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

        let shortcut_inhibit_manager: ZwpKeyboardShortcutsInhibitManagerV1 = match g.bind(&qh, 1..=1, ()) {
            Ok(shortcut_inhibit_manager) => shortcut_inhibit_manager,
            Err(_) => return Err(anyhow!("zwp_keyboard_shortcuts_inhibit_manager_v1 not supported")),
        };

        let g = Globals {
            compositor,
            shm,
            layer_shell,
            seat,
            pointer_constraints,
            relative_pointer_manager,
            shortcut_inhibit_manager,
        };

        // flush outgoing events
        queue.flush()?;

        // prepare reading wayland events
        let read_guard = queue.prepare_read()?;
        let wayland_fd = read_guard.connection_fd().as_raw_fd();
        let read_guard = Some(read_guard);

        Ok(WaylandEventProducer {
            queue,
            state: State {
                g,
                pointer_lock: None,
                rel_pointer: None,
                shortcut_inhibitor: None,
                client_for_window: Vec::new(),
                focused: None,
                qh,
                wayland_fd,
                read_guard,
                pending_events: vec![],
            }
        })
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

        // release pointer
        if let Some(pointer_lock) = &self.pointer_lock {
            pointer_lock.destroy();
            self.pointer_lock = None;
        }

        // destroy relative input
        if let Some(rel_pointer) = &self.rel_pointer {
            rel_pointer.destroy();
            self.rel_pointer = None;
        }

        // release shortcut inhibitor
        if let Some(shortcut_inhibitor) = &self.shortcut_inhibitor {
            shortcut_inhibitor.destroy();
            self.shortcut_inhibitor = None;
        }
    }

    fn add_client(&mut self, client: ClientHandle, pos: Position) {
        let window = Rc::new(Window::new(&self.g, &self.qh, pos));
        self.client_for_window.push((window, client));
    }
}

impl Source for WaylandEventProducer {
    fn register(
        &mut self,
        registry: &mio::Registry,
        token: mio::Token,
        interests: mio::Interest,
    ) -> std::io::Result<()> {
        SourceFd(&self.state.wayland_fd).register(registry, token, interests)
    }

    fn reregister(
        &mut self,
        registry: &mio::Registry,
        token: mio::Token,
        interests: mio::Interest,
    ) -> std::io::Result<()> {
        SourceFd(&self.state.wayland_fd).reregister(registry, token, interests)
    }

    fn deregister(&mut self, registry: &mio::Registry) -> std::io::Result<()> {
        SourceFd(&self.state.wayland_fd).deregister(registry)
    }
}
impl WaylandEventProducer {
    fn read(&mut self) -> bool {
        log::trace!("reading from wayland-socket");
        let res = match self.state.read_guard.take().unwrap().read() {
            Ok(_) => true,
            Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => false,
            Err(WaylandError::Io(e)) => {
                log::error!("error reading from wayland socket: {e}");
                false
            }
            Err(WaylandError::Protocol(e)) => {
                panic!("wayland protocol violation: {e}")
            }
        };
        log::trace!("preparing next read");
        self.prepare_read();
        log::trace!("done");
        res
    }

    fn prepare_read(&mut self) {
        match self.queue.prepare_read() {
            Ok(r) => self.state.read_guard = Some(r),
            Err(WaylandError::Io(e)) => {
                log::error!("error preparing read from wayland socket: {e}")
            }
            Err(WaylandError::Protocol(e)) => {
                panic!("wayland Protocol violation: {e}")
            }
        };
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

    fn flush_events(&mut self) {
        // flush outgoing events
        match self.queue.flush() {
            Ok(_) => (),
            Err(e) => match e {
                WaylandError::Io(e) => {
                    log::error!("error writing to wayland socket: {e}")
                },
                WaylandError::Protocol(e) => {
                    panic!("wayland protocol violation: {e}")
                },
            },
        }
    }
}

impl EventProducer for WaylandEventProducer {

    fn read_events(&mut self) -> Drain<(ClientHandle, Event)> {
        // read events
        while self.read() {}

        // prepare reading wayland events
        self.dispatch_events();

        self.flush_events();

        // return the events
        self.state.pending_events.drain(..)
    }

    fn notify(&mut self, client_event: ClientEvent) {
        if let ClientEvent::Create(handle, pos) = client_event {
            self.state.add_client(handle, pos);
            self.queue.flush().unwrap();
            self.queue.dispatch_pending(&mut self.state).unwrap();
        }
    }

    fn release(&mut self) {
        self.state.ungrab();
        self.queue.flush().unwrap();
        self.queue.dispatch_pending(&mut self.state).unwrap();
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
                    let (window, client) = app
                        .client_for_window
                        .iter()
                        .find(|(w, _c)| w.surface == surface)
                        .unwrap();
                    app.focused = Some((window.clone(), *client));
                    app.grab(&surface, pointer, serial.clone(), qh);
                }
                let (_, client) = app
                    .client_for_window
                    .iter()
                    .find(|(w, _c)| w.surface == surface)
                    .unwrap();
                app.pending_events.push((*client, Event::Release()));
            }
            wl_pointer::Event::Leave { .. } => {
                app.ungrab();
            }
            wl_pointer::Event::Button {
                serial: _,
                time,
                button,
                state,
            } => {
                let (_, client) = app.focused.as_ref().unwrap();
                app.pending_events.push((
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
                app.pending_events.push((
                    *client,
                    Event::Pointer(PointerEvent::Axis {
                        time,
                        axis: u32::from(axis) as u8,
                        value,
                    }),
                ));
            }
            wl_pointer::Event::Frame {} => {
                let (_, client) = app.focused.as_ref().unwrap();
                app.pending_events.push((
                    *client,
                    Event::Pointer(PointerEvent::Frame {}),
                ));
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
                    app.pending_events.push((
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
                    app.pending_events.push((
                        *client,
                        Event::Keyboard(KeyboardEvent::Modifiers {
                            mods_depressed,
                            mods_latched,
                            mods_locked,
                            group,
                        }),
                    ));
                }
                if mods_depressed == 77 {
                    // ctrl shift super alt
                    app.ungrab();
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
                app.pending_events.push((
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
            let (window, _client) = app
                .client_for_window
                .iter()
                .find(|(w, _c)| &w.layer_surface == layer_surface)
                .unwrap();
            // client corresponding to the layer_surface
            let surface = &window.surface;
            let buffer = &window.buffer;
            surface.attach(Some(&buffer), 0, 0);
            layer_surface.ack_configure(serial);
            surface.commit();
        }
    }
}

// delegate wl_registry events to App itself
// delegate_dispatch!(App: [wl_registry::WlRegistry: GlobalListContents] => App);
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

// don't emit any events
delegate_noop!(State: wl_region::WlRegion);
delegate_noop!(State: wl_shm_pool::WlShmPool);
delegate_noop!(State: wl_compositor::WlCompositor);
delegate_noop!(State: ZwlrLayerShellV1);
delegate_noop!(State: ZwpRelativePointerManagerV1);
delegate_noop!(State: ZwpKeyboardShortcutsInhibitManagerV1);
delegate_noop!(State: ZwpPointerConstraintsV1);

// ignore events
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore ZwpKeyboardShortcutsInhibitorV1);
delegate_noop!(State: ignore ZwpLockedPointerV1);
