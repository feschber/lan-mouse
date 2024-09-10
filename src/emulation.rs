use crate::{listen::LanMouseListener, server::Server};
use futures::StreamExt;
use input_emulation::{EmulationHandle, InputEmulation, InputEmulationError};
use input_event::Event;
use lan_mouse_ipc::Status;
use lan_mouse_proto::ProtoEvent;
use local_channel::mpsc::{channel, Receiver, Sender};
use std::{
    collections::HashMap,
    net::SocketAddr,
    time::{Duration, Instant},
};
use tokio::{
    select,
    task::{spawn_local, JoinHandle},
};

/// emulation handling events received from a listener
pub(crate) struct Emulation {
    task: JoinHandle<()>,
}

impl Emulation {
    pub(crate) fn new(server: Server, listener: LanMouseListener) -> Self {
        let emulation_proxy = EmulationProxy::new(server.clone());
        let task = spawn_local(Self::run(server, listener, emulation_proxy));
        Self { task }
    }

    async fn run(
        server: Server,
        mut listener: LanMouseListener,
        mut emulation_proxy: EmulationProxy,
    ) {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        let mut last_response = HashMap::new();
        loop {
            select! {
                e = listener.next() =>  {
                    let (event, addr) = match e {
                        Some(e) => e,
                        None => break,
                    };
                    last_response.insert(addr, Instant::now());
                    match event {
                        ProtoEvent::Enter(_) => {
                            server.release_capture();
                            listener.reply(addr, ProtoEvent::Ack(0)).await;
                        }
                        ProtoEvent::Leave(_) => {
                            emulation_proxy.release_keys(addr);
                            listener.reply(addr, ProtoEvent::Ack(0)).await;
                        }
                        ProtoEvent::Ack(_) => {}
                        ProtoEvent::Input(event) => emulation_proxy.consume(event, addr),
                        ProtoEvent::Ping => listener.reply(addr, ProtoEvent::Pong).await,
                        ProtoEvent::Pong => {},
                    }
                }
                _ = interval.tick() => {
                    for (addr, last_response) in last_response.iter() {
                        if last_response.elapsed() > Duration::from_secs(5) {
                            log::warn!("{addr} is not responding, releasing keys!");
                            emulation_proxy.release_keys(*addr);
                        }
                    }
                }
                _ = server.cancelled() => break,
            }
        }
        listener.terminate().await;
        emulation_proxy.terminate().await;
    }

    /// wait for termination
    pub(crate) async fn terminate(&mut self) {
        log::debug!("terminating emulation");
        if let Err(e) = (&mut self.task).await {
            log::warn!("{e}");
        }
    }
}

/// proxy handling the actual input emulation,
/// discarding events when it is disabled
pub(crate) struct EmulationProxy {
    server: Server,
    tx: Sender<(ProxyEvent, SocketAddr)>,
    task: JoinHandle<()>,
}

enum ProxyEvent {
    Input(Event),
    ReleaseKeys,
}

impl EmulationProxy {
    fn new(server: Server) -> Self {
        let (tx, rx) = channel();
        let task = spawn_local(Self::emulation_task(server.clone(), rx));
        Self { server, tx, task }
    }

    fn consume(&self, event: Event, addr: SocketAddr) {
        // ignore events if emulation is currently disabled
        if let Status::Enabled = self.server.emulation_status.get() {
            self.tx
                .send((ProxyEvent::Input(event), addr))
                .expect("channel closed");
        }
    }

    fn release_keys(&self, addr: SocketAddr) {
        self.tx
            .send((ProxyEvent::ReleaseKeys, addr))
            .expect("channel closed");
    }

    async fn emulation_task(server: Server, mut rx: Receiver<(ProxyEvent, SocketAddr)>) {
        let mut handles = HashMap::new();
        let mut next_id = 0;
        loop {
            if let Err(e) = Self::do_emulation(&server, &mut handles, &mut next_id, &mut rx).await {
                log::warn!("input emulation exited: {e}");
            }
            tokio::select! {
                _ = server.emulation_notified() => {},
                _ = server.cancelled() => return,
            }
        }
    }

    async fn do_emulation(
        server: &Server,
        handles: &mut HashMap<SocketAddr, EmulationHandle>,
        next_id: &mut EmulationHandle,
        rx: &mut Receiver<(ProxyEvent, SocketAddr)>,
    ) -> Result<(), InputEmulationError> {
        let backend = server.config.emulation_backend.map(|b| b.into());
        log::info!("creating input emulation ...");
        let mut emulation = tokio::select! {
            r = InputEmulation::new(backend) => r?,
            _ = server.cancelled() => return Ok(()),
        };
        server.set_emulation_status(Status::Enabled);

        // create active handles
        for &handle in handles.values() {
            emulation.create(handle).await;
        }

        let res = Self::do_emulation_session(server, &mut emulation, handles, next_id, rx).await;
        // FIXME replace with async drop when stabilized
        emulation.terminate().await;

        server.set_emulation_status(Status::Disabled);
        res
    }

    async fn do_emulation_session(
        server: &Server,
        emulation: &mut InputEmulation,
        handles: &mut HashMap<SocketAddr, EmulationHandle>,
        next_id: &mut EmulationHandle,
        rx: &mut Receiver<(ProxyEvent, SocketAddr)>,
    ) -> Result<(), InputEmulationError> {
        loop {
            tokio::select! {
                e = rx.recv() => {
                    let (event, addr) = e.expect("channel closed");
                    let handle = match handles.get(&addr) {
                        Some(&handle) => handle,
                        None => {
                            let handle = *next_id;
                            *next_id += 1;
                            emulation.create(handle).await;
                            handles.insert(addr, handle);
                            handle
                        }
                    };
                    match event {
                        ProxyEvent::Input(event) => emulation.consume(event, handle).await?,
                        ProxyEvent::ReleaseKeys => emulation.release_keys(handle).await?,
                    }
                }
                _ = server.cancelled() => break Ok(()),
            }
        }
    }

    async fn terminate(&mut self) {
        let _ = (&mut self.task).await;
    }
}
