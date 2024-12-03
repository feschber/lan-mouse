use async_trait::async_trait;
use core::task::{Context, Poll};
use futures::Stream;
use once_cell::unsync::Lazy;

use std::collections::HashSet;
use std::ptr::{addr_of, addr_of_mut};

use futures::executor::block_on;
use std::default::Default;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Mutex};
use std::task::ready;
use std::{pin::Pin, thread};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{FALSE, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplaySettingsW, DEVMODEW, DISPLAY_DEVICEW,
    DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, ENUM_CURRENT_SETTINGS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;

use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DispatchMessageW, GetMessageW, PostThreadMessageW,
    RegisterClassW, SetWindowsHookExW, TranslateMessage, EDD_GET_DEVICE_INTERFACE_NAME, HHOOK,
    HMENU, HOOKPROC, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, MSG, MSLLHOOKSTRUCT, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WINDOW_STYLE, WM_DISPLAYCHANGE, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_USER, WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSW,
    WNDPROC,
};

use input_event::{
    scancode::{self, Linux},
    Event, KeyboardEvent, PointerEvent, BTN_BACK, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT,
};

use super::{Capture, CaptureError, CaptureEvent, Position};

enum Request {
    Create(Position),
    Destroy(Position),
}

pub struct WindowsInputCapture {
    event_rx: Receiver<(Position, CaptureEvent)>,
    msg_thread: Option<std::thread::JoinHandle<()>>,
}

enum EventType {
    Request = 0,
    Release = 1,
    Exit = 2,
}

unsafe fn signal_message_thread(event_type: EventType) {
    if let Some(event_tid) = get_event_tid() {
        PostThreadMessageW(event_tid, WM_USER, WPARAM(event_type as usize), LPARAM(0)).unwrap();
    } else {
        panic!();
    }
}

#[async_trait]
impl Capture for WindowsInputCapture {
    async fn create(&mut self, pos: Position) -> Result<(), CaptureError> {
        unsafe {
            {
                let mut requests = REQUEST_BUFFER.lock().unwrap();
                requests.push(Request::Create(pos));
            }
            signal_message_thread(EventType::Request);
        }
        Ok(())
    }

    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError> {
        unsafe {
            {
                let mut requests = REQUEST_BUFFER.lock().unwrap();
                requests.push(Request::Destroy(pos));
            }
            signal_message_thread(EventType::Request);
        }
        Ok(())
    }

    async fn release(&mut self) -> Result<(), CaptureError> {
        unsafe { signal_message_thread(EventType::Release) };
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }
}

static mut REQUEST_BUFFER: Mutex<Vec<Request>> = Mutex::new(Vec::new());
static mut ACTIVE_CLIENT: Option<Position> = None;
static mut CLIENTS: Lazy<HashSet<Position>> = Lazy::new(HashSet::new);
static mut EVENT_TX: Option<Sender<(Position, CaptureEvent)>> = None;
static mut EVENT_THREAD_ID: AtomicU32 = AtomicU32::new(0);
unsafe fn set_event_tid(tid: u32) {
    EVENT_THREAD_ID.store(tid, Ordering::SeqCst);
}
unsafe fn get_event_tid() -> Option<u32> {
    match EVENT_THREAD_ID.load(Ordering::SeqCst) {
        0 => None,
        id => Some(id),
    }
}

static mut ENTRY_POINT: (i32, i32) = (0, 0);

fn to_mouse_event(wparam: WPARAM, lparam: LPARAM) -> Option<PointerEvent> {
    let mouse_low_level: MSLLHOOKSTRUCT = unsafe { *(lparam.0 as *const MSLLHOOKSTRUCT) };
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
            let (dx, dy) = (dx as f64, dy as f64);
            Some(PointerEvent::Motion { time: 0, dx, dy })
        },
        WPARAM(p) if p == WM_MOUSEWHEEL as usize => Some(PointerEvent::AxisDiscrete120 {
            axis: 0,
            value: -(mouse_low_level.mouseData as i32 >> 16),
        }),
        WPARAM(p) if p == WM_XBUTTONDOWN as usize || p == WM_XBUTTONUP as usize => {
            let hb = mouse_low_level.mouseData >> 16;
            let button = match hb {
                1 => BTN_BACK,
                2 => BTN_FORWARD,
                _ => {
                    log::warn!("unknown mouse button");
                    return None;
                }
            };
            Some(PointerEvent::Button {
                time: 0,
                button,
                state: if p == WM_XBUTTONDOWN as usize { 1 } else { 0 },
            })
        }
        w => {
            log::warn!("unknown mouse event: {w:?}");
            None
        }
    }
}

unsafe fn to_key_event(wparam: WPARAM, lparam: LPARAM) -> Option<KeyboardEvent> {
    let kybrdllhookstruct: KBDLLHOOKSTRUCT = *(lparam.0 as *const KBDLLHOOKSTRUCT);
    let mut scan_code = kybrdllhookstruct.scanCode;
    log::trace!("scan_code: {scan_code}");
    if kybrdllhookstruct.flags.contains(LLKHF_EXTENDED) {
        scan_code |= 0xE000;
    }
    let Ok(win_scan_code) = scancode::Windows::try_from(scan_code) else {
        log::warn!("failed to translate to windows scancode: {scan_code}");
        return None;
    };
    log::trace!("windows_scan: {win_scan_code:?}");
    let Ok(linux_scan_code): Result<Linux, ()> = win_scan_code.try_into() else {
        log::warn!("failed to translate into linux scancode: {win_scan_code:?}");
        return None;
    };
    log::trace!("windows_scan: {linux_scan_code:?}");
    let scan_code = linux_scan_code as u32;
    match wparam {
        WPARAM(p) if p == WM_KEYDOWN as usize => Some(KeyboardEvent::Key {
            time: 0,
            key: scan_code,
            state: 1,
        }),
        WPARAM(p) if p == WM_KEYUP as usize => Some(KeyboardEvent::Key {
            time: 0,
            key: scan_code,
            state: 0,
        }),
        WPARAM(p) if p == WM_SYSKEYDOWN as usize => Some(KeyboardEvent::Key {
            time: 0,
            key: scan_code,
            state: 1,
        }),
        WPARAM(p) if p == WM_SYSKEYUP as usize => Some(KeyboardEvent::Key {
            time: 0,
            key: scan_code,
            state: 0,
        }),
        _ => None,
    }
}

///
/// clamp point to display bounds
///
/// # Arguments
///
/// * `prev_point`: coordinates, the cursor was before entering, within bounds of a display
/// * `entry_point`: point to clamp
///
/// returns: (i32, i32), the corrected entry point
///
fn clamp_to_display_bounds(prev_point: (i32, i32), point: (i32, i32)) -> (i32, i32) {
    /* find display where movement came from */
    let display_regions = unsafe { get_display_regions() };
    let display = display_regions
        .iter()
        .find(|&d| is_within_dp_region(prev_point, d))
        .unwrap();

    /* clamp to bounds (inclusive) */
    let (x, y) = point;
    let (min_x, max_x) = (display.left, display.right - 1);
    let (min_y, max_y) = (display.top, display.bottom - 1);
    (x.clamp(min_x, max_x), y.clamp(min_y, max_y))
}

unsafe fn send_blocking(event: CaptureEvent) {
    if let Some(active) = ACTIVE_CLIENT {
        block_on(async move {
            let _ = EVENT_TX.as_ref().unwrap().send((active, event)).await;
        });
    }
}

unsafe fn check_client_activation(wparam: WPARAM, lparam: LPARAM) -> bool {
    if wparam.0 != WM_MOUSEMOVE as usize {
        return ACTIVE_CLIENT.is_some();
    }
    let mouse_low_level: MSLLHOOKSTRUCT = *(lparam.0 as *const MSLLHOOKSTRUCT);
    static mut PREV_POS: Option<(i32, i32)> = None;
    let curr_pos = (mouse_low_level.pt.x, mouse_low_level.pt.y);
    let prev_pos = PREV_POS.unwrap_or(curr_pos);
    PREV_POS.replace(curr_pos);

    /* next event is the first actual event */
    let ret = ACTIVE_CLIENT.is_some();

    /* client already active, no need to check */
    if ACTIVE_CLIENT.is_some() {
        return ret;
    }

    /* check if a client was activated */
    let Some(pos) = entered_barrier(prev_pos, curr_pos, get_display_regions()) else {
        return ret;
    };

    /* check if a client is registered for the barrier */
    if !CLIENTS.contains(&pos) {
        return ret;
    }

    /* update active client and entry point */
    ACTIVE_CLIENT.replace(pos);
    ENTRY_POINT = clamp_to_display_bounds(prev_pos, curr_pos);

    /* notify main thread */
    log::debug!("ENTERED @ {prev_pos:?} -> {curr_pos:?}");
    send_blocking(CaptureEvent::Begin);

    ret
}

unsafe extern "system" fn mouse_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let active = check_client_activation(wparam, lparam);

    /* no client was active */
    if !active {
        return CallNextHookEx(HHOOK::default(), ncode, wparam, lparam);
    }

    /* get active client if any */
    let Some(pos) = ACTIVE_CLIENT else {
        return LRESULT(1);
    };

    /* convert to lan-mouse event */
    let Some(pointer_event) = to_mouse_event(wparam, lparam) else {
        return LRESULT(1);
    };
    let event = (pos, CaptureEvent::Input(Event::Pointer(pointer_event)));

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
    let event = (client, CaptureEvent::Input(Event::Keyboard(key_event)));

    if let Err(e) = EVENT_TX.as_ref().unwrap().try_send(event) {
        log::warn!("e: {e}");
    }

    /* don't pass event to applications */
    LRESULT(1)
}

unsafe extern "system" fn window_proc(
    _hwnd: HWND,
    uint: u32,
    _wparam: WPARAM,
    _lparam: LPARAM,
) -> LRESULT {
    match uint {
        x if x == WM_DISPLAYCHANGE => {
            log::debug!("display resolution changed");
            DISPLAY_RESOLUTION_CHANGED = true;
        }
        _ => {}
    }
    LRESULT(1)
}

fn enumerate_displays() -> Vec<RECT> {
    unsafe {
        let mut display_rects = vec![];
        let mut devices = vec![];
        for i in 0.. {
            let mut device: DISPLAY_DEVICEW = std::mem::zeroed();
            device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            let ret = EnumDisplayDevicesW(None, i, &mut device, EDD_GET_DEVICE_INTERFACE_NAME);
            if ret == FALSE {
                break;
            }
            if device.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP != 0 {
                devices.push(device.DeviceName);
            }
        }
        for device in devices {
            let mut dev_mode: DEVMODEW = std::mem::zeroed();
            dev_mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
            let ret = EnumDisplaySettingsW(
                PCWSTR::from_raw(&device as *const _),
                ENUM_CURRENT_SETTINGS,
                &mut dev_mode,
            );
            if ret == FALSE {
                log::warn!("no display mode");
            }

            let pos = dev_mode.Anonymous1.Anonymous2.dmPosition;
            let (x, y) = (pos.x, pos.y);
            let (width, height) = (dev_mode.dmPelsWidth, dev_mode.dmPelsHeight);

            display_rects.push(RECT {
                left: x,
                right: x + width as i32,
                top: y,
                bottom: y + height as i32,
            });
        }
        display_rects
    }
}

static mut DISPLAY_RESOLUTION_CHANGED: bool = true;

unsafe fn get_display_regions() -> &'static Vec<RECT> {
    static mut DISPLAYS: Vec<RECT> = vec![];
    if DISPLAY_RESOLUTION_CHANGED {
        DISPLAYS = enumerate_displays();
        DISPLAY_RESOLUTION_CHANGED = false;
        log::debug!("displays: {DISPLAYS:?}");
    }
    &*addr_of!(DISPLAYS)
}

fn is_within_dp_region(point: (i32, i32), display: &RECT) -> bool {
    [
        Position::Left,
        Position::Right,
        Position::Top,
        Position::Bottom,
    ]
    .iter()
    .all(|&pos| is_within_dp_boundary(point, display, pos))
}
fn is_within_dp_boundary(point: (i32, i32), display: &RECT, pos: Position) -> bool {
    let (x, y) = point;
    match pos {
        Position::Left => display.left <= x,
        Position::Right => display.right > x,
        Position::Top => display.top <= y,
        Position::Bottom => display.bottom > y,
    }
}

/// returns whether the given position is within the display bounds with respect to the given
/// barrier position
///
/// # Arguments
///
/// * `x`:
/// * `y`:
/// * `displays`:
/// * `pos`:
///
/// returns: bool
///
fn in_bounds(point: (i32, i32), displays: &[RECT], pos: Position) -> bool {
    displays
        .iter()
        .any(|d| is_within_dp_boundary(point, d, pos))
}

fn in_display_region(point: (i32, i32), displays: &[RECT]) -> bool {
    displays.iter().any(|d| is_within_dp_region(point, d))
}

fn moved_across_boundary(
    prev_pos: (i32, i32),
    curr_pos: (i32, i32),
    displays: &[RECT],
    pos: Position,
) -> bool {
    /* was within bounds, but is not anymore */
    in_display_region(prev_pos, displays) && !in_bounds(curr_pos, displays, pos)
}

fn entered_barrier(
    prev_pos: (i32, i32),
    curr_pos: (i32, i32),
    displays: &[RECT],
) -> Option<Position> {
    [
        Position::Left,
        Position::Right,
        Position::Top,
        Position::Bottom,
    ]
    .into_iter()
    .find(|&pos| moved_across_boundary(prev_pos, curr_pos, displays, pos))
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

static WINDOW_CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);

fn message_thread(ready_tx: mpsc::Sender<()>) {
    unsafe {
        set_event_tid(GetCurrentThreadId());
        ready_tx.send(()).expect("channel closed");
        let mouse_proc: HOOKPROC = Some(mouse_proc);
        let kybrd_proc: HOOKPROC = Some(kybrd_proc);
        let window_proc: WNDPROC = Some(window_proc);

        /* register hooks */
        let _ = SetWindowsHookExW(WH_MOUSE_LL, mouse_proc, HINSTANCE::default(), 0).unwrap();
        let _ = SetWindowsHookExW(WH_KEYBOARD_LL, kybrd_proc, HINSTANCE::default(), 0).unwrap();

        let instance = GetModuleHandleW(None).unwrap();
        let window_class: WNDCLASSW = WNDCLASSW {
            lpfnWndProc: window_proc,
            hInstance: instance.into(),
            lpszClassName: w!("lan-mouse-message-window-class"),
            ..Default::default()
        };

        if WINDOW_CLASS_REGISTERED
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            /* register window class if not yet done so */
            let ret = RegisterClassW(&window_class);
            if ret == 0 {
                panic!("RegisterClassW");
            }
        }

        /* window is used ro receive WM_DISPLAYCHANGE messages */
        CreateWindowExW(
            Default::default(),
            w!("lan-mouse-message-window-class"),
            w!("lan-mouse-msg-window"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            HWND::default(),
            HMENU::default(),
            instance,
            None,
        )
        .expect("CreateWindowExW");

        /* run message loop */
        loop {
            // mouse / keybrd proc do not actually return a message
            let Some(msg) = get_msg() else {
                break;
            };
            if msg.hwnd.0.is_null() {
                /* messages sent via PostThreadMessage */
                match msg.wParam.0 {
                    x if x == EventType::Exit as usize => break,
                    x if x == EventType::Release as usize => {
                        ACTIVE_CLIENT.take();
                    }
                    x if x == EventType::Request as usize => {
                        let requests = {
                            let mut res = vec![];
                            let mut requests = REQUEST_BUFFER.lock().unwrap();
                            for request in requests.drain(..) {
                                res.push(request);
                            }
                            res
                        };

                        for request in requests {
                            update_clients(request)
                        }
                    }
                    _ => {}
                }
            } else {
                /* other messages for window_procs */
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

fn update_clients(request: Request) {
    match request {
        Request::Create(pos) => {
            unsafe { CLIENTS.insert(pos) };
        }
        Request::Destroy(pos) => unsafe {
            if let Some(active_pos) = ACTIVE_CLIENT {
                if pos == active_pos {
                    let _ = ACTIVE_CLIENT.take();
                }
            }
            CLIENTS.remove(&pos);
        },
    }
}

impl WindowsInputCapture {
    pub(crate) fn new() -> Self {
        unsafe {
            let (tx, rx) = channel(10);
            EVENT_TX.replace(tx);
            let (ready_tx, ready_rx) = mpsc::channel();
            let msg_thread = Some(thread::spawn(|| message_thread(ready_tx)));
            /* wait for thread to set its id */
            ready_rx.recv().expect("channel closed");
            Self {
                msg_thread,
                event_rx: rx,
            }
        }
    }
}

impl Stream for WindowsInputCapture {
    type Item = Result<(Position, CaptureEvent), CaptureError>;
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
