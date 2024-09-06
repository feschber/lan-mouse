use crate::server::Server;

pub(crate) struct Capture {
    server: Server,
}

impl Capture {
    pub(crate) fn new(server: Server) -> Self {
        Self { server }
    }

    pub(crate) async fn run(&mut self, backend: input_capture::Backend) {
        loop {
            if let Err(e) = do_capture(backend)
        }
    }
}
