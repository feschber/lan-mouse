use anyhow::Result;
use core::task::{Context, Poll};
use futures::Stream;
use once_cell::unsync::Lazy;

use std::collections::HashMap;
use std::ptr::addr_of_mut;

use std::task::ready;
use std::{io, pin::Pin, thread};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use windows::Win32::Foundation::{
    BOOL, FALSE, HINSTANCE, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::System::Threading::GetCurrentThreadId;

use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PostThreadMessageW, SetWindowsHookExW, HHOOK, HOOKPROC,
    KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_USER,
};

use crate::client::Position;
use crate::event::{KeyboardEvent, PointerEvent, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT};
use crate::{
    capture::InputCapture,
    client::{ClientEvent, ClientHandle},
    event::Event,
};

pub struct WindowsInputCapture {
    event_rx: Receiver<(ClientHandle, Event)>,
    msg_thread: Option<std::thread::JoinHandle<()>>,
}

enum EventType {
    ClientEvent = 0,
    Release = 1,
    Exit = 2,
}

unsafe fn signal_message_thread(event_type: EventType) {
    if let Some(event_tid) = get_event_tid() {
        PostThreadMessageW(event_tid, WM_USER, WPARAM(event_type as usize), LPARAM(0)).unwrap();
    }
}

impl InputCapture for WindowsInputCapture {
    fn notify(&mut self, event: ClientEvent) -> io::Result<()> {
        unsafe {
            EVENT_BUFFER.push(event);
            signal_message_thread(EventType::ClientEvent);
        }
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        unsafe { signal_message_thread(EventType::Release) };
        Ok(())
    }
}

static mut EVENT_BUFFER: Vec<ClientEvent> = Vec::new();
static mut ACTIVE_CLIENT: Option<ClientHandle> = None;
static mut CLIENT_FOR_POS: Lazy<HashMap<Position, ClientHandle>> = Lazy::new(HashMap::new);
static mut EVENT_TX: Option<Sender<(ClientHandle, Event)>> = None;
static mut DISPLAY_INFO: Option<Vec<RECT>> = None;
static mut EVENT_THREAD_ID: Option<u32> = None;
unsafe fn set_event_tid(tid: u32) {
    EVENT_THREAD_ID.replace(tid);
}
unsafe fn get_event_tid() -> Option<u32> {
    EVENT_THREAD_ID
}

static mut ENTRY_POINT: (i32, i32) = (0, 0);

fn to_mouse_event(wparam: WPARAM, lparam: LPARAM) -> Option<PointerEvent> {
    let mouse_low_level: MSLLHOOKSTRUCT =
        unsafe { *std::mem::transmute::<LPARAM, *const MSLLHOOKSTRUCT>(lparam) };
    match wparam {
        WPARAM(p) if p == WM_LBUTTONDOWN as usize => Some(PointerEvent::Button {
            time: 0,
            button: BTN_LEFT,
            state: 1,
        }),
        WPARAM(p) if p == WM_MBUTTONDOWN as usize => Some(PointerEvent::Button {
            time: 0,
            button: BTN_MIDDLE,
            state: 1,
        }),
        WPARAM(p) if p == WM_RBUTTONDOWN as usize => Some(PointerEvent::Button {
            time: 0,
            button: BTN_RIGHT,
            state: 1,
        }),
        WPARAM(p) if p == WM_LBUTTONUP as usize => Some(PointerEvent::Button {
            time: 0,
            button: BTN_LEFT,
            state: 0,
        }),
        WPARAM(p) if p == WM_MBUTTONUP as usize => Some(PointerEvent::Button {
            time: 0,
            button: BTN_MIDDLE,
            state: 0,
        }),
        WPARAM(p) if p == WM_RBUTTONUP as usize => Some(PointerEvent::Button {
            time: 0,
            button: BTN_RIGHT,
            state: 0,
        }),
        WPARAM(p) if p == WM_MOUSEMOVE as usize => unsafe {
            let (x, y) = (mouse_low_level.pt.x, mouse_low_level.pt.y);
            let (ex, ey) = ENTRY_POINT;
            let (dx, dy) = (x - ex, y - ey);
            Some(PointerEvent::Motion {
                time: 0,
                relative_x: dx as f64,
                relative_y: dy as f64,
            })
        },
        WPARAM(p) if p == WM_MOUSEWHEEL as usize => Some(PointerEvent::Axis {
            time: 0,
            axis: 0,
            value: -(mouse_low_level.mouseData as i32) as f64,
        }),
        _ => None,
    }
}

unsafe fn to_key_event(wparam: WPARAM, lparam: LPARAM) -> Option<KeyboardEvent> {
    let kybrdllhookstruct: KBDLLHOOKSTRUCT =
        *std::mem::transmute::<LPARAM, *const KBDLLHOOKSTRUCT>(lparam);
    let scancode = kybrdllhookstruct.scanCode;
    match wparam {
        WPARAM(p) if p == WM_KEYDOWN as usize => Some(KeyboardEvent::Key {
            time: 0,
            key: scancode,
            state: 1,
        }),
        WPARAM(p) if p == WM_KEYUP as usize => Some(KeyboardEvent::Key {
            time: 0,
            key: scancode,
            state: 0,
        }),
        WPARAM(p) if p == WM_SYSKEYDOWN as usize => Some(KeyboardEvent::Key {
            time: 0,
            key: scancode,
            state: 1,
        }),
        WPARAM(p) if p == WM_SYSKEYUP as usize => Some(KeyboardEvent::Key {
            time: 0,
            key: scancode,
            state: 1,
        }),
        _ => None,
    }
}

///
/// correct the entry point according to display coordinates
///
/// # Arguments
///
/// * `entry_point`: coordinates, where the mouse entered the barrier
/// * `pos`: position of the barrier relative to the display
///
/// returns: (i32, i32), the corrected entry point
///
fn correct_entry_point(entry_point: (i32, i32), pos: Position) -> (i32, i32) {
    let (x, y) = entry_point;
    match pos {
        Position::Right => (x - 1, y),
        Position::Bottom => (x, y - 1),
        _ => (x, y),
    }
}

unsafe fn send_blocking(event: Event) {
    loop {
        /* enter event must not get lost under any circumstances */
        match EVENT_TX.as_ref().unwrap().try_send((0, event)) {
            Err(TrySendError::Full(_)) => continue,
            Err(TrySendError::Closed(_)) => panic!("channel closed"),
            Ok(_e) => break,
        }
    }
}

unsafe fn check_client_activation(wparam: WPARAM, lparam: LPARAM) -> bool {
    if wparam.0 != WM_MOUSEMOVE as usize {
        return ACTIVE_CLIENT.is_some();
    }
    let mouse_low_level: MSLLHOOKSTRUCT =
        unsafe { *std::mem::transmute::<LPARAM, *const MSLLHOOKSTRUCT>(lparam) };
    static mut PREV_X: Option<i32> = None;
    static mut PREV_Y: Option<i32> = None;
    let (x, y) = (mouse_low_level.pt.x, mouse_low_level.pt.y);
    let (px, py) = (PREV_X.unwrap_or(x), PREV_Y.unwrap_or(y));
    PREV_X.replace(x);
    PREV_Y.replace(y);

    /* next event is the first actual event */
    let ret = ACTIVE_CLIENT.is_some();

    /* client already active, no need to check */
    if ACTIVE_CLIENT.is_some() {
        return ret;
    }

    /* check if a client was activated */
    let Some(pos) = entered_barrier(px, py, x, y, DISPLAY_INFO.as_ref().unwrap()) else {
        return ret;
    };

    /* check if a client is registered for the barrier */
    let Some(client) = CLIENT_FOR_POS.get(&pos) else {
        return ret;
    };

    /* update active client and entry point */
    ACTIVE_CLIENT.replace(*client);
    ENTRY_POINT = correct_entry_point((x, y), pos);

    /* notify main thread */
    log::debug!("ENTERED @ ({px},{py}) -> ({x},{y})");
    send_blocking(Event::Enter());

    ret
}

unsafe extern "system" fn mouse_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let active = check_client_activation(wparam, lparam);

    /* no client was active */
    if !active {
        return CallNextHookEx(HHOOK::default(), ncode, wparam, lparam);
    }

    /* get active client if any */
    let Some(client) = ACTIVE_CLIENT else {
        return LRESULT(1);
    };

    /* convert to lan-mouse event */
    let Some(pointer_event) = to_mouse_event(wparam, lparam) else {
        return LRESULT(1);
    };
    let event = (client, Event::Pointer(pointer_event));

    /* notify mainthread (drop events if sending too fast) */
    if let Err(e) = EVENT_TX.as_ref().unwrap().try_send(event) {
        log::warn!("e: {e}");
    }

    /* don't pass event to applications */
    LRESULT(1)
}

unsafe extern "system" fn kybrd_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    /* get active client if any */
    let Some(client) = ACTIVE_CLIENT else {
        return CallNextHookEx(HHOOK::default(), ncode, wparam, lparam);
    };

    /* convert to key event */
    let Some(key_event) = to_key_event(wparam, lparam) else {
        return LRESULT(1);
    };
    let event = (client, Event::Keyboard(key_event));

    if let Err(e) = EVENT_TX.as_ref().unwrap().try_send(event) {
        log::warn!("e: {e}");
    }

    /* don't pass event to applications */
    LRESULT(1)
}

unsafe extern "system" fn monitor_enum_proc(
    hmon: HMONITOR,
    _hdc: HDC,
    _lprect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let monitors = lparam.0 as *mut Vec<HMONITOR>;
    (*monitors).push(hmon);
    TRUE // continue enumeration
}

fn get_display_regions() -> Vec<RECT> {
    unsafe {
        let mut display_rects = vec![];
        let mut monitors: Vec<HMONITOR> = Vec::new();
        let _displays = EnumDisplayMonitors(
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
    /* check if point is at exactly one boundary i.e. at the edge of a display
     * that does not intersect another display.
     * 0 boundaries means the point is somewhere in the middle
     * 2 boundaries means point is between two monitors, which should be ignored */
    displays
        .iter()
        .filter(|&d| is_at_dp_boundary(x, y, d, pos))
        .count()
        == 1
}

fn moved_into_boundary(px: i32, py: i32, x: i32, y: i32, displays: &[RECT], pos: Position) -> bool {
    /* was not at boundary, but is now */
    !is_at_boundary(px, py, displays, pos) && is_at_boundary(x, y, displays, pos)
}

fn entered_barrier(px: i32, py: i32, x: i32, y: i32, displays: &[RECT]) -> Option<Position> {
    [
        Position::Left,
        Position::Right,
        Position::Top,
        Position::Bottom,
    ]
    .into_iter()
    .find(|&pos| moved_into_boundary(px, py, x, y, displays, pos))
}

fn get_msg() -> Option<MSG> {
    unsafe {
        let mut msg = std::mem::zeroed();
        let ret = GetMessageW(addr_of_mut!(msg), HWND::default(), 0, 0);
        match ret.0 {
            0 => None,
            x if x > 0 => Some(msg),
            _ => panic!("error in GetMessageW"),
        }
    }
}

fn message_thread() {
    unsafe {
        set_event_tid(GetCurrentThreadId());
        let mouse_proc: HOOKPROC = Some(mouse_proc);
        let kybrd_proc: HOOKPROC = Some(kybrd_proc);
        let display_info: Vec<RECT> = get_display_regions();
        log::info!("displays: {display_info:?}");
        DISPLAY_INFO.replace(display_info);
        let _ = SetWindowsHookExW(WH_MOUSE_LL, mouse_proc, HINSTANCE::default(), 0).unwrap();
        let _ = SetWindowsHookExW(WH_KEYBOARD_LL, kybrd_proc, HINSTANCE::default(), 0).unwrap();
        loop {
            // mouse / keybrd proc do not actually return a message
            match get_msg() {
                None => break,
                Some(msg) => match msg.wParam.0 {
                    x if x == EventType::Exit as usize => break,
                    x if x == EventType::Release as usize => {
                        let _ = ACTIVE_CLIENT.take();
                    }
                    x if x == EventType::ClientEvent as usize => {
                        while let Some(event) = EVENT_BUFFER.pop() {
                            update_clients(event)
                        }
                    }
                    _ => {}
                },
            }
        }
    }
}

fn update_clients(client_event: ClientEvent) {
    match client_event {
        ClientEvent::Create(handle, pos) => {
            unsafe { CLIENT_FOR_POS.insert(pos, handle) };
        }
        ClientEvent::Destroy(handle) => {
            for pos in [
                Position::Left,
                Position::Right,
                Position::Top,
                Position::Bottom,
            ] {
                if unsafe { CLIENT_FOR_POS.get(&pos).copied() } == Some(handle) {
                    unsafe { CLIENT_FOR_POS.remove(&pos) };
                }
            }
        }
    }
}

impl WindowsInputCapture {
    pub(crate) fn new() -> Result<Self> {
        unsafe {
            let (tx, rx) = channel(10);
            EVENT_TX.replace(tx);
            let msg_thread = Some(thread::spawn(message_thread));
            Ok(Self {
                msg_thread,
                event_rx: rx,
            })
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

impl Drop for WindowsInputCapture {
    fn drop(&mut self) {
        unsafe { signal_message_thread(EventType::Exit) };
        let _ = self.msg_thread.take().unwrap().join();
    }
}
