use super::Frontend;

pub fn create() -> Box<dyn Frontend> {
    return Box::new(CliFrontend::new());
}

pub struct CliFrontend {}

impl CliFrontend {
    pub fn new() -> Self {
        Self{}
    }
}

impl Frontend for CliFrontend {
    fn event_channel(&self) -> ipc_channel::ipc::IpcReceiver<super::FrontendEvent> {
        todo!()
    }

    fn notify_channel(&self) -> ipc_channel::ipc::IpcSender<super::FrontendNotify> {
        todo!()
    }
}
