use anyhow::Result;
use core::task::{Context, Poll};
use futures::Stream;
use std::ptr::addr_of_mut;
use std::{io, pin::Pin, ptr};
use windows::Win32::Foundation::{
    BOOL, FALSE, HINSTANCE, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::UI::Input::KeyboardAndMouse::MOUSEINPUT;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, SetWindowsHookExW, HHOOK, HOOKPROC, KBDLLHOOKSTRUCT,
    MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP,
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

static mut LOCKED: bool = false;

unsafe extern "system" fn mouse_proc(i: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let msllhookstruct: MSLLHOOKSTRUCT =
        *std::mem::transmute::<LPARAM, *const MSLLHOOKSTRUCT>(lparam);
    let (x, y) = (msllhookstruct.pt.x, msllhookstruct.pt.y);
    match wparam {
        WPARAM(p) if p == WM_LBUTTONDOWN as usize => log::info!("LEFT BUTTON DOWN"),
        WPARAM(p) if p == WM_RBUTTONDOWN as usize => log::info!("RIGHT BUTTON DOWN"),
        WPARAM(p) if p == WM_LBUTTONUP as usize => log::info!("LEFT BUTTON UP"),
        WPARAM(p) if p == WM_RBUTTONUP as usize => log::info!("RIGHT BUTTON UP"),
        WPARAM(p) if p == WM_MOUSEMOVE as usize => log::info!("MOUSE MOVE: {x},{y}"),
        WPARAM(p) if p == WM_MOUSEWHEEL as usize => log::info!("SCROLL {:?}", wparam),
        _ => {}
    };
    if LOCKED {
        LRESULT(1) /* dont pass event */
    } else {
        CallNextHookEx(HHOOK::default(), i, wparam, lparam)
    }
}

unsafe extern "system" fn kybrd_proc(i: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let kybrdllhookstruct: KBDLLHOOKSTRUCT =
        *std::mem::transmute::<LPARAM, *const KBDLLHOOKSTRUCT>(lparam);
    let scancode = kybrdllhookstruct.scanCode;
    match wparam {
        WPARAM(p) if p == WM_KEYDOWN as usize => log::info!("KEY DOWN {scancode}"),
        WPARAM(p) if p == WM_KEYUP as usize => log::info!("KEY UP {scancode}"),
        WPARAM(p) if p == WM_SYSKEYDOWN as usize => log::info!("SYS KEY DOWN {scancode}"),
        WPARAM(p) if p == WM_SYSKEYUP as usize => log::info!("SYS KEY UP {scancode}"),
        _ => {}
    };
    CallNextHookEx(HHOOK::default(), i, wparam, lparam)
}

unsafe extern "system" fn monitor_enum_proc(
    hmon: HMONITOR,
    hdc: HDC,
    lprect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let mut monitors = lparam.0 as *mut Vec<HMONITOR>;
    (*monitors).push(hmon);
    TRUE // continue enumeration
}

fn get_display_regions() -> Vec<RECT> {
    unsafe {
        let mut display_rects = vec![];
        let mut monitors: Vec<HMONITOR> = Vec::new();
        let displays = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(monitor_enum_proc),
            LPARAM(&mut monitors as *mut Vec<HMONITOR> as isize),
        );
        for monitor in monitors {
            let mut monitor_info: MONITORINFO = std::mem::zeroed();
            monitor_info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut monitor_info) == FALSE {
                panic!();
            }
            display_rects.push(monitor_info.rcMonitor);
        }
        display_rects
    }
}

impl WindowsInputCapture {
    pub(crate) fn new() -> Result<Self> {
        unsafe {
            let mouse_proc: HOOKPROC = Some(mouse_proc);
            let kybrd_proc: HOOKPROC = Some(kybrd_proc);
            let display_info: Vec<RECT> = get_display_regions();
            log::info!("displays: {display_info:?}");
            let _ = SetWindowsHookExW(WH_MOUSE_LL, mouse_proc, HINSTANCE::default(), 0).unwrap();
            let _ = SetWindowsHookExW(WH_KEYBOARD_LL, kybrd_proc, HINSTANCE::default(), 0).unwrap();
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
