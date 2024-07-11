use std::{io, net::SocketAddr};

use thiserror::Error;
use tokio::{
    net::UdpSocket,
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};

use crate::frontend::FrontendEvent;
use input_event::{Event, ProtocolError};

use super::Server;

pub async fn new(
    server: Server,
    udp_recv_tx: Sender<Result<(Event, SocketAddr), NetworkError>>,
    udp_send_rx: Receiver<(Event, SocketAddr)>,
    mut port_rx: Receiver<u16>,
    frontend_notify_tx: Sender<FrontendEvent>,
) -> io::Result<JoinHandle<()>> {
    // bind the udp socket
    let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), server.port.get());
    let mut socket = UdpSocket::bind(listen_addr).await?;

    Ok(tokio::task::spawn_local(async move {
        let mut sender_rx = udp_send_rx;
        loop {
            let udp_receiver = udp_receiver(&socket, &udp_recv_tx);
            let udp_sender = udp_sender(&socket, &mut sender_rx);
            tokio::select! {
                _ = udp_receiver => break, /* channel closed */
                _ = udp_sender => break, /* channel closed */
                port = port_rx.recv() => match port {
                    Some(port) => update_port(&server, &frontend_notify_tx, &mut socket, port).await,
                    _ => continue,
                },
                _ = server.cancelled() => break, /* cancellation requested */
            }
        }
    }))
}

async fn update_port(
    server: &Server,
    frontend_chan: &Sender<FrontendEvent>,
    socket: &mut UdpSocket,
    port: u16,
) {
    // if port is the same, we dont need to change it
    if socket.local_addr().unwrap().port() == port {
        return;
    }

    // create new socket
    let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), port);
    let frontend_event = match UdpSocket::bind(listen_addr).await {
        Ok(new_socket) => {
            *socket = new_socket;
            server.port.replace(port);
            FrontendEvent::PortChanged(port, None)
        }
        Err(e) => {
            log::warn!("could not change port: {e}");
            let port = socket.local_addr().unwrap().port();
            FrontendEvent::PortChanged(port, Some(format!("could not change port: {e}")))
        }
    };
    let _ = frontend_chan.send(frontend_event).await;
}

async fn udp_receiver(
    socket: &UdpSocket,
    receiver_tx: &Sender<Result<(Event, SocketAddr), NetworkError>>,
) {
    loop {
        let event = receive_event(socket).await;
        receiver_tx.send(event).await.expect("channel closed");
    }
}

async fn udp_sender(socket: &UdpSocket, rx: &mut Receiver<(Event, SocketAddr)>) {
    loop {
        let (event, addr) = rx.recv().await.expect("channel closed");
        if let Err(e) = send_event(socket, event, addr) {
            log::warn!("udp send failed: {e}");
        };
    }
}

#[derive(Debug, Error)]
pub(crate) enum NetworkError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("network error: `{0}`")]
    Io(#[from] io::Error),
}

async fn receive_event(socket: &UdpSocket) -> Result<(Event, SocketAddr), NetworkError> {
    let mut buf = vec![0u8; 22];
    let (_amt, src) = socket.recv_from(&mut buf).await?;
    Ok((Event::try_from(buf)?, src))
}

fn send_event(sock: &UdpSocket, e: Event, addr: SocketAddr) -> Result<usize, NetworkError> {
    log::trace!("{:20} ------>->->-> {addr}", e.to_string());
    let data: Vec<u8> = (&e).into();
    // When udp blocks, we dont want to block the event loop.
    // Dropping events is better than potentially crashing the input capture.
    Ok(sock.try_send_to(&data, addr)?)
}
