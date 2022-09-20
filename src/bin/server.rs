use lan_mouse::protocol;
use memmap::Mmap;

use std::{
    fs::File,
    io::{BufWriter, Write},
    os::unix::prelude::{AsRawFd, FromRawFd},
};

use wayland_protocols::wp::{
    pointer_constraints::zv1::client::{zwp_locked_pointer_v1, zwp_pointer_constraints_v1},
    relative_pointer::zv1::client::{zwp_relative_pointer_manager_v1, zwp_relative_pointer_v1},
    keyboard_shortcuts_inhibit::zv1::client::{
        zwp_keyboard_shortcuts_inhibit_manager_v1,
        zwp_keyboard_shortcuts_inhibitor_v1,
    },
};

use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1,
    zwlr_layer_surface_v1,
};

use wayland_client::{
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm,
        wl_shm_pool, wl_surface, wl_region,
    },
    Connection, Dispatch, QueueHandle, WEnum,
};

use tempfile;

struct App {
    running: bool,
    compositor: Option<wl_compositor::WlCompositor>,
    buffer: Option<wl_buffer::WlBuffer>,
    surface: Option<wl_surface::WlSurface>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    pointer_constraints: Option<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1>,
    rel_pointer_manager: Option<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1>,
    pointer_lock: Option<zwp_locked_pointer_v1::ZwpLockedPointerV1>,
    rel_pointer: Option<zwp_relative_pointer_v1::ZwpRelativePointerV1>,
    shortcut_inhibit_manager: Option<zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1>,
    shortcut_inhibitor: Option<zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1>,
    connection: protocol::Connection,
    seat: Option<wl_seat::WlSeat>,
}

fn main() {
    let config = lan_mouse::config::Config::new("config.toml").unwrap();
    let connection = protocol::Connection::new(config);
    // establish connection via environment-provided configuration.
    let conn = Connection::connect_to_env().unwrap();

    // Retrieve the wayland display object
    let display = conn.display();

    // Create an event queue for our event processing
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    // Create a wl_registry object by sending the wl_display.get_registry request
    display.get_registry(&qh, ());

    let mut app = App {
        running: true,
        compositor: None,
        buffer: None,
        surface: None,
        layer_shell: None,
        layer_surface: None,
        pointer_constraints: None,
        rel_pointer_manager: None,
        pointer_lock: None,
        rel_pointer: None,
        connection,
        shortcut_inhibit_manager: None,
        shortcut_inhibitor: None,
        seat: None,
    };

    // use roundtrip to process this event synchronously
    event_queue.roundtrip(&mut app).unwrap();

    let compositor = app.compositor.as_ref().unwrap();
    app.surface = Some(compositor.create_surface(&qh, ()));

    let layer_shell = app.layer_shell.as_ref().unwrap();
    let layer_surface = layer_shell.get_layer_surface(
        &app.surface.as_ref().unwrap(),
        None,
        zwlr_layer_shell_v1::Layer::Top,
        "LAN Mouse Sharing".into(),
        &qh,
        ()
    );
    app.layer_surface = Some(layer_surface);
    let layer_surface = app.layer_surface.as_ref().unwrap();
    layer_surface.set_anchor(zwlr_layer_surface_v1::Anchor::Right);
    layer_surface.set_size(1, 1440);
    layer_surface.set_exclusive_zone(1);
    layer_surface.set_margin(0, 0, 0, 0);
    app.surface.as_ref().unwrap().set_input_region(None);
    app.surface.as_ref().unwrap().commit();
    while app.running {
        event_queue.blocking_dispatch(&mut app).unwrap();
    }
}

fn draw(f: &mut File, (width, height): (u32, u32)) {
    let mut buf = BufWriter::new(f);
    for _ in 0..height {
        for _ in 0..width {
            buf.write_all(&0x88FbF1C7u32.to_ne_bytes()).unwrap();
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        app: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        // Match global event to get globals after requesting them in main
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            match &interface[..] {
                "wl_compositor" => {
                    app.compositor =
                        Some(registry.bind::<wl_compositor::WlCompositor, _, _>(name, 4, qh, ()));
                }
                "wl_shm" => {
                    let shm = registry.bind::<wl_shm::WlShm, _, _>(name, 1, qh, ());
                    let (width, height) = (1, 1440);
                    let mut file = tempfile::tempfile().unwrap();
                    draw(&mut file, (width, height));
                    let pool =
                        shm.create_pool(file.as_raw_fd(), (width * height * 4) as i32, &qh, ());
                    let buffer = pool.create_buffer(
                        0,
                        width as i32,
                        height as i32,
                        (width * 4) as i32,
                        wl_shm::Format::Argb8888,
                        qh,
                        (),
                    );
                    app.buffer = Some(buffer);
                }
                "wl_seat" => {
                    app.seat = Some(registry.bind::<wl_seat::WlSeat, _, _>(name, 8, qh, ()));
                }
                "zwp_pointer_constraints_v1" => {
                    app.pointer_constraints = Some(
                        registry.bind::<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1, _, _>(
                            name,
                            1,
                            &qh,
                            (),
                        ),
                    );
                }
                "zwp_relative_pointer_manager_v1" => {
                    app.rel_pointer_manager = Some(
                        registry.bind::<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1, _, _>(
                            name,
                            1,
                            &qh,
                            (),
                        ),
                    );
                }
                "zwlr_layer_shell_v1" => {
                    app.layer_shell = Some(registry.bind::<zwlr_layer_shell_v1::ZwlrLayerShellV1, _, _>(
                        name,
                        4, &qh, (),
                    ));
                }
                "zwp_keyboard_shortcuts_inhibit_manager_v1" => {
                    app.shortcut_inhibit_manager = Some(registry.bind::<zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1, _, _>(
                        name, 1, &qh, (),
                    ));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_compositor::WlCompositor,
        _: <wl_compositor::WlCompositor as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        todo!()
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: <wl_surface::WlSurface as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        todo!()
    }
}

impl Dispatch<wl_shm::WlShm, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_shm::WlShm,
        _: <wl_shm::WlShm as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // ignore
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: <wl_shm_pool::WlShmPool as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        todo!()
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        _: <wl_buffer::WlBuffer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        //
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
                serial: _,
                surface: _,
                surface_x: _,
                surface_y: _,
            } => {
                if let Some(s) = app.layer_surface.as_ref() {
                    s.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive);
                    app.surface.as_ref().unwrap().commit();
                }
                if app.pointer_lock.is_none() {
                    app.pointer_lock =
                        Some(app.pointer_constraints.as_ref().unwrap().lock_pointer(
                            &app.surface.as_ref().unwrap(),
                            pointer,
                            None,
                            zwp_pointer_constraints_v1::Lifetime::Oneshot,
                            qh,
                            (),
                        ));
                }
                if app.rel_pointer.is_none() {
                    app.rel_pointer = Some(
                        app.rel_pointer_manager
                            .as_ref()
                            .unwrap()
                            .get_relative_pointer(pointer, qh, ()),
                    );
                }
                if app.shortcut_inhibitor.is_none() {
                    app.shortcut_inhibitor = Some(app.shortcut_inhibit_manager.as_ref().unwrap().inhibit_shortcuts(
                            app.surface.as_ref().unwrap(),
                            app.seat.as_ref().unwrap(),
                            qh, (),
                    ));
                }
            }
            wl_pointer::Event::Leave {..} => {
                if let Some(s) = app.layer_surface.as_ref() {
                    s.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
                    app.surface.as_ref().unwrap().commit();
                }
            }
            wl_pointer::Event::Button {..} => {
                app.connection.send_event(event);
            }
            wl_pointer::Event::Axis {..} => {
                app.connection.send_event(event);
            }
            wl_pointer::Event::Frame {..} => {
                app.connection.send_event(event);
            }
            _ => {},
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
            wl_keyboard::Event::Key { serial: _, time: _, key, state: _ } => {
                if key == 1 {
                    // ESC key
                    if let Some(pointer_lock) = app.pointer_lock.as_ref() {
                        pointer_lock.destroy();
                        app.pointer_lock = None;
                    }
                    if let Some(rel_pointer) = app.rel_pointer.as_ref() {
                        rel_pointer.destroy();
                        app.rel_pointer = None;
                    }
                    if let Some(shortcut_inhibitor) = app.shortcut_inhibitor.as_ref() {
                        shortcut_inhibitor.destroy();
                        app.shortcut_inhibitor = None;
                    }
                } else {
                    app.connection.send_event(event);
                }
            }
            wl_keyboard::Event::Modifiers {..} => {
                app.connection.send_event(event);
            }
            wl_keyboard::Event::Keymap { format:_ , fd, size:_ } => {
                let mmap = unsafe { Mmap::map(&File::from_raw_fd(fd.as_raw_fd())).unwrap() };
                app.connection.offer_data(protocol::DataRequest::KeyMap, mmap);
            }
            _ => (),
        }
    }
}

impl Dispatch<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &zwp_pointer_constraints_v1::ZwpPointerConstraintsV1,
        _: <zwp_pointer_constraints_v1::ZwpPointerConstraintsV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_locked_pointer_v1::ZwpLockedPointerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &zwp_locked_pointer_v1::ZwpLockedPointerV1,
        _: <zwp_locked_pointer_v1::ZwpLockedPointerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
        _: <zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        //
    }
}

impl Dispatch<zwp_relative_pointer_v1::ZwpRelativePointerV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &zwp_relative_pointer_v1::ZwpRelativePointerV1,
        event: <zwp_relative_pointer_v1::ZwpRelativePointerV1 as wayland_client::Proxy>::Event,
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
                        } = event {
            let time = (((utime_hi as u64) << 32 | utime_lo as u64) / 1000) as u32;
            app.connection.send_event(wl_pointer::Event::Motion{ time, surface_x, surface_y });
        }
    }
}

impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        _: <zwlr_layer_shell_v1::ZwlrLayerShellV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        //
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        app: &mut Self,
        surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: <zwlr_layer_surface_v1::ZwlrLayerSurfaceV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            app.surface.as_ref().unwrap().commit();
            surface.ack_configure(serial);
            app.surface.as_ref().unwrap().attach(Some(app.buffer.as_ref().unwrap()), 0, 0);
        }
    }
}

impl Dispatch<wl_region::WlRegion, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_region::WlRegion,
        _: <wl_region::WlRegion as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) { }
}

impl Dispatch<zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
        _: <zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) { }
}

impl Dispatch<zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
        _: <zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) { }
}
