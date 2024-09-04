use std::net::SocketAddr;

use slab::Slab;

use lan_mouse_ipc::{ClientConfig, ClientHandle, ClientState, Position};

#[derive(Default)]
pub struct ClientManager {
    clients: Slab<(ClientConfig, ClientState)>,
}

impl ClientManager {
    /// add a new client to this manager
    pub fn add_client(&mut self) -> ClientHandle {
        self.clients.insert(Default::default()) as ClientHandle
    }

    /// find a client by its address
    pub fn get_client(&self, addr: SocketAddr) -> Option<ClientHandle> {
        // since there shouldn't be more than a handful of clients at any given
        // time this is likely faster than using a HashMap
        self.clients
            .iter()
            .find_map(|(k, (_, s))| {
                if s.active && s.ips.contains(&addr.ip()) {
                    Some(k)
                } else {
                    None
                }
            })
            .map(|p| p as ClientHandle)
    }

    pub fn find_client(&self, pos: Position) -> Option<ClientHandle> {
        self.clients
            .iter()
            .find_map(|(k, (c, s))| {
                if s.active && c.pos == pos {
                    Some(k)
                } else {
                    None
                }
            })
            .map(|p| p as ClientHandle)
    }

    /// remove a client from the list
    pub fn remove_client(&mut self, client: ClientHandle) -> Option<(ClientConfig, ClientState)> {
        // remove id from occupied ids
        self.clients.try_remove(client as usize)
    }

    // returns an immutable reference to the client state corresponding to `client`
    pub fn get(&self, handle: ClientHandle) -> Option<&(ClientConfig, ClientState)> {
        self.clients.get(handle as usize)
    }

    /// returns a mutable reference to the client state corresponding to `client`
    pub fn get_mut(&mut self, handle: ClientHandle) -> Option<&mut (ClientConfig, ClientState)> {
        self.clients.get_mut(handle as usize)
    }

    pub fn get_client_states(
        &self,
    ) -> impl Iterator<Item = (ClientHandle, &(ClientConfig, ClientState))> {
        self.clients.iter().map(|(k, v)| (k as ClientHandle, v))
    }

    pub fn get_client_states_mut(
        &mut self,
    ) -> impl Iterator<Item = (ClientHandle, &mut (ClientConfig, ClientState))> {
        self.clients.iter_mut().map(|(k, v)| (k as ClientHandle, v))
    }
}
