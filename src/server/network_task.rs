use std::{io, net::SocketAddr};

use thiserror::Error;
use tokio::{
    net::UdpSocket,
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};

use super::Server;
use lan_mouse_proto::{ProtoEvent, ProtocolError};

pub(crate) async fn new(
    server: Server,
    udp_recv_tx: Sender<Result<(ProtoEvent, SocketAddr), NetworkError>>,
    udp_send_rx: Receiver<(ProtoEvent, SocketAddr)>,
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
                _ = server.notifies.port_changed.notified() => update_port(&server, &mut socket).await,
                _ = server.cancelled() => break, /* cancellation requested */
            }
        }
    }))
}

async fn update_port(server: &Server, socket: &mut UdpSocket) {
    let new_port = server.port.get();
    let current_port = socket.local_addr().expect("socket not bound").port();

    // if port is the same, we dont need to change it
    if current_port == new_port {
        return;
    }

    // bind new socket
    let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), new_port);
    let new_socket = UdpSocket::bind(listen_addr).await;
    let err = match new_socket {
        Ok(new_socket) => {
            *socket = new_socket;
            None
        }
        Err(e) => Some(e.to_string()),
    };

    // notify frontend of the actual port
    let port = socket.local_addr().expect("socket not bound").port();
    server.notify_port_changed(port, err);
}

async fn udp_receiver(
    socket: &UdpSocket,
    receiver_tx: &Sender<Result<(ProtoEvent, SocketAddr), NetworkError>>,
) {
    loop {
        let event = receive_event(socket).await;
        receiver_tx.send(event).await.expect("channel closed");
    }
}

async fn udp_sender(socket: &UdpSocket, rx: &mut Receiver<(ProtoEvent, SocketAddr)>) {
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

async fn receive_event(socket: &UdpSocket) -> Result<(ProtoEvent, SocketAddr), NetworkError> {
    let mut buf = [0u8; lan_mouse_proto::MAX_EVENT_SIZE];
    let (_len, src) = socket.recv_from(&mut buf).await?;
    let event = ProtoEvent::try_from(buf)?;
    Ok((event, src))
}

fn send_event(sock: &UdpSocket, e: ProtoEvent, addr: SocketAddr) -> Result<usize, NetworkError> {
    log::trace!("{:20} ------>->->-> {addr}", e.to_string());
    let (data, len): ([u8; lan_mouse_proto::MAX_EVENT_SIZE], usize) = e.into();
    // When udp blocks, we dont want to block the event loop.
    // Dropping events is better than potentially crashing the input capture.
    Ok(sock.try_send_to(&data[..len], addr)?)
}
