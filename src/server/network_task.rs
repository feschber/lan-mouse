use std::net::SocketAddr;

use anyhow::Result;
use tokio::{
    net::UdpSocket,
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};

use crate::{event::Event, frontend::FrontendNotify};

use super::Server;

pub async fn new(
    server: Server,
    frontend_notify_tx: Sender<FrontendNotify>,
) -> Result<(
    JoinHandle<()>,
    Sender<(Event, SocketAddr)>,
    Receiver<Result<(Event, SocketAddr)>>,
    Sender<u16>,
)> {
    // bind the udp socket
    let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), server.port.get());
    let mut socket = UdpSocket::bind(listen_addr).await?;
    let (receiver_tx, receiver_rx) = tokio::sync::mpsc::channel(32);
    let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(32);
    let (port_tx, mut port_rx) = tokio::sync::mpsc::channel(32);

    let udp_task = tokio::task::spawn_local(async move {
        loop {
            tokio::select! {
                event = receive_event(&socket) => {
                    let _ = receiver_tx.send(event).await;
                }
                event = sender_rx.recv() => {
                    let Some((event, addr)) = event else {
                        break;
                    };
                    if let Err(e) = send_event(&socket, event, addr) {
                        log::warn!("udp send failed: {e}");
                    };
                }
                port = port_rx.recv() => {
                    let Some(port) = port else {
                        break;
                    };

                    if socket.local_addr().unwrap().port() == port {
                        continue;
                    }

                    let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), port);
                    match UdpSocket::bind(listen_addr).await {
                        Ok(new_socket) => {
                            socket = new_socket;
                            server.port.replace(port);
                            let _ = frontend_notify_tx.send(FrontendNotify::NotifyPortChange(port, None)).await;
                        }
                        Err(e) => {
                            log::warn!("could not change port: {e}");
                            let port = socket.local_addr().unwrap().port();
                            let _ = frontend_notify_tx.send(FrontendNotify::NotifyPortChange(
                                    port,
                                    Some(format!("could not change port: {e}")),
                                )).await;
                        }
                    }

                }
            }
        }
    });
    Ok((udp_task, sender_tx, receiver_rx, port_tx))
}

async fn receive_event(socket: &UdpSocket) -> Result<(Event, SocketAddr)> {
    let mut buf = vec![0u8; 22];
    let (_amt, src) = socket.recv_from(&mut buf).await?;
    Ok((Event::try_from(buf)?, src))
}

fn send_event(sock: &UdpSocket, e: Event, addr: SocketAddr) -> Result<usize> {
    log::trace!("{:20} ------>->->-> {addr}", e.to_string());
    let data: Vec<u8> = (&e).into();
    // When udp blocks, we dont want to block the event loop.
    // Dropping events is better than potentially crashing the event
    // producer.
    Ok(sock.try_send_to(&data, addr)?)
}
