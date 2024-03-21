use crate::client::{ClientEvent, ClientHandle};
use crate::consumer::EventConsumer;
use async_trait::async_trait;
use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use wayland_client::backend::WaylandError;
use wayland_client::WEnum;

use anyhow::{anyhow, Result};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{Axis, ButtonState};
use wayland_client::protocol::wl_seat::WlSeat;
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
    protocol::{wl_registry, wl_seat},
    Connection, Dispatch, EventQueue, QueueHandle,
};

use crate::event::{Event, KeyboardEvent, PointerEvent};

struct State {
    keymap: Option<(u32, OwnedFd, u32)>,
    input_for_client: HashMap<ClientHandle, VirtualInput>,
    seat: wl_seat::WlSeat,
    qh: QueueHandle<Self>,
    vpm: VpManager,
    vkm: VkManager,
}

// App State, implements Dispatch event handlers
pub(crate) struct WlrootsConsumer {
    last_flush_failed: bool,
    state: State,
    queue: EventQueue<State>,
}

impl WlrootsConsumer {
    pub fn new() -> Result<Self> {
        let conn = Connection::connect_to_env()?;
        let (globals, queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();

        let seat: wl_seat::WlSeat = match globals.bind(&qh, 7..=8, ()) {
            Ok(wl_seat) => wl_seat,
            Err(_) => return Err(anyhow!("wl_seat >= v7 not supported")),
        };

        let vpm: VpManager = globals.bind(&qh, 1..=1, ())?;
        let vkm: VkManager = globals.bind(&qh, 1..=1, ())?;

        let input_for_client: HashMap<ClientHandle, VirtualInput> = HashMap::new();

        let mut consumer = WlrootsConsumer {
            last_flush_failed: false,
            state: State {
                keymap: None,
                input_for_client,
                seat,
                vpm,
                vkm,
                qh,
            },
            queue,
        };
        while consumer.state.keymap.is_none() {
            consumer
                .queue
                .blocking_dispatch(&mut consumer.state)
                .unwrap();
        }
        // let fd = unsafe { &File::from_raw_fd(consumer.state.keymap.unwrap().1.as_raw_fd()) };
        // let mmap = unsafe { MmapOptions::new().map_copy(fd).unwrap() };
        // log::debug!("{:?}", &mmap[..100]);
        Ok(consumer)
    }
}

impl State {
    fn add_client(&mut self, client: ClientHandle) {
        let pointer: Vp = self.vpm.create_virtual_pointer(None, &self.qh, ());
        let keyboard: Vk = self.vkm.create_virtual_keyboard(&self.seat, &self.qh, ());

        // TODO: use server side keymap
        if let Some((format, fd, size)) = self.keymap.as_ref() {
            keyboard.keymap(*format, fd.as_fd(), *size);
        } else {
            panic!("no keymap");
        }

        let vinput = VirtualInput { pointer, keyboard };

        self.input_for_client.insert(client, vinput);
    }
}

#[async_trait]
impl EventConsumer for WlrootsConsumer {
    async fn consume(&mut self, event: Event, client_handle: ClientHandle) {
        if let Some(virtual_input) = self.state.input_for_client.get(&client_handle) {
            if self.last_flush_failed {
                if let Err(WaylandError::Io(e)) = self.queue.flush() {
                    if e.kind() == io::ErrorKind::WouldBlock {
                        /*
                         * outgoing buffer is full - sending more events
                         * will overwhelm the output buffer and leave the
                         * wayland connection in a broken state
                         */
                        log::warn!(
                            "can't keep up, discarding event: ({client_handle}) - {event:?}"
                        );
                        return;
                    }
                }
            }
            virtual_input.consume_event(event).unwrap();
            match self.queue.flush() {
                Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.last_flush_failed = true;
                    log::warn!("can't keep up, retrying ...");
                }
                Err(WaylandError::Io(e)) => {
                    log::error!("{e}")
                }
                Err(WaylandError::Protocol(e)) => {
                    panic!("wayland protocol violation: {e}")
                }
                Ok(()) => {
                    self.last_flush_failed = false;
                }
            }
        }
    }

    async fn notify(&mut self, client_event: ClientEvent) {
        if let ClientEvent::Create(client, _) = client_event {
            self.state.add_client(client);
            if let Err(e) = self.queue.flush() {
                log::error!("{}", e);
            }
        }
    }

    async fn destroy(&mut self) {}
}

struct VirtualInput {
    pointer: Vp,
    keyboard: Vk,
}

impl VirtualInput {
    fn consume_event(&self, event: Event) -> Result<(), ()> {
        match event {
            Event::Pointer(e) => {
                match e {
                    PointerEvent::Motion {
                        time,
                        relative_x,
                        relative_y,
                    } => self.pointer.motion(time, relative_x, relative_y),
                    PointerEvent::Button {
                        time,
                        button,
                        state,
                    } => {
                        let state: ButtonState = state.try_into()?;
                        self.pointer.button(time, button, state);
                    }
                    PointerEvent::Axis { time, axis, value } => {
                        let axis: Axis = (axis as u32).try_into()?;
                        self.pointer.axis(time, axis, value);
                        self.pointer.frame();
                    }
                    PointerEvent::Frame {} => self.pointer.frame(),
                }
                self.pointer.frame();
            }
            Event::Keyboard(e) => match e {
                KeyboardEvent::Key { time, key, state } => {
                    self.keyboard.key(time, key, state as u32);
                }
                KeyboardEvent::Modifiers {
                    mods_depressed,
                    mods_latched,
                    mods_locked,
                    group,
                } => {
                    self.keyboard
                        .modifiers(mods_depressed, mods_latched, mods_locked, group);
                }
            },
            _ => {}
        }
        Ok(())
    }
}

delegate_noop!(State: Vp);
delegate_noop!(State: Vk);
delegate_noop!(State: VpManager);
delegate_noop!(State: VkManager);

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut State,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: <WlKeyboard as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap { format, fd, size } = event {
            state.keymap = Some((u32::from(format), fd, size));
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: <WlSeat as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qhandle, ());
            }
        }
    }
}
