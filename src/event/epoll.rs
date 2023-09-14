use std::os::fd::RawFd;

pub struct Epoll;

impl Epoll {
    pub fn new(fds: &[RawFd]) -> Self {
        Self {}
    }

    pub(crate) fn wait(&self) -> RawFd {
        todo!()
    }
}
