#![allow(clashing_extern_declarations)]

use std::{
    cell::RefCell,
    ffi::{CStr, CString, c_char, c_double, c_uint, c_void},
    sync::OnceLock,
};

use adw::prelude::*;
use gtk::{gio, glib};

use crate::window::Window;

type Id = *mut c_void;
type Class = *mut c_void;
type Sel = *mut c_void;
type Bool = i8;

struct StatusItem {
    app: glib::WeakRef<adw::Application>,
    window: glib::WeakRef<Window>,
    _hold: gio::ApplicationHoldGuard,
    _delegate: Id,
    _status_item: Id,
}

thread_local! {
    static STATUS_ITEM: RefCell<Option<StatusItem>> = const { RefCell::new(None) };
}

pub fn setup(app: &adw::Application, window: &Window) {
    log::debug!("macos_status_item::setup entered");
    STATUS_ITEM.with(|item| {
        let already_initialized = item.borrow().is_some();
        if already_initialized {
            let mut cell = item.borrow_mut();
            if let Some(existing) = cell.as_mut() {
                existing.app.set(Some(app));
                existing.window.set(Some(window));
            }
            return;
        }

        unsafe {
            let hold = app.hold();

            let ns_app = msg_send_id(class(c"NSApplication"), sel(c"sharedApplication"));
            assert!(
                !ns_app.is_null(),
                "NSApplication sharedApplication returned null"
            );
            msg_send_bool_usize(ns_app, sel(c"setActivationPolicy:"), 1);

            let delegate = new_delegate();
            let menu = menu(&[
                menu_item(c"Open Lan Mouse", c"showLanMouse:"),
                separator_item(),
                menu_item(c"Quit Lan Mouse", c"quitLanMouse:"),
            ]);

            let status_bar = msg_send_id(class(c"NSStatusBar"), sel(c"systemStatusBar"));
            assert!(
                !status_bar.is_null(),
                "NSStatusBar systemStatusBar returned null"
            );
            let status_item = msg_send_id_f64(status_bar, sel(c"statusItemWithLength:"), -1.0);
            assert!(!status_item.is_null(), "statusItemWithLength returned null");
            // Retain so the status item survives autorelease pool drain.
            let status_item = msg_send_id(status_item, sel(c"retain"));

            let button = msg_send_id(status_item, sel(c"button"));
            assert!(!button.is_null(), "NSStatusItem.button was null");
            set_button_image(button);
            msg_send_void_id(button, sel(c"setToolTip:"), nsstring(c"Lan Mouse"));
            msg_send_void_id(status_item, sel(c"setMenu:"), menu);

            for item in menu_items(menu) {
                msg_send_void_id(item, sel(c"setTarget:"), delegate);
            }

            install_reopen_handler(delegate);

            log::debug!("macos_status_item ready at {status_item:p}");

            item.replace(Some(StatusItem {
                app: app.downgrade(),
                window: window.downgrade(),
                _hold: hold,
                _delegate: delegate,
                _status_item: status_item,
            }));
        }
    });
}

// Prefer a pre-rendered template PNG (black silhouette with alpha) so macOS
// auto-tints the glyph to match the menu bar in light and dark modes.
// Falls back to the full-color icns, then to "LM" text.
unsafe fn set_button_image(button: Id) {
    if let Some(image) = load_menubar_template() {
        msg_send_void_bool(image, sel(c"setTemplate:"), 1);
        msg_send_void_id(button, sel(c"setImage:"), image);
        return;
    }
    if let Some(image) = load_app_icon() {
        msg_send_void_id(button, sel(c"setImage:"), image);
        return;
    }
    log::warn!("no menu bar image available; falling back to text title");
    msg_send_void_id(button, sel(c"setTitle:"), nsstring(c"LM"));
}

unsafe fn load_menubar_template() -> Option<Id> {
    load_resource_image(c"menubar-template", c"png", MENUBAR_ICON_SIZE)
}

unsafe fn load_app_icon() -> Option<Id> {
    load_resource_image(c"icon", c"icns", MENUBAR_ICON_SIZE)
}

unsafe fn load_resource_image(name: &CStr, ext: &CStr, size_pt: c_double) -> Option<Id> {
    let bundle = msg_send_id(class(c"NSBundle"), sel(c"mainBundle"));
    if bundle.is_null() {
        return None;
    }
    let path = msg_send_id_id_id(
        bundle,
        sel(c"pathForResource:ofType:"),
        nsstring(name),
        nsstring(ext),
    );
    if path.is_null() {
        return None;
    }
    let image = msg_send_id_id(
        msg_send_id(class(c"NSImage"), sel(c"alloc")),
        sel(c"initWithContentsOfFile:"),
        path,
    );
    if image.is_null() {
        return None;
    }
    // Render at menu bar height; 22pt is the full status bar icon height.
    msg_send_void_size(image, sel(c"setSize:"), size_pt, size_pt);
    Some(image)
}

const MENUBAR_ICON_SIZE: c_double = 22.0;

unsafe fn menu(items: &[Id]) -> Id {
    let menu = msg_send_id(msg_send_id(class(c"NSMenu"), sel(c"alloc")), sel(c"init"));
    for item in items {
        msg_send_void_id(menu, sel(c"addItem:"), *item);
    }
    menu
}

unsafe fn menu_item(title: &CStr, action: &CStr) -> Id {
    msg_send_id_id_sel_id(
        msg_send_id(class(c"NSMenuItem"), sel(c"alloc")),
        sel(c"initWithTitle:action:keyEquivalent:"),
        nsstring(title),
        sel(action),
        nsstring(c""),
    )
}

unsafe fn separator_item() -> Id {
    msg_send_id(class(c"NSMenuItem"), sel(c"separatorItem"))
}

unsafe fn menu_items(menu: Id) -> Vec<Id> {
    let count = msg_send_usize(menu, sel(c"numberOfItems"));
    (0..count)
        .map(|idx| msg_send_id_usize(menu, sel(c"itemAtIndex:"), idx))
        .collect()
}

unsafe fn new_delegate() -> Id {
    let class = delegate_class();
    msg_send_id(msg_send_id(class, sel(c"alloc")), sel(c"init"))
}

fn delegate_class() -> Class {
    static CLASS: OnceLock<usize> = OnceLock::new();

    *CLASS.get_or_init(|| unsafe {
        let superclass = class(c"NSObject");
        let class_name = CString::new("LanMouseStatusItemDelegate").unwrap();
        let class = objc_allocateClassPair(superclass, class_name.as_ptr(), 0);
        assert!(!class.is_null(), "failed to allocate status item delegate");

        class_addMethod(
            class,
            sel(c"showLanMouse:"),
            show_lan_mouse as *const c_void,
            c"v@:@".as_ptr(),
        );
        class_addMethod(
            class,
            sel(c"quitLanMouse:"),
            quit_lan_mouse as *const c_void,
            c"v@:@".as_ptr(),
        );
        // kAEReopenApplication handler — fires when the user re-launches
        // the .app while it's already running (Finder, `open`, Dock).
        class_addMethod(
            class,
            sel(c"handleReopenEvent:withReplyEvent:"),
            handle_reopen_event as *const c_void,
            c"v@:@@".as_ptr(),
        );
        objc_registerClassPair(class);
        class as usize
    }) as Class
}

extern "C" fn show_lan_mouse(_this: Id, _cmd: Sel, _sender: Id) {
    present_window();
}

extern "C" fn handle_reopen_event(_this: Id, _cmd: Sel, _event: Id, _reply: Id) {
    log::debug!("kAEReopenApplication received — presenting main window");
    present_window();
}

fn present_window() {
    STATUS_ITEM.with(|item| {
        let item = item.borrow();
        let Some(item) = item.as_ref() else {
            return;
        };
        if let Some(window) = item.window.upgrade() {
            window.present();
        }

        unsafe {
            let ns_app = msg_send_id(class(c"NSApplication"), sel(c"sharedApplication"));
            msg_send_void_bool(ns_app, sel(c"activateIgnoringOtherApps:"), 1);
        }
    });
}

/// macOS NSApplicationActivationPolicy values.
/// - `Regular` (0): standard app, Dock icon visible.
/// - `Accessory` (1): no Dock icon, can have menu-bar item.
pub(crate) const ACTIVATION_POLICY_REGULAR: usize = 0;
pub(crate) const ACTIVATION_POLICY_ACCESSORY: usize = 1;

/// Set NSApp's activation policy. Toggle between Regular (Dock
/// icon shown — used while the main window is visible) and
/// Accessory (no Dock icon — used while only the menu bar is
/// visible).
pub(crate) fn set_activation_policy(policy: usize) {
    unsafe {
        let ns_app = msg_send_id(class(c"NSApplication"), sel(c"sharedApplication"));
        msg_send_bool_usize(ns_app, sel(c"setActivationPolicy:"), policy);
    }
}

// Register the status-item delegate as the handler for the
// kAEReopenApplication Apple Event ('aevt'/'rapp'). NSApplication
// installs a default handler at -finishLaunching that just delegates to
// applicationShouldHandleReopen:hasVisibleWindows: — which is a no-op
// here because GApplication owns NSApp's delegate. Replacing it lets us
// re-present the window when the user double-clicks the .app while
// we're already running.
unsafe fn install_reopen_handler(delegate: Id) {
    const K_CORE_EVENT_CLASS: c_uint = 0x6165_7674; // 'aevt'
    const K_AE_REOPEN_APPLICATION: c_uint = 0x7261_7070; // 'rapp'

    let manager = msg_send_id(
        class(c"NSAppleEventManager"),
        sel(c"sharedAppleEventManager"),
    );
    if manager.is_null() {
        log::warn!("NSAppleEventManager unavailable; re-launch will not re-open window");
        return;
    }
    msg_send_void_id_sel_u32_u32(
        manager,
        sel(c"setEventHandler:andSelector:forEventClass:andEventID:"),
        delegate,
        sel(c"handleReopenEvent:withReplyEvent:"),
        K_CORE_EVENT_CLASS,
        K_AE_REOPEN_APPLICATION,
    );
}

extern "C" fn quit_lan_mouse(_this: Id, _cmd: Sel, _sender: Id) {
    STATUS_ITEM.with(|item| {
        if let Some(app) = item.borrow().as_ref().and_then(|item| item.app.upgrade()) {
            app.quit();
        }
    });
}

unsafe fn class(name: &CStr) -> Class {
    let class = objc_getClass(name.as_ptr());
    assert!(!class.is_null(), "missing Objective-C class {name:?}");
    class
}

unsafe fn sel(name: &CStr) -> Sel {
    sel_registerName(name.as_ptr())
}

unsafe fn nsstring(value: &CStr) -> Id {
    msg_send_id_ptr(
        class(c"NSString"),
        sel(c"stringWithUTF8String:"),
        value.as_ptr(),
    )
}

#[link(name = "objc")]
extern "C" {
    fn objc_allocateClassPair(superclass: Class, name: *const c_char, extra_bytes: usize) -> Class;
    fn objc_getClass(name: *const c_char) -> Class;
    fn objc_registerClassPair(class: Class);
    fn sel_registerName(name: *const c_char) -> Sel;
    fn class_addMethod(class: Class, name: Sel, imp: *const c_void, types: *const c_char) -> Bool;
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "objc")]
extern "C" {
    #[link_name = "objc_msgSend"]
    fn msg_send_id(receiver: Id, selector: Sel) -> Id;
    #[link_name = "objc_msgSend"]
    fn msg_send_id_f64(receiver: Id, selector: Sel, value: c_double) -> Id;
    #[link_name = "objc_msgSend"]
    fn msg_send_id_id_sel_id(receiver: Id, selector: Sel, a: Id, b: Sel, c: Id) -> Id;
    #[link_name = "objc_msgSend"]
    fn msg_send_id_id_id(receiver: Id, selector: Sel, a: Id, b: Id) -> Id;
    #[link_name = "objc_msgSend"]
    fn msg_send_id_id(receiver: Id, selector: Sel, a: Id) -> Id;
    #[link_name = "objc_msgSend"]
    fn msg_send_void_size(receiver: Id, selector: Sel, width: c_double, height: c_double);
    #[link_name = "objc_msgSend"]
    fn msg_send_id_ptr(receiver: Id, selector: Sel, value: *const c_char) -> Id;
    #[link_name = "objc_msgSend"]
    fn msg_send_id_usize(receiver: Id, selector: Sel, value: usize) -> Id;
    #[link_name = "objc_msgSend"]
    fn msg_send_usize(receiver: Id, selector: Sel) -> usize;
    #[link_name = "objc_msgSend"]
    fn msg_send_void_bool(receiver: Id, selector: Sel, value: Bool);
    #[link_name = "objc_msgSend"]
    fn msg_send_void_id(receiver: Id, selector: Sel, value: Id);
    #[link_name = "objc_msgSend"]
    fn msg_send_bool_usize(receiver: Id, selector: Sel, value: usize) -> Bool;
    #[link_name = "objc_msgSend"]
    fn msg_send_void_id_sel_u32_u32(
        receiver: Id,
        selector: Sel,
        a: Id,
        b: Sel,
        c: c_uint,
        d: c_uint,
    );
}
