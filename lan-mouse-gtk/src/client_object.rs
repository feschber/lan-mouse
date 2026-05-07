mod imp;

use adw::subclass::prelude::*;
use gtk::glib::{self, Object};

use lan_mouse_ipc::{ClientConfig, ClientHandle, ClientState};

glib::wrapper! {
    pub struct ClientObject(ObjectSubclass<imp::ClientObject>);
}

impl ClientObject {
    pub fn new(handle: ClientHandle, client: ClientConfig, state: ClientState) -> Self {
        Object::builder()
            .property("handle", handle)
            .property("hostname", client.hostname)
            .property("port", client.port as u32)
            .property("position", client.pos.to_string())
            .property("active", state.active)
            .property(
                "ips",
                state
                    .ips
                    .iter()
                    .map(|ip| ip.to_string())
                    .collect::<Vec<_>>(),
            )
            .property("resolving", state.resolving)
            .property("peer-commit", peer_commit_to_string(state.peer_commit))
            .build()
    }

    pub fn get_data(&self) -> ClientData {
        self.imp().data.borrow().clone()
    }
}

/// Render the 8-byte ASCII commit hash carried in
/// [`lan_mouse_ipc::ClientState::peer_commit`] as a `String`. `None`
/// in → `None` out (peer hasn't sent a Hello yet, or speaks an older
/// proto).
pub fn peer_commit_to_string(commit: Option<[u8; 8]>) -> Option<String> {
    commit.and_then(|c| std::str::from_utf8(&c).ok().map(str::to_string))
}

#[derive(Default, Clone)]
pub struct ClientData {
    pub handle: ClientHandle,
    pub hostname: Option<String>,
    pub port: u32,
    pub active: bool,
    pub position: String,
    pub resolving: bool,
    pub ips: Vec<String>,
    pub peer_commit: Option<String>,
}
