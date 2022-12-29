use lan_mouse::protocol;
use memmap::Mmap;

use std::{
    fs::File,
    io::{BufWriter, Write},
    os::unix::prelude::{AsRawFd, FromRawFd},
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
    Connection, Dispatch, QueueHandle, WEnum,
};

use tempfile;

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
    windows: Windows,
    pointer_lock: Option<ZwpLockedPointerV1>,
    rel_pointer: Option<ZwpRelativePointerV1>,
    shortcut_inhibitor: Option<ZwpKeyboardShortcutsInhibitorV1>,
    connection: protocol::Connection,
    g: Globals,
}

struct Windows {
    _left: Option<Window>,
    right: Option<Window>,
    _top: Option<Window>,
    _bottom: Option<Window>,
}

struct Window {
    buffer: wl_buffer::WlBuffer,
    surface: wl_surface::WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
}

impl Window {
    fn new(g: &Globals, qh: QueueHandle<App>) -> Window {
        let (width, height) = (1, 1440);
        let mut file = tempfile::tempfile().unwrap();
        draw(&mut file, (width, height));
        let pool = g
            .shm
            .create_pool(file.as_raw_fd(), (width * height * 4) as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            (width * 4) as i32,
            wl_shm::Format::Argb8888,
            &qh,
            (),
        );
        let surface = g.compositor.create_surface(&qh, ());

        let layer_surface = g.layer_shell.get_layer_surface(
            &surface,
            None,
            Layer::Top,
            "LAN Mouse Sharing".into(),
            &qh,
            (),
        );

        layer_surface.set_anchor(Anchor::Right);
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

fn main() {
    let config = lan_mouse::config::Config::new("config.toml").unwrap();
    let connection = protocol::Connection::new(config);
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
        .expect("zwlr_layer_shell_v1 >= v3 not supported");
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

    let windows: Windows = Windows {
        _left: None,
        right: Some(Window::new(&g, qh)),
        _top: None,
        _bottom: None,
    };

    let mut app = App {
        running: true,
        g,
        windows,
        pointer_lock: None,
        rel_pointer: None,
        shortcut_inhibitor: None,
        connection,
    };

    while app.running {
        queue.blocking_dispatch(&mut app).unwrap();
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
    fn grab(&mut self, pointer: &wl_pointer::WlPointer, serial: u32, qh: &QueueHandle<App>) {
        pointer.set_cursor(serial, None, 0, 0);
        let layer_surface = &self.windows.right.as_ref().unwrap().layer_surface;
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        let surface = &self.windows.right.as_ref().unwrap().surface;
        surface.commit();
        if self.pointer_lock.is_none() {
            self.pointer_lock = Some(self.g.pointer_constraints.lock_pointer(
                &surface,
                pointer,
                None,
                Lifetime::Oneshot,
                qh,
                (),
            ));
        }
        if self.rel_pointer.is_none() {
            self.rel_pointer = Some(self.g.relative_pointer_manager.get_relative_pointer(
                pointer,
                qh,
                (),
            ));
        }
        if self.shortcut_inhibitor.is_none() {
            self.shortcut_inhibitor = Some(self.g.shortcut_inhibit_manager.inhibit_shortcuts(
                &surface,
                &self.g.seat,
                qh,
                (),
            ));
        }
    }

    fn ungrab(&mut self) {
        let layer_surface = &self.windows.right.as_ref().unwrap().layer_surface;
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
        let surface = &self.windows.right.as_ref().unwrap().surface;
        surface.commit();
        if let Some(pointer_lock) = &self.pointer_lock {
            pointer_lock.destroy();
            self.pointer_lock = None;
        }
        if let Some(rel_pointer) = &self.rel_pointer {
            rel_pointer.destroy();
            self.rel_pointer = None;
        }
        if let Some(shortcut_inhibitor) = &self.shortcut_inhibitor {
            shortcut_inhibitor.destroy();
            self.shortcut_inhibitor = None;
        }
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
                surface: _,
                surface_x: _,
                surface_y: _,
            } => {
                app.grab(pointer, serial, qh);
            }
            wl_pointer::Event::Leave { .. } => {
                app.ungrab();
            }
            wl_pointer::Event::Button { .. } => {
                app.connection.send_event(event);
            }
            wl_pointer::Event::Axis { .. } => {
                app.connection.send_event(event);
            }
            wl_pointer::Event::Frame { .. } => {
                app.connection.send_event(event);
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
        match event {
            wl_keyboard::Event::Key { .. } => {
                app.connection.send_event(event);
            }
            wl_keyboard::Event::Modifiers { mods_depressed, .. } => {
                app.connection.send_event(event);
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
                app.connection
                    .offer_data(protocol::DataRequest::KeyMap, mmap);
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
            let time = (((utime_hi as u64) << 32 | utime_lo as u64) / 1000) as u32;
            app.connection.send_event(wl_pointer::Event::Motion {
                time,
                surface_x,
                surface_y,
            });
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
            let surface = &app.windows.right.as_ref().unwrap().surface;
            surface.commit();
            layer_surface.ack_configure(serial);
            surface.attach(Some(&app.windows.right.as_ref().unwrap().buffer), 0, 0);
            surface.commit();
        }
    }
}

// delegate wl_registry events to App itself
delegate_dispatch!(App: [wl_registry::WlRegistry: GlobalListContents] => App);

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
