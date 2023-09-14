use std::os::fd::RawFd;

pub struct Epoll;

impl Epoll {
    pub fn new(fds: &[RawFd]) -> Self {
        let _fds = fds;
        Self {}
    }

    pub(crate) fn wait(&self) -> RawFd {
        todo!()
    }
}
