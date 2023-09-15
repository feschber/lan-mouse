use std::{ffi::CStr, thread, io::Write, net::SocketAddr};
#[cfg(unix)]
use std::os::fd::{RawFd, AsRawFd};
#[cfg(unix)]
use libc::c_void;

use anyhow::Result;
use ipc_channel::ipc::{IpcReceiver, IpcSender};
use crate::client::Position;

use super::{Frontend, FrontendEvent, FrontendNotify};

pub fn create() -> Result<Box<dyn Frontend>> {
    Ok(Box::new(CliFrontend::new()?))
}

pub struct CliFrontend {
    event_rx: IpcReceiver<FrontendEvent>,
    event_tx: Option<IpcSender<FrontendEvent>>,
    _notify_rx: IpcReceiver<FrontendNotify>,
    notify_tx: IpcSender<FrontendNotify>,
    #[cfg(unix)]
    event_fd: Option<RawFd>,
}

impl CliFrontend {
    pub fn new() -> Result<Self> {
        let (event_tx, event_rx) = ipc_channel::ipc::channel()?;
        let (notify_tx, _notify_rx) = ipc_channel::ipc::channel()?;
        #[cfg(unix)]
        let event_fd = unsafe {
            let fd = libc::eventfd(0, 0);
            if fd < 0 { 
                let errno = *libc::__errno_location();
                let error = libc::strerror(errno);
                let error = CStr::from_ptr(error);
                panic!("{error:?}", );
            }
            Some(fd.as_raw_fd())
        };
        #[cfg(unix)]
        log::debug!("eventfd: {event_fd:?}");

        Ok(Self {
            event_rx,
            event_tx: Some(event_tx),
            _notify_rx,
            notify_tx,
            #[cfg(unix)]
            event_fd,
        })
    }
}

impl Frontend for CliFrontend {
    fn event_channel(&self) -> &IpcReceiver<FrontendEvent> {
        &self.event_rx
    }

    fn notify_channel(&self) -> &IpcSender<FrontendNotify> {
        &self.notify_tx
    }

    #[cfg(unix)]
    fn eventfd(&self) -> Option<RawFd> {
        self.event_fd
    }

    #[cfg(unix)]
    fn read_event(&self) -> u64 {
        let l = 0u64;
        unsafe {
            libc::read(self.event_fd.unwrap() as i32, l as *mut c_void, 8) as u64
        }
    }

    fn start(&mut self) {
        #[cfg(unix)]
        let eventfd = self.event_fd;
        let event_tx = self.event_tx.take().unwrap();
        thread::Builder::new()
            .name("cli-frontend".to_string())
            .spawn(move || {
            loop {
                eprint!("lan-mouse > ");
                std::io::stderr().flush().unwrap();
                let mut buf = String::new();
                match std::io::stdin().read_line(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => {
                        if let Some(event) = parse_event(buf) {
                            if let Err(e) = event_tx.send(event) {
                                log::error!("error sending message: {e}");
                            };
                            #[cfg(unix)]
                            if let Some(eventfd) = eventfd {
                                signal_event(eventfd);
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("{e:?}");
                        break
                    }
                }
            }
        }).unwrap();
    }
}

fn parse_event(s: String) -> Option<FrontendEvent> {
    let mut l = s.split_whitespace();
    let cmd = l.next()?;
    match cmd {
        "connect" => {
            let addr = match l.next()?.parse() {
                Ok(addr) => SocketAddr::V4(addr),
                Err(e) => {
                    log::error!("parse error: {e}");
                    return None;
                }
            };
            Some(FrontendEvent::RequestClientAdd(addr, Position::Left ))
        }
        _ => {
            log::error!("unknown command: {s}");
            None
        }
    }
}

#[cfg(unix)]
fn signal_event(fd: RawFd) {
    unsafe {
        let i = 1u64;
        let err = libc::write(fd as i32, &i as *const u64 as *const c_void, 8);
        if err < 0 { 
            let errno = *libc::__errno_location();
            let error = libc::strerror(errno);
            let error = CStr::from_ptr(error);
            panic!("write: {error:?} ({errno})");
        }
    }
}
