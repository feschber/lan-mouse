use memmap::Mmap;
use crate::config::{self, Config};
use std::{io::prelude::*, net::TcpListener, thread, sync::{Arc, RwLock}, collections::HashMap};
use crate::dns;

use wayland_client::protocol::{
    wl_pointer::{Axis, ButtonState},
    wl_keyboard::KeyState,
};

use std::net::{SocketAddr, UdpSocket, TcpStream};

trait Resolve {
    fn resolve(&self) -> Option<SocketAddr>;
}

impl Resolve for Option<config::Client> {
    fn resolve(&self) -> Option<SocketAddr> {
        let client = match self {
            Some(client) => client,
            None => return None,
        };
        let ip = match client.ip {
            Some(ip) => ip,
            None => dns::resolve(&client.host_name).unwrap()
        };
        Some(SocketAddr::new(ip, client.port.unwrap_or(42069)))
    }
}

struct ClientAddrs {
    left: Option<SocketAddr>,
    right: Option<SocketAddr>,
    _top: Option<SocketAddr>,
    _bottom: Option<SocketAddr>,
}

pub struct Connection {
    udp_socket: UdpSocket,
    client: ClientAddrs,
    offer_data: Arc<RwLock<HashMap<DataRequest, Mmap>>>,
}

pub enum Event {
    Mouse{t: u32, x: f64, y: f64},
    Button{t: u32, b: u32, s: ButtonState},
    Axis{t: u32, a: Axis, v: f64},
    Key{t: u32, k: u32, s: KeyState},
    KeyModifier{mods_depressed: u32, mods_latched: u32, mods_locked: u32, group: u32},
}

impl From<Vec<u8>> for Event {
    fn from(buf: Vec<u8>) -> Self {
        match buf[0] {
            0 => Self::Mouse {
                t: u32::from_ne_bytes(buf[1..5].try_into().unwrap()),
                x: f64::from_ne_bytes(buf[5..13].try_into().unwrap()),
                y: f64::from_ne_bytes(buf[13..21].try_into().unwrap()),
            },
            1 => Self::Button {
                t: (u32::from_ne_bytes(buf[1..5].try_into().unwrap())),
                b: (u32::from_ne_bytes(buf[5..9].try_into().unwrap())),
                s: (ButtonState::try_from(buf[9] as u32).unwrap())
            },
            2 => Self::Axis {
                t: (u32::from_ne_bytes(buf[1..5].try_into().unwrap())),
                a: (Axis::try_from(buf[5] as u32).unwrap()),
                v: (f64::from_ne_bytes(buf[6..14].try_into().unwrap())),
            },
            3 => Self::Key {
                t: u32::from_ne_bytes(buf[1..5].try_into().unwrap()),
                k: u32::from_ne_bytes(buf[5..9].try_into().unwrap()),
                s: KeyState::try_from(buf[9] as u32).unwrap(),
            },
            4 => Self::KeyModifier {
                mods_depressed: u32::from_ne_bytes(buf[1..5].try_into().unwrap()),
                mods_latched: u32::from_ne_bytes(buf[5..9].try_into().unwrap()),
                mods_locked: u32::from_ne_bytes(buf[9..13].try_into().unwrap()),
                group: u32::from_ne_bytes(buf[13..17].try_into().unwrap()),
            },
            _ => panic!("protocol violation"),
        }
    }
}

impl From<&Event> for Vec<u8> {
    fn from(e: &Event) -> Self {
        let mut buf = Vec::new();
        match e {
            Event::Mouse { t, x, y } => {
                buf.push(0u8);
                buf.extend_from_slice(t.to_ne_bytes().as_ref());
                buf.extend_from_slice(x.to_ne_bytes().as_ref());
                buf.extend_from_slice(y.to_ne_bytes().as_ref());
            }
            Event::Button { t, b, s } => {
                buf.push(1u8);
                buf.extend_from_slice(t.to_ne_bytes().as_ref());
                buf.extend_from_slice(b.to_ne_bytes().as_ref());
                buf.push(u32::from(*s) as u8);
            }
            Event::Axis{t, a, v} => {
                buf.push(2u8);
                buf.extend_from_slice(t.to_ne_bytes().as_ref());
                buf.push(u32::from(*a) as u8);
                buf.extend_from_slice(v.to_ne_bytes().as_ref());
            }
            Event::Key{t, k, s } => {
                buf.push(3u8);
                buf.extend_from_slice(t.to_ne_bytes().as_ref());
                buf.extend_from_slice(k.to_ne_bytes().as_ref());
                buf.push(u32::from(*s) as u8);
            }
            Event::KeyModifier{ mods_depressed, mods_latched, mods_locked, group } => {
                buf.push(4u8);
                buf.extend_from_slice(mods_depressed.to_ne_bytes().as_ref());
                buf.extend_from_slice(mods_latched.to_ne_bytes().as_ref());
                buf.extend_from_slice(mods_locked.to_ne_bytes().as_ref());
                buf.extend_from_slice(group.to_ne_bytes().as_ref());
            }
        }
        buf
    }
}

#[derive(PartialEq, Eq, Hash)]
pub enum DataRequest {
    KeyMap,
}

impl From<u32> for DataRequest {
    fn from(idx: u32) -> Self {
        match idx {
            0 => Self::KeyMap,
            _ => panic!("invalid enum value"),
        }
    }
}

impl From<DataRequest> for u32 {
    fn from(d: DataRequest) -> Self {
        match d {
            DataRequest::KeyMap => 0,
        }
    }
}

impl From<[u8;4]> for DataRequest {
    fn from(buf: [u8;4]) -> Self {
        DataRequest::from(u32::from_ne_bytes(buf))
    }
}

fn handle_request(data: &Arc<RwLock<HashMap<DataRequest, Mmap>>>, mut stream: TcpStream) {
    let mut buf = [0u8; 4];
    stream.read_exact(&mut buf).unwrap();
    match DataRequest::from(buf) {
        DataRequest::KeyMap => {
            let data = data.read().unwrap();
            let buf = data.get(&DataRequest::KeyMap);
            match buf {
                None => {
                    stream.write(&0u32.to_ne_bytes()).unwrap();
                }
                Some(buf) => {
                    stream.write(&buf[..].len().to_ne_bytes()).unwrap();
                    stream.write(&buf[..]).unwrap();
                }
            }
            stream.flush().unwrap();
        }
    }
}

impl Connection {
    pub fn new(config: Config) -> Connection {
        let clients = ClientAddrs {
            left: config.client.left.resolve(),
            right: config.client.right.resolve(),
            _top: config.client.top.resolve(),
            _bottom: config.client.bottom.resolve(),
        };
        let data: Arc<RwLock<HashMap<DataRequest, Mmap>>> = Arc::new(RwLock::new(HashMap::new()));
        let thread_data = data.clone();
        let port = config.port.unwrap_or(42069);
        let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), port);
        thread::spawn(move || {
            let sock = TcpListener::bind(listen_addr).unwrap();
            for stream in sock.incoming() {
                if let Ok(stream) = stream {
                    handle_request(&thread_data, stream);
                }
            }
        });
        let c = Connection {
            udp_socket: UdpSocket::bind(listen_addr).unwrap(),
            client: clients,
            offer_data: data,
        };
        c
    }

    pub fn offer_data(&self, req: DataRequest, d: Mmap) {
        self.offer_data.write().unwrap().insert(req, d);
    }

    pub fn receive_data(&self, req: DataRequest) -> Option<Vec<u8>> {
        let mut sock = TcpStream::connect(self.client.left.unwrap()).unwrap();
        sock.write(&u32::from(req).to_ne_bytes()).unwrap();
        sock.flush().unwrap();
        let mut buf = [0u8;8];
        sock.read_exact(&mut buf[..]).unwrap();
        let len = usize::from_ne_bytes(buf);
        if len == 0 {
            return None;
        }
        let mut data: Vec<u8> = vec![0u8; len];
        sock.read_exact(&mut data[..]).unwrap();
        Some(data)
    }

    pub fn send_event(&self, e: &Event) {
        // TODO check which client
        if let Some(addr) = self.client.right {
            let buf: Vec<u8> = e.into();
            self.udp_socket.send_to(&buf, addr).unwrap();
        }
    }

    pub fn receive_event(&self) -> Option<Event> {
        let mut buf = vec![0u8; 21];
        if let Ok((_amt, _src)) = self.udp_socket.recv_from(&mut buf) {
            Some(Event::from(buf))
        } else {
            None
        }
    }
}
