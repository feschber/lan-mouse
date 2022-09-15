use std::io::{self, Write};
use std::{f64::consts::PI, net::UdpSocket};

use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1 as VpManager,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1 as Vp,
};

use wayland_client::{protocol::wl_registry, Connection, Dispatch, EventQueue, QueueHandle};

// App State, implements Dispatch event handlers
struct AppData {
    vpm: Option<VpManager>,
}

// Implement `Dispatch<WlRegistry, ()> event handler
// user-data = ()
impl Dispatch<wl_registry::WlRegistry, ()> for AppData {
    fn event(
        app: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<AppData>,
    ) {
        // Match global event to get globals after requesting them in main
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            // println!("[{}] {} (v{})", name, interface, version);
            match &interface[..] {
                "zwlr_virtual_pointer_manager_v1" => {
                    // virtual pointer protocol
                    let vpm = registry.bind::<VpManager, _, _>(name, 1, qh, ()); // get the vp manager
                    app.vpm = Some(vpm); // save it to app state
                }
                _ => {}
            }
        }
    }
}

// The main function of our program
fn main() {
    // establish connection via environment-provided configuration.
    let conn = Connection::connect_to_env().unwrap();

    // Retrieve the wayland display object
    let display = conn.display();

    // Create an event queue for our event processing
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();

    // Create a wl_registry object by sending the wl_display.get_registry request
    let _registry = display.get_registry(&qh, ());

    let mut app_data = AppData { vpm: None };

    // use roundtrip to process this event synchronously
    event_queue.roundtrip(&mut app_data).unwrap();
    if let Some(vpm) = app_data.vpm {
        let pointer: Vp = vpm.create_virtual_pointer(None, &qh, ());
        udp_loop(pointer, event_queue).unwrap();
        println!();
    } else {
        panic!("zwlr_virtual_pointer_manager_v1 protocol required")
    };
}

/// main loop handling udp packets
fn udp_loop(pointer: Vp, q: EventQueue<AppData>) -> std::io::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:42069")?;
    // we don't care about possible dropped packets for now

    let mut buf = [0u8; 20];
    loop {
        let (amt, _src) = socket.recv_from(&mut buf)?;
        assert!(amt == 20);

        let time: u32 = u32::from_ne_bytes(buf[0..4].try_into().unwrap());
        let x: f64 = f64::from_ne_bytes(buf[4..12].try_into().unwrap());
        let y: f64 = f64::from_ne_bytes(buf[12..20].try_into().unwrap());

        pointer.motion(time, x, y);
        q.flush().unwrap();
    }
}

impl Dispatch<VpManager, ()> for AppData {
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

impl Dispatch<Vp, ()> for AppData {
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
