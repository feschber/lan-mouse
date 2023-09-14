use std::os::fd::RawFd;
use anyhow::Result;
use epoll::{self, ControlOptions::EPOLL_CTL_ADD, Events, Event};

pub struct Epoll {
    epfd: RawFd,
}

impl Epoll {
    pub fn new(fds: &[RawFd]) -> Result<Self> {
        let _fds = fds;
        let epfd = epoll::create(true)?;

        for fd in fds {
            let event = epoll::Event::new(Events::EPOLLIN, *fd as u64);
            epoll::ctl(epfd, EPOLL_CTL_ADD, *fd, event)?;
        }
        Ok(Self {epfd})
    }

    pub(crate) fn wait(&self) -> RawFd {
        let mut buf = [Event::new(Events::EPOLLIN, 0); 1];
        let count = epoll::wait(self.epfd, -1, &mut buf[..]).unwrap();
        assert_eq!(count, 1);
        let event = buf[0];
        event.data as RawFd
    }
}
