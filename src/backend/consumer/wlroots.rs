use crate::client::{Client, ClientHandle};
use crate::request::{self, Request};
use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::time::Duration;
use std::{io, thread};
use std::{
    io::{BufWriter, Write},
    os::unix::prelude::AsRawFd,
};

use wayland_client::globals::BindError;
use wayland_client::protocol::wl_pointer::{Axis, ButtonState};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1 as VpManager,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1 as Vp,
};

use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1 as VkManager,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1 as Vk,
};

use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;

use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_registry, wl_seat},
    Connection, Dispatch, EventQueue, QueueHandle,
};

use tempfile;

use crate::event::{Event, KeyboardEvent, PointerEvent};

enum VirtualInputManager {
    Wlroots { vpm: VpManager, vkm: VkManager },
    Kde { fake_input: OrgKdeKwinFakeInput },
}

// App State, implements Dispatch event handlers
struct App {
    input_for_client: HashMap<ClientHandle, VirtualInput>,
    seat: wl_seat::WlSeat,
    event_rx: Receiver<(Event, ClientHandle)>,
    virtual_input_manager: VirtualInputManager,
    queue: EventQueue<Self>,
    qh: QueueHandle<Self>,
}

pub fn run(event_rx: Receiver<(Event, ClientHandle)>, clients: Vec<Client>) {
    let mut app = App::new(event_rx, clients);
    app.run();
}

impl App {
    pub fn new(event_rx: Receiver<(Event, ClientHandle)>, clients: Vec<Client>) -> Self {
        let conn = Connection::connect_to_env().unwrap();
        let (globals, queue) = registry_queue_init::<App>(&conn).unwrap();
        let qh = queue.handle();

        let vpm: Result<VpManager, BindError> = globals.bind(&qh, 1..=1, ());
        let vkm: Result<VkManager, BindError> = globals.bind(&qh, 1..=1, ());
        let fake_input: Result<OrgKdeKwinFakeInput, BindError> = globals.bind(&qh, 4..=4, ());

        let virtual_input_manager = match (vpm, vkm, fake_input) {
            (Ok(vpm), Ok(vkm), _) => VirtualInputManager::Wlroots { vpm, vkm },
            (_, _, Ok(fake_input)) => {
                fake_input.authenticate(
                    "lan-mouse".into(),
                    "Allow remote clients to control this device".into(),
                );
                VirtualInputManager::Kde { fake_input }
            }
            (Err(e1), Err(e2), Err(e3)) => {
                eprintln!("zwlr_virtual_pointer_v1: {e1}");
                eprintln!("zwp_virtual_keyboard_v1: {e2}");
                eprintln!("org_kde_kwin_fake_input: {e3}");
                panic!("neither wlroots nor kde input emulation protocol supported!")
            }
            _ => {
                panic!()
            }
        };

        let input_for_client: HashMap<ClientHandle, VirtualInput> = HashMap::new();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 7..=8, ()).unwrap();
        let mut app = App {
            input_for_client,
            seat,
            event_rx,
            virtual_input_manager,
            queue,
            qh,
        };
        for client in clients {
            app.add_client(client);
        }
        app
    }

    pub fn run(&mut self) {
        loop {
            let (event, client) = self.event_rx.recv().expect("event receiver unavailable");
            if let Some(virtual_input) = self.input_for_client.get(&client) {
                virtual_input.consume_event(event).unwrap();
                if let Err(e) = self.queue.flush() {
                    eprintln!("{}", e);
                }
            }
        }
    }

    fn add_client(&mut self, client: Client) {
        // create virtual input devices
        match &self.virtual_input_manager {
            VirtualInputManager::Wlroots { vpm, vkm } => {
                let pointer: Vp = vpm.create_virtual_pointer(None, &self.qh, ());
                let keyboard: Vk = vkm.create_virtual_keyboard(&self.seat, &self.qh, ());

                // receive keymap from device
                eprint!("\rconnecting to {} ", client.addr);
                let mut attempts = 0;
                let data = loop {
                    let result = request::request_data(client.addr, Request::KeyMap);
                    eprint!("\rconnecting to {} ", client.addr);
                    for _ in 0..attempts {
                        eprint!(".");
                    }
                    match result {
                        Ok(data) => break data,
                        Err(e) => {
                            eprint!(" - {}", e);
                        }
                    }
                    io::stderr().flush().unwrap();
                    thread::sleep(Duration::from_millis(500));
                    attempts += 1;
                };

                eprint!("\rconnecting to {} ", client.addr);
                for _ in 0..attempts {
                    eprint!(".");
                }
                eprintln!(" done!                          ");

                // TODO use shm_open
                let f = tempfile::tempfile().unwrap();
                let mut buf = BufWriter::new(&f);
                buf.write_all(&data[..]).unwrap();
                buf.flush().unwrap();
                keyboard.keymap(1, f.as_raw_fd(), data.len() as u32);

                let vinput = VirtualInput::Wlroots { pointer, keyboard };

                self.input_for_client.insert(client.handle, vinput);
            }
            VirtualInputManager::Kde { fake_input } => {
                let fake_input = fake_input.clone();
                let vinput = VirtualInput::Kde { fake_input };
                self.input_for_client.insert(client.handle, vinput);
            }
        }
    }
}

enum VirtualInput {
    Wlroots { pointer: Vp, keyboard: Vk },
    Kde { fake_input: OrgKdeKwinFakeInput },
}

impl VirtualInput {
    fn consume_event(&self, event: Event) -> Result<(),()> {
        match event {
            Event::Pointer(e) => match e {
                PointerEvent::Motion {
                    time,
                    relative_x,
                    relative_y,
                } => match self {
                    VirtualInput::Wlroots {
                        pointer,
                        keyboard: _,
                    } => {
                        pointer.motion(time, relative_x, relative_y);
                        pointer.frame();
                    }
                    VirtualInput::Kde { fake_input } => {
                        fake_input.pointer_motion(relative_y, relative_y);
                    }
                },
                PointerEvent::Button {
                    time,
                    button,
                    state,
                } => {
                    let state: ButtonState = state.try_into()?;
                    match self {
                        VirtualInput::Wlroots {
                            pointer,
                            keyboard: _,
                        } => {
                            pointer.button(time, button, state);
                            pointer.frame();
                        }
                        VirtualInput::Kde { fake_input } => {
                            fake_input.button(button, state as u32);
                        }
                    }
                }
                PointerEvent::Axis { time, axis, value } => {
                    let axis: Axis = (axis as u32).try_into()?;
                    match self {
                        VirtualInput::Wlroots {
                            pointer,
                            keyboard: _,
                        } => {
                            pointer.axis(time, axis, value);
                            pointer.frame();
                        }
                        VirtualInput::Kde { fake_input } => {
                            fake_input.axis(axis as u32, value);
                        }
                    }
                }
                PointerEvent::Frame {} => match self {
                    VirtualInput::Wlroots {
                        pointer,
                        keyboard: _,
                    } => {
                        pointer.frame();
                    }
                    VirtualInput::Kde { fake_input: _ } => {}
                },
            },
            Event::Keyboard(e) => match e {
                KeyboardEvent::Key { time, key, state } => match self {
                    VirtualInput::Wlroots {
                        pointer: _,
                        keyboard,
                    } => {
                        keyboard.key(time, key, state as u32);
                    }
                    VirtualInput::Kde { fake_input } => {
                        fake_input.keyboard_key(key, state as u32);
                    }
                },
                KeyboardEvent::Modifiers {
                    mods_depressed,
                    mods_latched,
                    mods_locked,
                    group,
                } => match self {
                    VirtualInput::Wlroots {
                        pointer: _,
                        keyboard,
                    } => {
                        keyboard.modifiers(mods_depressed, mods_latched, mods_locked, group);
                    }
                    VirtualInput::Kde { fake_input: _ } => {}
                },
            },
            Event::Release() => match self {
                VirtualInput::Wlroots {
                    pointer: _,
                    keyboard,
                } => {
                    keyboard.modifiers(77, 0, 0, 0);
                    keyboard.modifiers(0, 0, 0, 0);
                }
                VirtualInput::Kde { fake_input: _ } => {}
            },
        }
        Ok(())
    }
}

delegate_noop!(App: Vp);
delegate_noop!(App: Vk);
delegate_noop!(App: VpManager);
delegate_noop!(App: VkManager);
delegate_noop!(App: wl_seat::WlSeat);
delegate_noop!(App: OrgKdeKwinFakeInput);

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
