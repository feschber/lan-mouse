use std::{net::UdpSocket, f64::consts::PI};
use std::io::{self, Write};

use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1 as Vp,
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1 as VpManager,
};

use wayland_client::{
    protocol::wl_registry,
    Connection, Dispatch, QueueHandle, EventQueue,
};

// App State, implements Dispatch event handlers
struct AppData{
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
        if let wl_registry::Event::Global { name, interface, .. } = event {
            // println!("[{}] {} (v{})", name, interface, version);
            match &interface[..] {
                "zwlr_virtual_pointer_manager_v1" => {  // virtual pointer protocol
                    let vpm = registry.bind::<VpManager, _, _>(name, 1, qh, ());  // get the vp manager
                    app.vpm = Some(vpm);  // save it to app state
                },
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
fn udp_loop(pointer: Vp, q: EventQueue<AppData>) -> std::io::Result<()>{
    let socket = UdpSocket::bind("0.0.0.0:42069")?;
    // we don't care about possible dropped packets for now

    let mut buf = [0; 0];
    let rps = 1.0;
    let rpms = rps * 0.001;
    let radpms = rpms * 2.0 * PI;
    let mut rad = 0_f64;
    let mut time = 0;
    loop {
        let (_amt, _src) = socket.recv_from(&mut buf)?;

        let x = rad.cos();
        let y = rad.sin();

        let scale = 100.0;

        pointer.motion(time, x * scale * radpms, y * scale * radpms);
        q.flush().unwrap();
        rad += radpms;
        rad %= 2.0*PI;
        time+=1;
        print!("{}\r", time);
        io::stdout().flush().unwrap();
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
