use anyhow::Result;
use core::task::{Context, Poll};
use futures::Stream;
use std::ptr::addr_of_mut;
use std::{io, pin::Pin, ptr, thread};
use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::ready;
use once_cell::unsync::Lazy;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::mpsc::error::TrySendError;
use windows::Win32::Foundation::{
    BOOL, FALSE, HINSTANCE, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::MOUSEINPUT;
use windows::Win32::UI::WindowsAndMessaging::{CallNextHookEx, GetMessageW, SetWindowsHookExW, HHOOK, HOOKPROC, KBDLLHOOKSTRUCT, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_MBUTTONUP, WM_MBUTTONDOWN, PostThreadMessageW, MSG, WM_USER};

use crate::{
    capture::InputCapture,
    client::{ClientEvent, ClientHandle},
    event::Event,
};
use crate::client::Position;
use crate::event::{BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, KeyboardEvent, PointerEvent};

pub struct WindowsInputCapture {
    event_rx: Receiver<(ClientHandle, Event)>,
    msg_thread: std::thread::JoinHandle<()>,
}

enum EventType {
    ClientEvent = 0,
    Release = 1,
}

impl InputCapture for WindowsInputCapture {
    fn notify(&mut self, event: ClientEvent) -> io::Result<()> {
        unsafe {
            EVENT_BUFFER.push(event);
            if let Some(tid) = crate::capture::windows::EVENT_THREAD_ID {
                PostThreadMessageW(tid, WM_USER, WPARAM(EventType::ClientEvent as usize), LPARAM(0)).unwrap();
            }
        }
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        unsafe {
            if let Some(tid) = EVENT_THREAD_ID {
                PostThreadMessageW(tid, WM_USER, WPARAM(EventType::Release as usize), LPARAM(0)).unwrap();
            }
        }
        Ok(())
    }
}

static mut EVENT_BUFFER: Vec<ClientEvent> = Vec::new();
static mut LOCKED: bool = false;
static mut ACTIVE_CLIENT: Option<ClientHandle> = None;
static mut CLIENT_FOR_POS: Lazy<HashMap<Position, ClientHandle>> = Lazy::new(|| HashMap::new());
static mut EVENT_TX: Option<Sender<(ClientHandle, Event)>> = None;
static mut DISPLAY_INFO: Option<Vec<RECT>> = None;
static mut EVENT_THREAD_ID: Option<u32> = None;
static mut ENTRY_POINT_X: i32 = 0;
static mut ENTRY_POINT_Y: i32 = 0;

unsafe extern "system" fn mouse_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let msllhookstruct: MSLLHOOKSTRUCT =
        *std::mem::transmute::<LPARAM, *const MSLLHOOKSTRUCT>(lparam);
    // log::info!("ncode: {ncode}, {msllhookstruct:?}, Entered at: {ENTRY_POINT_X} {ENTRY_POINT_Y}");
    let mut propagate_event = !LOCKED;
    let pointer_event = match wparam {
        WPARAM(p) if p == WM_LBUTTONDOWN as usize => PointerEvent::Button {
            time: 0,
            button: BTN_LEFT,
            state: 1,
        },
        WPARAM(p) if p == WM_MBUTTONDOWN as usize => PointerEvent::Button {
            time: 0,
            button: BTN_MIDDLE,
            state: 1,
        },
        WPARAM(p) if p == WM_RBUTTONDOWN as usize => PointerEvent::Button {
            time: 0,
            button: BTN_RIGHT,
            state: 1,
        },
        WPARAM(p) if p == WM_LBUTTONUP as usize => PointerEvent::Button {
            time: 0,
            button: BTN_LEFT,
            state: 0,
        },
        WPARAM(p) if p == WM_MBUTTONUP as usize => PointerEvent::Button {
            time: 0,
            button: BTN_MIDDLE,
            state: 0,
        },
        WPARAM(p) if p == WM_RBUTTONUP as usize => PointerEvent::Button {
            time: 0,
            button: BTN_RIGHT,
            state: 0,
        },
        WPARAM(p) if p == WM_MOUSEMOVE as usize => {
            static mut PREV_X: Option<i32> = None;
            static mut PREV_Y: Option<i32> = None;
            let (x, y) = (msllhookstruct.pt.x, msllhookstruct.pt.y);
            let (px, py) = (PREV_X.unwrap_or(x), PREV_Y.unwrap_or(y));
            PREV_X.replace(x);
            PREV_Y.replace(y);
            if LOCKED == false {
                if let Some(pos) = entered_barrier(px, py, x, y, &DISPLAY_INFO.as_ref().unwrap()) {
                    if let Some(client) = CLIENT_FOR_POS.get(&pos) {
                        propagate_event = true;
                        ACTIVE_CLIENT.replace(*client);
                        LOCKED = true;
                        /* windows is weird */
                        match pos {
                            Position::Right => {
                                ENTRY_POINT_X = x - 1;
                                ENTRY_POINT_Y = y;
                            }
                            Position::Bottom => {
                                ENTRY_POINT_X = x;
                                ENTRY_POINT_Y = y - 1;
                            }
                            _ => {
                                ENTRY_POINT_X = x;
                                ENTRY_POINT_Y = y;
                            }
                        }
                        loop {
                            /* enter event must not get lost under any circumstances */
                            log::info!("ENTERED @ ({px},{py}) -> ({x},{y})");
                            match EVENT_TX.as_ref().unwrap().try_send((0, Event::Enter())) {
                                Err(TrySendError::Full(_)) => continue,
                                Err(TrySendError::Closed(_)) => panic!("channel closed"),
                                Ok(e) => break,
                            }
                        }
                    }
                }
            }
            let (dx, dy) = (x - ENTRY_POINT_X, y - ENTRY_POINT_Y);
            PointerEvent::Motion {
                time: 0,
                relative_x: dx as f64,
                relative_y: dy as f64,
            }
        },
        WPARAM(p) if p == WM_MOUSEWHEEL as usize => PointerEvent::Axis {
            time: 0,
            axis: 0,
            value: msllhookstruct.mouseData as i32 as f64,
        },
        _ => return CallNextHookEx(HHOOK::default(), ncode, wparam, lparam),
    };
    if !propagate_event {
        if let Some(client) = ACTIVE_CLIENT {
            let event = Event::Pointer(pointer_event);
            let event = (client, event);

            if let Err(e) = EVENT_TX.as_ref().unwrap().try_send(event) {
                log::warn!("e: {e}");
            }
        }
        LRESULT(1) /* don't pass event to applications */
    } else {
        CallNextHookEx(HHOOK::default(), ncode, wparam, lparam)
    }
}

unsafe extern "system" fn kybrd_proc(i: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let kybrdllhookstruct: KBDLLHOOKSTRUCT =
        *std::mem::transmute::<LPARAM, *const KBDLLHOOKSTRUCT>(lparam);
    let scancode = kybrdllhookstruct.scanCode;
    let key_event = match wparam {
        WPARAM(p) if p == WM_KEYDOWN as usize => Some(KeyboardEvent::Key {
            time: 0, key: scancode, state: 1,
        }),
        WPARAM(p) if p == WM_KEYUP as usize => Some(KeyboardEvent::Key {
            time: 0, key: scancode, state: 0,
        }),
        WPARAM(p) if p == WM_SYSKEYDOWN as usize => {
            log::debug!("SYS KEY DOWN {scancode}");
            None
        },
        WPARAM(p) if p == WM_SYSKEYUP as usize => {
            log::debug!("SYS KEY UP {scancode}");
            None
        },
        _ => None
    };
    if LOCKED {
        if let Some(key_event) = key_event {
            if let Some(client) = ACTIVE_CLIENT {
                let event = Event::Keyboard(key_event);
                let event = (client, event);

                if let Err(e) = EVENT_TX.as_ref().unwrap().try_send(event) {
                    log::warn!("e: {e}");
                }
            }
        }
        LRESULT(1) /* don't pass event to applications */
    } else {
        CallNextHookEx(HHOOK::default(), i, wparam, lparam)
    }
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

fn is_at_dp_boundary(x: i32, y: i32, display: &RECT, pos: Position) -> bool {
    match pos {
        Position::Left => display.left == x,
        Position::Right => display.right == x,
        Position::Top => display.top == y,
        Position::Bottom => display.bottom == y,
    }
}

fn is_at_boundary(x: i32, y: i32, displays: &[RECT], pos: Position) -> bool {
    /* point is at exactly one boundary.
     * 0 boundaries means the point is somewhere in the middle
     * 2 boundaries means point is between two monitors,
     * which should be ignored */
    displays.iter().filter(|&d| is_at_dp_boundary(x, y, d, pos)).count() == 1
}

fn moved_into_boundary(px: i32, py: i32, x: i32, y: i32, displays: &[RECT], pos: Position) -> bool {
    /* was not at boundary, but is now */
    !is_at_boundary(px, py, displays, pos) && is_at_boundary(x, y, displays, pos)
}

fn entered_barrier(px: i32, py: i32, x: i32, y: i32, displays: &[RECT]) -> Option<Position> {
    for pos in [Position::Left, Position::Right, Position::Top, Position::Bottom] {
        if moved_into_boundary(px, py, x, y, displays, pos) {
            return Some(pos);
        }
    }
    return None;
}

impl WindowsInputCapture {
    pub(crate) fn new() -> Result<Self> {
        unsafe {
            let (tx, rx) = channel(10);
            EVENT_TX.replace(tx);
            let msg_thread = thread::spawn(|| {
                EVENT_THREAD_ID.replace(GetCurrentThreadId());
                let mouse_proc: HOOKPROC = Some(mouse_proc);
                let kybrd_proc: HOOKPROC = Some(kybrd_proc);
                let display_info: Vec<RECT> = get_display_regions();
                log::info!("displays: {display_info:?}");
                DISPLAY_INFO.replace(display_info);
                let _ = SetWindowsHookExW(WH_MOUSE_LL, mouse_proc, HINSTANCE::default(), 0).unwrap();
                let _ = SetWindowsHookExW(WH_KEYBOARD_LL, kybrd_proc, HINSTANCE::default(), 0).unwrap();
                loop {
                    let mut msg = std::mem::zeroed();
                    let res = GetMessageW(addr_of_mut!(msg), HWND::default(), 0, 0);
                    // mouse / keybrd proc do not actually return a message
                    if msg.wParam.0 == EventType::Release as usize {
                        LOCKED = false;
                    } else if msg.wParam.0 == EventType::ClientEvent as usize {
                        while let Some(event) = EVENT_BUFFER.pop() {
                            match event {
                                ClientEvent::Create(handle, pos) => {
                                    CLIENT_FOR_POS.insert(pos, handle);
                                },
                                ClientEvent::Destroy(handle) => {
                                    for pos in [Position::Left, Position::Right, Position::Top, Position::Bottom] {
                                        if CLIENT_FOR_POS.get(&pos).copied() == Some(handle) {
                                            CLIENT_FOR_POS.remove(&pos);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            });
            Ok(Self { msg_thread, event_rx: rx })
        }
    }
}

impl Stream for WindowsInputCapture {
    type Item = io::Result<(ClientHandle, Event)>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match ready!(self.event_rx.poll_recv(cx)) {
            None => Poll::Ready(None),
            Some(e) => Poll::Ready(Some(Ok(e))),
        }
    }
}
