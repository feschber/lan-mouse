use anyhow::{anyhow, Result};
use std::collections::VecDeque;
use std::os::fd::{AsRawFd, RawFd};
use std::task::{ready, Poll};
use std::{io, ptr};

use futures_core::Stream;

use crate::event::{Event, PointerEvent};
use crate::producer::EventProducer;

use crate::client::{ClientEvent, ClientHandle};
use tokio::io::unix::AsyncFd;

use x11::xlib::{
    self, CWBackPixel, CWEventMask, CWOverrideRedirect, CopyFromParent, EnterWindowMask,
    ExposureMask, KeyPress, KeyPressMask, KeyRelease, KeyReleaseMask, LeaveWindowMask,
    PointerMotionMask, VisibilityChangeMask, XClassHint, XCloseDisplay, XCreateWindow,
    XDefaultScreen, XFlush, XMapRaised, XNextEvent, XOpenDisplay, XPending, XRootWindow,
    XSetClassHint, XSetWindowAttributes, XWhitePixel, EnterNotify, XGetInputFocus, XGrabKeyboard, XDefaultRootWindow, GrabModeAsync, CurrentTime, XGrabPointer, ButtonPressMask, ButtonReleaseMask, MotionNotify,
};

pub struct X11Producer(AsyncFd<Inner>);

struct Inner {
    connection_fd: RawFd,
    display: *mut xlib::Display,
    pending_events: VecDeque<(ClientHandle, Event)>,
    window: u64,
}

impl AsRawFd for Inner {
    fn as_raw_fd(&self) -> std::os::unix::prelude::RawFd {
        self.connection_fd
    }
}

impl X11Producer {
    pub fn new() -> Result<Self> {
        let display = unsafe {
            match XOpenDisplay(ptr::null()) {
                d if d == ptr::null::<xlib::Display>() as *mut xlib::Display => {
                    Err(anyhow!("could not open display"))
                }
                display => Ok(display),
            }
        }?;
        let screen = unsafe { XDefaultScreen(display) };
        log::warn!("screen: {screen}");

        let root_window = unsafe { XRootWindow(display, screen) };
        log::warn!("root: {root_window}");
        let mut attr: XSetWindowAttributes = unsafe { std::mem::zeroed() };
        attr.override_redirect = true as i32;
        attr.background_pixel = unsafe { XWhitePixel(display, screen) };
        attr.event_mask = ExposureMask
            | VisibilityChangeMask
            | KeyPressMask
            | KeyReleaseMask
            | PointerMotionMask
            | EnterWindowMask
            | LeaveWindowMask;
        let window = unsafe {
            XCreateWindow(
                display,
                root_window,
                0,                     /* x */
                0,                     /* y */
                2560,                  /* min width */
                10,                    /* min height */
                0,                     /* border width */
                CopyFromParent,        /* depth */
                CopyFromParent as u32, /* class */
                ptr::null_mut(),       /* Visual *visual */
                CWOverrideRedirect | CWBackPixel | CWEventMask,
                &mut attr as *mut _,
            )
        };
        let mut name: String = "lan-mouse".into();
        let name = name.as_mut_ptr();

        let mut class_hint = XClassHint {
            res_name: name as *mut i8,
            res_class: name as *mut i8,
        };
        unsafe { XSetClassHint(display, window, &mut class_hint as *mut _) };
        log::warn!("window: {window}");
        // unsafe { XSelectInput(display, window, event_mask as i64) };
        unsafe { XMapRaised(display, window) };
        unsafe { XFlush(display) };

        /* can not fail */
        let connection_fd = unsafe { xlib::XConnectionNumber(display) };
        let pending_events = VecDeque::new();
        let inner = Inner {
            connection_fd,
            display,
            window,
            pending_events,
        };
        let async_fd = AsyncFd::new(inner)?;
        Ok(X11Producer(async_fd))
    }
}

impl Inner {
    fn decode(&mut self, xevent: xlib::XEvent) -> Option<(u32, Event)> {
        log::info!("decoding {xevent:?}");
        match xevent.get_type() {
            t if t == KeyPress || t == KeyRelease => {
                let key_event: xlib::XKeyEvent = unsafe { xevent.key };
                let code = key_event.keycode;
                let linux_code = code - 8;
                let state = (xevent.get_type() == KeyPress) as u8;
                return Some((
                    0,
                    Event::Keyboard(crate::event::KeyboardEvent::Key {
                        time: 0,
                        key: linux_code,
                        state,
                    }),
                ));
            }
            t if t == EnterNotify => {
                let mut prev_win = 0;
                unsafe { XGetInputFocus(self.display, &mut self.window as *mut _, &mut prev_win as * mut _) };
                unsafe { XGrabKeyboard(self.display, XDefaultRootWindow(self.display), true as i32, GrabModeAsync, GrabModeAsync, CurrentTime) };
                unsafe { XGrabPointer(self.display, XDefaultRootWindow(self.display), true as i32, (PointerMotionMask | ButtonPressMask | ButtonReleaseMask) as u32, GrabModeAsync, GrabModeAsync, self.window, 0, CurrentTime) };

                Some((0, Event::Enter()))
            }
            t if t == MotionNotify => {
                let pointer_event = unsafe { xevent.motion };
                let (abs_x, abs_y) = (pointer_event.x, pointer_event.y);
                let event = Event::Pointer(PointerEvent::Motion { time: 0, relative_x: abs_x as f64, relative_y: abs_y as f64 });
                Some((0, event))
            }
            _ => None,
        }
    }

    fn dispatch(&mut self) -> io::Result<bool> {
        unsafe {
            if XPending(self.display) > 0 {
                let mut xevent: xlib::XEvent = std::mem::zeroed();
                XNextEvent(self.display, &mut xevent as *mut _);
                if let Some(event) = self.decode(xevent) {
                    self.pending_events.push_back(event);
                }
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        unsafe {
            XCloseDisplay(self.display);
        }
    }
}

impl EventProducer for X11Producer {
    fn notify(&mut self, _event: ClientEvent) -> io::Result<()> {
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Stream for X11Producer {
    type Item = io::Result<(ClientHandle, Event)>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        if let Some(event) = self.0.get_mut().pending_events.pop_front() {
            return Poll::Ready(Some(Ok(event)));
        }
        loop {
            let mut guard = ready!(self.0.poll_read_ready_mut(cx))?;
            {
                let inner = guard.get_inner_mut();
                loop {
                    if match inner.dispatch() {
                        Ok(event) => event,
                        Err(e) => {
                            guard.clear_ready();
                            return Poll::Ready(Some(Err(e)));
                        }
                    } == false
                    {
                        break;
                    }
                }
            }
            guard.clear_ready();

            match guard.get_inner_mut().pending_events.pop_front() {
                Some(event) => {
                    return Poll::Ready(Some(Ok(event)));
                }
                None => continue,
            }
        }
    }
}
