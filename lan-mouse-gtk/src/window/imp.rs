use std::cell::{Cell, RefCell};

use adw::subclass::prelude::*;
use adw::{ActionRow, PreferencesGroup, ToastOverlay, prelude::*};
use glib::subclass::InitializingObject;
use gtk::glib::clone;
use gtk::{
    Button, CompositeTemplate, Entry, EventControllerScroll, EventControllerScrollFlags, Image,
    Label, ListBox, PropagationPhase, Scale, ScrolledWindow, Switch, gdk, gio, glib,
};

use lan_mouse_ipc::{DEFAULT_PORT, FrontendRequestWriter};

use crate::authorization_window::AuthorizationWindow;

#[derive(CompositeTemplate, Default)]
#[template(resource = "/de/feschber/LanMouse/window.ui")]
pub struct Window {
    #[template_child]
    pub authorized_placeholder: TemplateChild<ActionRow>,
    #[template_child]
    pub fingerprint_row: TemplateChild<ActionRow>,
    #[template_child]
    pub port_edit_apply: TemplateChild<Button>,
    #[template_child]
    pub port_edit_cancel: TemplateChild<Button>,
    #[template_child]
    pub client_list: TemplateChild<ListBox>,
    #[template_child]
    pub client_placeholder: TemplateChild<ActionRow>,
    #[template_child]
    pub port_entry: TemplateChild<Entry>,
    #[template_child]
    pub hostname_copy_icon: TemplateChild<Image>,
    #[template_child]
    pub hostname_label: TemplateChild<Label>,
    #[template_child]
    pub toast_overlay: TemplateChild<ToastOverlay>,
    #[template_child]
    pub capture_emulation_group: TemplateChild<PreferencesGroup>,
    #[template_child]
    pub capture_status_row: TemplateChild<ActionRow>,
    #[template_child]
    pub emulation_status_row: TemplateChild<ActionRow>,
    #[template_child]
    pub input_emulation_button: TemplateChild<Button>,
    #[template_child]
    pub input_capture_button: TemplateChild<Button>,
    #[template_child]
    pub authorized_list: TemplateChild<ListBox>,
    #[template_child]
    pub release_threshold_row: TemplateChild<ActionRow>,
    #[template_child]
    pub release_threshold_scale: TemplateChild<Scale>,
    #[template_child]
    pub release_threshold_value: TemplateChild<Label>,
    #[template_child]
    pub mdns_discovery_row: TemplateChild<ActionRow>,
    #[template_child]
    pub mdns_discovery_switch: TemplateChild<Switch>,
    pub clients: RefCell<Option<gio::ListStore>>,
    pub authorized: RefCell<Option<gio::ListStore>>,
    pub frontend_request_writer: RefCell<Option<FrontendRequestWriter>>,
    pub port: Cell<u16>,
    pub capture_active: Cell<bool>,
    pub emulation_active: Cell<bool>,
    pub authorization_window: RefCell<Option<AuthorizationWindow>>,
    /// Connected handler for the auto-release-threshold scale's
    /// value-changed signal, so we can block it when programmatically
    /// updating the slider in response to a Sync event.
    pub release_threshold_handler: RefCell<Option<glib::SignalHandlerId>>,
    /// Connected handler for the mDNS-discovery switch's state-set
    /// signal, blocked while the daemon is pushing the initial value
    /// via Sync.
    pub mdns_discovery_handler: RefCell<Option<glib::SignalHandlerId>>,
}

#[glib::object_subclass]
impl ObjectSubclass for Window {
    // `NAME` needs to match `class` attribute of template
    const NAME: &'static str = "LanMouseWindow";
    const ABSTRACT: bool = false;

    type Type = super::Window;
    type ParentType = adw::ApplicationWindow;

    fn class_init(klass: &mut Self::Class) {
        klass.bind_template();
        klass.bind_template_callbacks();
    }

    fn instance_init(obj: &InitializingObject<Self>) {
        obj.init_template();
    }
}

#[gtk::template_callbacks]
impl Window {
    #[template_callback]
    fn handle_add_client_pressed(&self, _button: &Button) {
        self.obj().request_client_create();
    }

    #[template_callback]
    fn handle_copy_hostname(&self, _: &Button) {
        if let Ok(hostname) = hostname::get() {
            let display = gdk::Display::default().unwrap();
            let clipboard = display.clipboard();
            clipboard.set_text(hostname.to_str().expect("hostname: invalid utf8"));
            let icon = self.hostname_copy_icon.clone();
            icon.set_icon_name(Some("emblem-ok-symbolic"));
            icon.set_css_classes(&["success"]);
            glib::spawn_future_local(clone!(
                #[weak]
                icon,
                async move {
                    glib::timeout_future_seconds(1).await;
                    icon.set_icon_name(Some("edit-copy-symbolic"));
                    icon.set_css_classes(&[]);
                }
            ));
        }
    }

    #[template_callback]
    fn handle_copy_fingerprint(&self, button: &Button) {
        let fingerprint: String = self.fingerprint_row.property("subtitle");
        let display = gdk::Display::default().unwrap();
        let clipboard = display.clipboard();
        clipboard.set_text(&fingerprint);
        button.set_icon_name("emblem-ok-symbolic");
        button.set_css_classes(&["success"]);
        glib::spawn_future_local(clone!(
            #[weak]
            button,
            async move {
                glib::timeout_future_seconds(1).await;
                button.set_icon_name("edit-copy-symbolic");
                button.set_css_classes(&[]);
            }
        ));
    }

    #[template_callback]
    fn handle_port_changed(&self, _entry: &Entry) {
        self.port_edit_apply.set_visible(true);
        self.port_edit_cancel.set_visible(true);
    }

    #[template_callback]
    fn handle_port_edit_apply(&self) {
        self.obj().request_port_change();
    }

    #[template_callback]
    fn handle_port_edit_cancel(&self) {
        log::debug!("cancel port edit");
        self.port_entry
            .set_text(self.port.get().to_string().as_str());
        self.port_edit_apply.set_visible(false);
        self.port_edit_cancel.set_visible(false);
    }

    #[template_callback]
    fn handle_emulation(&self) {
        // On macOS the emulation_status_row is hidden — capture_status_row
        // acts as the shared warning (see update_capture_emulation_status).
        // This handler still fires for the non-macOS platforms where the
        // emulation row is distinct.
        self.obj().request_emulation();
    }

    #[template_callback]
    fn handle_capture(&self) {
        #[cfg(target_os = "macos")]
        {
            use crate::macos_privacy;
            if macos_privacy::accessibility_granted() {
                // AX granted but the row is still visible => the daemon
                // subprocess bailed before AX was in place and needs a
                // fresh process. Quit + relaunch via Launch Services.
                log::info!("capture row clicked in relaunch-required state");
                macos_privacy::relaunch_bundle();
                if let Some(app) = self.obj().application() {
                    app.quit();
                }
                return;
            }
            log::info!("capture row clicked in AX-missing state, opening pane");
            macos_privacy::open_accessibility_settings();
        }
        self.obj().request_capture();
    }

    #[template_callback]
    fn handle_add_cert_fingerprint(&self, _button: &Button) {
        self.obj().open_fingerprint_dialog(None);
    }

    pub fn set_port(&self, port: u16) {
        self.port.set(port);
        if port == DEFAULT_PORT {
            self.port_entry.set_text("");
        } else {
            self.port_entry.set_text(format!("{port}").as_str());
        }
        self.port_edit_apply.set_visible(false);
        self.port_edit_cancel.set_visible(false);
    }
}

impl ObjectImpl for Window {
    fn constructed(&self) {
        if let Ok(hostname) = hostname::get() {
            self.hostname_label
                .set_text(hostname.to_str().expect("hostname: invalid utf8"));
        }
        self.parent_constructed();
        self.set_port(DEFAULT_PORT);
        let obj = self.obj();
        obj.setup_icon();
        obj.setup_clients();
        obj.setup_authorized();

        // Connect the auto-release threshold slider. Stash the handler
        // id so set_release_threshold() can block the signal when the
        // daemon-driven Sync sets the value programmatically.
        let scale = self.release_threshold_scale.clone();
        let handler_id = scale.connect_value_changed(clone!(
            #[weak(rename_to = window)]
            obj,
            move |scale| {
                let value = scale.value().round() as u32;
                let label = if value == 0 {
                    "disabled".to_string()
                } else {
                    format!("{value} px")
                };
                window.imp().release_threshold_value.set_label(&label);
                window.request_release_threshold(value);
            }
        ));
        self.release_threshold_handler.replace(Some(handler_id));

        // Pass scroll-wheel events on the threshold slider through to
        // the ancestor ScrolledWindow instead of letting GtkScale's
        // default handler treat them as increment / decrement (which
        // would drift the threshold any time the user scrolls past
        // the slider). Returning `Stop` from a capture-phase handler
        // suppresses the scale's own scroll-to-adjust handler, but
        // also stops propagation to the parent — so we additionally
        // bump the parent ScrolledWindow's vadjustment by hand to
        // mimic native scroll-passthrough.
        let scroll_forward = EventControllerScroll::new(
            EventControllerScrollFlags::VERTICAL | EventControllerScrollFlags::HORIZONTAL,
        );
        scroll_forward.set_propagation_phase(PropagationPhase::Capture);
        scroll_forward.connect_scroll(clone!(
            #[weak(rename_to = scale)]
            self.release_threshold_scale,
            #[upgrade_or]
            glib::Propagation::Stop,
            move |_, _dx, dy| {
                let mut walker = scale.parent();
                while let Some(w) = walker {
                    if let Some(scrolled) = w.downcast_ref::<ScrolledWindow>() {
                        let vadj = scrolled.vadjustment();
                        // step_increment is the "wheel tick" unit; if
                        // unset (rare), fall back to a sensible
                        // pixel default so a single tick still moves.
                        let step = if vadj.step_increment() > 0.0 {
                            vadj.step_increment()
                        } else {
                            40.0
                        };
                        let target = (vadj.value() + dy * step)
                            .clamp(vadj.lower(), vadj.upper() - vadj.page_size());
                        vadj.set_value(target);
                        break;
                    }
                    walker = w.parent();
                }
                glib::Propagation::Stop
            }
        ));
        self.release_threshold_scale.add_controller(scroll_forward);

        // mDNS-discovery switch — connect state-set, stash the handler
        // so the daemon's Sync push doesn't ricochet back.
        let mdns_switch = self.mdns_discovery_switch.clone();
        let mdns_handler = mdns_switch.connect_state_set(clone!(
            #[weak(rename_to = window)]
            obj,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, state| {
                window.request_mdns_discovery(state);
                glib::Propagation::Proceed
            }
        ));
        self.mdns_discovery_handler.replace(Some(mdns_handler));
    }
}

impl WidgetImpl for Window {}
impl WindowImpl for Window {}
impl ApplicationWindowImpl for Window {}
impl AdwApplicationWindowImpl for Window {}
