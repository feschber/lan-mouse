use crate::{request, client::{ClientHandle, Position, Client}};

use memmap::Mmap;

use std::{
    fs::File,
    io::{BufWriter, Write},
    os::unix::prelude::{AsRawFd, FromRawFd}, sync::mpsc::SyncSender, rc::Rc, thread, time::Duration,
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
    delegate_noop, delegate_dispatch,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_region, wl_registry, wl_seat, wl_shm,
        wl_shm_pool, wl_surface,
    },
    Connection, Dispatch, QueueHandle, WEnum, DispatchError, backend::WaylandError,
};

use tempfile;

use super::Event;

struct Globals {
    compositor: wl_compositor::WlCompositor,
    pointer_constraints: ZwpPointerConstraintsV1,
    relative_pointer_manager: ZwpRelativePointerManagerV1,
    shortcut_inhibit_manager: ZwpKeyboardShortcutsInhibitManagerV1,
    seat: wl_seat::WlSeat,
    shm: wl_shm::WlShm,
    layer_shell: ZwlrLayerShellV1,
}

struct App {
    running: bool,
    pointer_lock: Option<ZwpLockedPointerV1>,
    rel_pointer: Option<ZwpRelativePointerV1>,
    shortcut_inhibitor: Option<ZwpKeyboardShortcutsInhibitorV1>,
    client_for_window: Vec<(Rc<Window>, ClientHandle)>,
    focused: Option<(Rc<Window>, ClientHandle)>,
    g: Globals,
    tx: SyncSender<(Event, ClientHandle)>,
    server: request::Server,
    qh: QueueHandle<Self>,
}

struct Window {
    buffer: wl_buffer::WlBuffer,
    surface: wl_surface::WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
}

impl Window {
    fn new(g: &Globals, qh: &QueueHandle<App>, pos: Position) -> Window {
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

pub fn run(
    tx: SyncSender<(Event, ClientHandle)>,
    server: request::Server,
    clients: Vec<Client>,
) {
    let conn = Connection::connect_to_env().expect("could not connect to wayland compositor");
    let (g, mut queue) = registry_queue_init::<App>(&conn).expect("failed to initialize wl_registry");
    let qh = queue.handle();

    let compositor: wl_compositor::WlCompositor = g
        .bind(&qh, 4..=5, ())
        .expect("wl_compositor >= v4 not supported");
    let shm: wl_shm::WlShm = g
        .bind(&qh, 1..=1, ())
        .expect("wl_shm v1 not supported");
    let layer_shell: ZwlrLayerShellV1 = g
        .bind(&qh, 3..=4, ())
        .expect("zwlr_layer_shell_v1 >= v3 not supported - required to display a surface at the edge of the screen");
    let seat: wl_seat::WlSeat = g
        .bind(&qh, 7..=8, ())
        .expect("wl_seat >= v7 not supported");
    let pointer_constraints: ZwpPointerConstraintsV1 = g
        .bind(&qh, 1..=1, ())
        .expect("zwp_pointer_constraints_v1 not supported");
    let relative_pointer_manager: ZwpRelativePointerManagerV1 = g
        .bind(&qh, 1..=1, ())
        .expect("zwp_relative_pointer_manager_v1 not supported");
    let shortcut_inhibit_manager: ZwpKeyboardShortcutsInhibitManagerV1 = g
        .bind(&qh, 1..=1, ())
        .expect("zwp_keyboard_shortcuts_inhibit_manager_v1 not supported");

    let g = Globals {
        compositor,
        shm,
        layer_shell,
        seat,
        pointer_constraints,
        relative_pointer_manager,
        shortcut_inhibit_manager,
    };

    let client_for_window = Vec::new();

    let mut app = App {
        running: true,
        g,
        pointer_lock: None,
        rel_pointer: None,
        shortcut_inhibitor: None,
        client_for_window,
        focused: None,
        tx,
        server,
        qh,
    };

    for client in clients {
        app.add_client(client.handle, client.pos);
    }

    while app.running {
        match queue.blocking_dispatch(&mut app) {
            Ok(_) => { },
            Err(DispatchError::Backend(WaylandError::Io(e))) => {
                eprintln!("Wayland Error: {}", e);
                thread::sleep(Duration::from_millis(500));
            },
            Err(DispatchError::Backend(e)) => {
                panic!("{}", e);
            }
            Err(DispatchError::BadMessage{ sender_id, interface, opcode }) => {
                panic!("bad message {}, {} , {}", sender_id, interface, opcode);
            }
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

impl App {

    fn grab(
        &mut self,
        surface: &wl_surface::WlSurface,
        pointer: &wl_pointer::WlPointer,
        serial: u32,
        qh: &QueueHandle<App>
    ) {
        let (window, _) = self.focused.as_ref().unwrap();

        // hide the cursor
        pointer.set_cursor(serial, None, 0, 0);

        // capture input
        window.layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        window.surface.commit();

        // lock pointer
        if self.pointer_lock.is_none() {
            self.pointer_lock = Some(self.g.pointer_constraints.lock_pointer(
                surface,
                pointer,
                None,
                Lifetime::Oneshot,
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
        let (window, _client) = self.focused.as_ref().unwrap();

        // ungrab surface
        window.layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
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

impl Dispatch<wl_seat::WlSeat, ()> for App {
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

impl Dispatch<wl_pointer::WlPointer, ()> for App {
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
                    let (window, client) = app.client_for_window
                        .iter()
                        .find(|(w,_c)| w.surface == surface)
                        .unwrap();
                    app.focused = Some((window.clone(), *client));
                    app.grab(&surface, pointer, serial.clone(), qh);
                }
                let (_, client) = app.client_for_window
                    .iter()
                    .find(|(w,_c)| w.surface == surface)
                    .unwrap();
                app.tx.send((Event::Release(), *client)).unwrap();
            }
            wl_pointer::Event::Leave { .. } => {
                app.ungrab();
            }
            wl_pointer::Event::Button { .. } => {
                let (_, client) = app.focused.as_ref().unwrap();
                app.tx.send((Event::Pointer(event), *client)).unwrap();
            }
            wl_pointer::Event::Axis { .. } => {
                let (_, client) = app.focused.as_ref().unwrap();
                app.tx.send((Event::Pointer(event), *client)).unwrap();
            }
            wl_pointer::Event::Frame { .. } => {
                let (_, client) = app.focused.as_ref().unwrap();
                app.tx.send((Event::Pointer(event), *client)).unwrap();
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
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
            wl_keyboard::Event::Key { .. } => {
                if let Some(client) = client {
                    app.tx.send((Event::Keyboard(event), *client)).unwrap();
                }
            }
            wl_keyboard::Event::Modifiers { mods_depressed, .. } => {
                if let Some(client) = client {
                    app.tx.send((Event::Keyboard(event), *client)).unwrap();
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
                let mmap = unsafe { Mmap::map(&File::from_raw_fd(fd.as_raw_fd())).unwrap() };
                app.server.offer_data(request::Request::KeyMap, mmap);
            }
            _ => (),
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for App {
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
                app.tx.send((
                    Event::Pointer(wl_pointer::Event::Motion {
                        time,
                        surface_x,
                        surface_y,
                    }),
                    *client,
                )).unwrap();

            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        app: &mut Self,
        layer_surface: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            let (window, _client) = app.client_for_window
                .iter()
                .find(|(w,_c)| &w.layer_surface == layer_surface)
                .unwrap();
            // client corresponding to the layer_surface
            let surface = &window.surface;
            let buffer = &window.buffer;
            surface.commit();
            layer_surface.ack_configure(serial);
            surface.attach(Some(&buffer), 0, 0);
            surface.commit();
        }
    }
}

// delegate wl_registry events to App itself
// delegate_dispatch!(App: [wl_registry::WlRegistry: GlobalListContents] => App);
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for App {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: <wl_registry::WlRegistry as wayland_client::Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {}
}

// don't emit any events
delegate_noop!(App: wl_region::WlRegion);
delegate_noop!(App: wl_shm_pool::WlShmPool);
delegate_noop!(App: wl_compositor::WlCompositor);
delegate_noop!(App: ZwlrLayerShellV1);
delegate_noop!(App: ZwpRelativePointerManagerV1);
delegate_noop!(App: ZwpKeyboardShortcutsInhibitManagerV1);
delegate_noop!(App: ZwpPointerConstraintsV1);

// ignore events
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_buffer::WlBuffer);
delegate_noop!(App: ignore wl_surface::WlSurface);
delegate_noop!(App: ignore ZwpKeyboardShortcutsInhibitorV1);
delegate_noop!(App: ignore ZwpLockedPointerV1);
