use anyhow::{anyhow, Result};
use core::task::{Context, Poll};
use futures::Stream;
use std::ptr::null;
use std::ptr::null_mut;
use std::sync::mpsc::SyncSender;
use std::{io, pin::Pin};

use anyhow::{anyhow, Result};

use winapi::shared::minwindef::{LPARAM, LRESULT, WPARAM};
use winapi::um::{
    libloaderapi::GetModuleHandleW,
    winuser::{
        CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
        UnhookWindowsHookEx, MOUSEHOOKSTRUCT, MSG, WH_MOUSE_LL, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
        WM_LBUTTONUP, WM_MBUTTONDBLCLK, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
        WM_MOUSEWHEEL, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDBLCLK,
        WM_XBUTTONDOWN, WM_XBUTTONUP,
    },
};

use crate::{
    client::{ClientEvent, ClientHandle},
    event::Event,
    producer::EventProducer,
};

pub struct WindowsProducer {}

impl EventProducer for WindowsProducer {
    fn notify(&mut self, _event: ClientEvent) -> io::Result<()> {
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl WindowsProducer {
    pub(crate) fn new() -> Result<Self> {
        Err(anyhow!("not implemented"))
    }
}

impl Stream for WindowsProducer {
    type Item = io::Result<(ClientHandle, Event)>;
    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

pub unsafe extern "system" fn mouse_handler(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(null_mut(), code, wparam, lparam);
    }
    let mouse_struct: MOUSEHOOKSTRUCT = *(lparam as *mut MOUSEHOOKSTRUCT);
    match wparam as u32 {
        WM_LBUTTONDOWN => println!("wm_lbuttondown"),
        WM_LBUTTONUP => println!("wm_lbuttonup"),
        WM_LBUTTONDBLCLK => println!("wm_lbuttondblclk"),
        WM_RBUTTONDOWN => println!("wm_rbuttondown"),
        WM_RBUTTONUP => println!("wm_rbuttonup"),
        WM_RBUTTONDBLCLK => println!("wm_rbuttondblclk"),
        WM_MBUTTONDOWN => println!("wm_mbuttondown"),
        WM_MBUTTONUP => println!("wm_mbuttonup"),
        WM_MBUTTONDBLCLK => println!("wm_mbuttondblclk"),
        WM_MOUSEWHEEL => println!("wm_mousewheel"),
        WM_XBUTTONDOWN => println!("wm_xbuttondown"),
        WM_XBUTTONUP => println!("wm_xbuttonup"),
        WM_XBUTTONDBLCLK => println!("wm_xbuttondblclk"),
        WM_MOUSEHWHEEL => println!("wm_mousehwheel"),
        WM_MOUSEMOVE => {
            print!("                                                        \r");
            print!("{}, {}, {}", code, mouse_struct.pt.x, mouse_struct.pt.y);
        }
        _ => panic!("invalid mouse event: {wparam:#0x}"),
    }
    0 as LRESULT
}

pub fn run(
    produce_tx: SyncSender<(Event, ClientHandle)>,
    _server: Server,
    _clients: Vec<Client>,
) -> Result<()> {
    unsafe {
        let hinstance = GetModuleHandleW(null());
        let mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_handler), hinstance, 0);

        let mut msg: MSG = std::mem::zeroed();
        loop {
            // Get message from message queue
            if GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            } else {
                // Return on error (<0) or exit (=0) cases
                UnhookWindowsHookEx(mouse_hook);
                return Err(anyhow!("GetMessageW failed: {}", msg.wParam));
            }
        }
    }
}
