use anyhow::Result;
use core::task::{Context, Poll};
use futures::Stream;
use std::ptr::addr_of_mut;
use std::{io, pin::Pin};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, SetWindowsHookExW, HHOOK, HOOKPROC,
    MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_RBUTTONDOWN,
};

use crate::{
    capture::InputCapture,
    client::{ClientEvent, ClientHandle},
    event::Event,
};

pub struct WindowsInputCapture {}

impl InputCapture for WindowsInputCapture {
    fn notify(&mut self, _event: ClientEvent) -> io::Result<()> {
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        Ok(())
    }
}

unsafe extern "system" fn mouse_proc(i: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match wparam {
        WPARAM(p) if p == WM_LBUTTONDOWN as usize => log::info!("LEFT BUTTON"),
        WPARAM(p) if p == WM_RBUTTONDOWN as usize => log::info!("RIGHT BUTTON"),
        WPARAM(p) if p == WM_MOUSEMOVE as usize => {
            let movement: MSLLHOOKSTRUCT =
                *std::mem::transmute::<LPARAM, *const MSLLHOOKSTRUCT>(lparam);
            let (absx, absy) = (movement.pt.x, movement.pt.y);
            log::info!("MOUSE MOVE: {absx},{absy}");
        }
        _ => {}
    };
    CallNextHookEx(HHOOK::default(), i, wparam, lparam)
}

impl WindowsInputCapture {
    pub(crate) fn new() -> Result<Self> {
        unsafe {
            let hookproc: HOOKPROC = Some(mouse_proc);
            let _ = SetWindowsHookExW(WH_MOUSE_LL, hookproc, HINSTANCE::default(), 0).unwrap();
            let mut i = 0;
            loop {
                let mut msg = std::mem::zeroed();
                GetMessageW(addr_of_mut!(msg), HWND::default(), 0, 0);
                log::info!("msg {i}: {msg:?}");
                i += 1;
            }
        }
    }
}

impl Stream for WindowsInputCapture {
    type Item = io::Result<(ClientHandle, Event)>;
    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
