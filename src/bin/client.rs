use lan_mouse::{
    config::Config,
    protocol::{self, DataRequest},
};
use std::{
    io::{BufWriter, Write},
    os::unix::prelude::AsRawFd,
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
    delegate_noop,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_keyboard, wl_pointer, wl_registry, wl_seat},
    Connection, Dispatch, EventQueue, QueueHandle,
};

use tempfile;

// App State, implements Dispatch event handlers
struct App;

fn main() {
    let config = Config::new("config.toml").unwrap();
    let conn = Connection::connect_to_env().unwrap();
    let (globals, queue) = registry_queue_init::<App>(&conn).unwrap();
    let qh = queue.handle();

    let vpm: VpManager = globals.bind(&qh, 1..=1, ()).unwrap();
    let vkm: VkManager = globals.bind(&qh, 1..=1, ()).unwrap();
    let seat: wl_seat::WlSeat = globals.bind(&qh, 7..=8, ()).unwrap();

    let pointer: Vp = vpm.create_virtual_pointer(None, &qh, ());
    let keyboard: Vk = vkm.create_virtual_keyboard(&seat, &qh, ());
    let connection = protocol::Connection::new(config);
    let data = loop {
        match connection.receive_data(DataRequest::KeyMap) {
            Some(data) => break data,
            None => {}
        }
    };
    // TODO use shm_open
    let f = tempfile::tempfile().unwrap();
    let mut buf = BufWriter::new(&f);
    buf.write_all(&data[..]).unwrap();
    buf.flush().unwrap();
    keyboard.keymap(1, f.as_raw_fd(), data.len() as u32);
    loop {
        receive_event(&connection, &pointer, &keyboard, &queue).unwrap();
    }
}

/// main loop handling udp packets
fn receive_event(
    connection: &protocol::Connection,
    pointer: &Vp,
    keyboard: &Vk,
    q: &EventQueue<App>,
) -> std::io::Result<()> {
    let event = if let Some(event) = connection.receive_event() {
        event
    } else {
        return Ok(());
    };
    match event {
        protocol::Event::Pointer(e) => match e {
            wl_pointer::Event::Motion {
                time,
                surface_x,
                surface_y,
            } => {
                pointer.motion(time, surface_x, surface_y);
                pointer.frame();
            }
            wl_pointer::Event::Button {
                serial: _,
                time: t,
                button: b,
                state: s,
            } => {
                pointer.button(t, b, s.into_result().unwrap());
                pointer.frame();
            }
            wl_pointer::Event::Axis {
                time: t,
                axis: a,
                value: v,
            } => {
                pointer.axis(t, a.into_result().unwrap(), v);
                pointer.frame();
            }
            wl_pointer::Event::Frame => {
                pointer.frame();
            }
            _ => todo!(),
        },
        protocol::Event::Keyboard(e) => match e {
            wl_keyboard::Event::Key {
                serial: _,
                time: t,
                key: k,
                state: s,
            } => {
                keyboard.key(t, k, u32::from(s));
            }
            wl_keyboard::Event::Modifiers {
                serial: _,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                keyboard.modifiers(mods_depressed, mods_latched, mods_locked, group);
            }
            _ => todo!(),
        },
    }
    q.flush().unwrap();
    Ok(())
}

delegate_noop!(App: Vp);
delegate_noop!(App: Vk);
delegate_noop!(App: VpManager);
delegate_noop!(App: VkManager);
delegate_noop!(App: wl_seat::WlSeat);

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for App {
    fn event(
        _: &mut App,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}
