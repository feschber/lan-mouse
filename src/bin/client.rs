use std::{os::unix::prelude::AsRawFd, io::{Write, BufWriter}};
use lan_mouse::{protocol::{self, DataRequest}, config::Config};

use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1 as VpManager,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1 as Vp,
};

use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1 as VkManager,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1 as Vk,
};

use wayland_client::{
    protocol::{wl_registry, wl_seat, wl_pointer, wl_keyboard},
    Connection, Dispatch, EventQueue, QueueHandle,
};

use tempfile;

// App State, implements Dispatch event handlers
struct App {
    vpm: Option<VpManager>,
    vkm: Option<VkManager>,
    seat: Option<wl_seat::WlSeat>,
}

// Implement `Dispatch<WlRegistry, ()> event handler
// user-data = ()
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
                "wl_keyboard" => {
                    registry.bind::<wl_keyboard::WlKeyboard, _, _>(name, 1, qh, ());
                }
                "wl_seat" => {
                    app.seat = Some(registry.bind::<wl_seat::WlSeat, _, _>(name, 1, qh, ()));
                }
                "zwlr_virtual_pointer_manager_v1" => {
                    app.vpm = Some(registry.bind::<VpManager, _, _>(name, 1, qh, ()));
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    app.vkm = Some(registry.bind::<VkManager, _, _>(name, 1, qh, ()));
                }
                _ => {}
            }
        }
    }
}

// The main function of our program
fn main() {
    let config = Config::new("config.toml").unwrap();
    // establish connection via environment-provided configuration.
    let conn = Connection::connect_to_env().unwrap();

    // Retrieve the wayland display object
    let display = conn.display();

    // Create an event queue for our event processing
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    // Create a wl_registry object by sending the wl_display.get_registry request
    let _registry = display.get_registry(&qh, ());

    let mut app = App {
        vpm: None,
        vkm: None,
        seat: None,
    };

    // use roundtrip to process this event synchronously
    event_queue.roundtrip(&mut app).unwrap();


    let vpm = app.vpm.as_ref().unwrap();
    let vkm = app.vkm.as_ref().unwrap();
    let seat = app.seat.as_ref().unwrap();
    let pointer: Vp = vpm.create_virtual_pointer(None, &qh, ());
    let keyboard: Vk = vkm.create_virtual_keyboard(&seat, &qh, ());
    let connection = protocol::Connection::new(config);
    let data = loop {
        match connection.receive_data(DataRequest::KeyMap) {
            Some(data) => { break data }
            None => {}
        }
    };
    // TODO use shm_open
    let f = tempfile::tempfile().unwrap();
    let mut buf = BufWriter::new(&f);
    buf.write_all(&data[..]).unwrap();
    buf.flush().unwrap();
    keyboard.keymap(1, f.as_raw_fd(), data.len() as u32);
    event_queue.roundtrip(&mut app).unwrap();
    udp_loop(&connection, &pointer, &keyboard, event_queue).unwrap();
}

/// main loop handling udp packets
fn udp_loop(connection: &protocol::Connection, pointer: &Vp, keyboard: &Vk, q: EventQueue<App>) -> std::io::Result<()> {
    loop {
        if let Some(event) = connection.receive_event() {
            match event {
                protocol::Event::Pointer(e) => {
                    match e {
                        wl_pointer::Event::Motion { time, surface_x, surface_y } => {
                            pointer.motion(time, surface_x, surface_y);
                            pointer.frame();
                        }
                        wl_pointer::Event::Button { serial: _, time: t, button: b, state: s } => {
                            pointer.button( t, b, s.into_result().unwrap());
                            pointer.frame();
                        }
                        wl_pointer::Event::Axis { time: t, axis: a, value: v } => {
                            pointer.axis(t, a.into_result().unwrap(), v);
                            pointer.frame();
                        }
                        wl_pointer::Event::Frame {} => {}
                        _ => todo!(),
                    }
                }
                protocol::Event::Keyboard(e) => {
                    match e {
                        wl_keyboard::Event::Key { serial: _, time: t, key: k, state: s } => {
                            keyboard.key(t, k, u32::from(s));
                        },
                        wl_keyboard::Event::Modifiers { serial: _, mods_depressed, mods_latched, mods_locked, group } => {
                            keyboard.modifiers(mods_depressed, mods_latched, mods_locked, group);
                        },
                        _ => todo!(),
                    }
                }
            }
        }
        q.flush().unwrap();
    }
}

impl Dispatch<VpManager, ()> for App {
    fn event(
        _: &mut Self,
        _: &VpManager,
        _: <VpManager as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // nothing to do here since no events are defined for VpManager
    }
}

impl Dispatch<Vp, ()> for App {
    fn event(
        _: &mut Self,
        _: &Vp,
        _: <Vp as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // no events defined for vp either
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        _: <wl_keyboard::WlKeyboard as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        //
    }
}

impl Dispatch<VkManager, ()> for App {
    fn event(
        _: &mut Self,
        _: &VkManager,
        _: <VkManager as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        //
    }
}

impl Dispatch<Vk, ()> for App {
    fn event(
        _: &mut Self,
        _: &Vk,
        _: <Vk as wayland_client::Proxy>::Event,
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
        _: &wl_seat::WlSeat,
        _: <wl_seat::WlSeat as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        //
    }
}
