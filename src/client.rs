use std::net::SocketAddr;

#[derive(Eq, Hash, PartialEq, Clone, Copy)]
pub enum Position {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy)]
pub struct Client {
    pub addr: SocketAddr,
    pub pos: Position,
    pub handle: ClientHandle,
}

impl Client {
    pub fn handle(&self) -> ClientHandle {
        return self.handle;
    }
}

pub enum ClientEvent {
    Create(Client),
    Destroy(Client),
}

pub struct ClientManager {
    next_id: u32,
    clients: Vec<Client>,
}

pub type ClientHandle = u32;

impl ClientManager {
    fn new_id(&mut self) -> ClientHandle {
        self.next_id += 1;
        self.next_id
    }

    pub fn new() -> Self {
        ClientManager {
            next_id: 0,
            clients: Vec::new(),
        }
    }

    pub fn add_client(&mut self, addr: SocketAddr, pos: Position) {
        let handle = self.new_id();
        let client = Client { addr, pos, handle };
        self.clients.push(client);
    }

    pub fn get_clients(&self) -> Vec<Client> {
        self.clients.clone()
    }
}
