use std::cell::RefCell;

use adw::subclass::prelude::*;
use adw::{ActionRow, ComboRow, prelude::*};
use glib::{Binding, subclass::InitializingObject};
use gtk::glib::subclass::Signal;
use gtk::glib::{SignalHandlerId, clone};
use gtk::{Button, CompositeTemplate, Entry, Switch, glib};
use lan_mouse_ipc::Position;
use std::sync::OnceLock;

use crate::client_object::ClientObject;

#[derive(CompositeTemplate, Default)]
#[template(resource = "/de/feschber/LanMouse/client_row.ui")]
pub struct ClientRow {
    #[template_child]
    pub enable_switch: TemplateChild<gtk::Switch>,
    #[template_child]
    pub dns_button: TemplateChild<gtk::Button>,
    #[template_child]
    pub hostname: TemplateChild<gtk::Entry>,
    #[template_child]
    pub port: TemplateChild<gtk::Entry>,
    #[template_child]
    pub position: TemplateChild<ComboRow>,
    #[template_child]
    pub delete_row: TemplateChild<ActionRow>,
    #[template_child]
    pub delete_button: TemplateChild<gtk::Button>,
    #[template_child]
    pub dns_loading_indicator: TemplateChild<gtk::Spinner>,
    pub bindings: RefCell<Vec<Binding>>,
    hostname_change_handler: RefCell<Option<SignalHandlerId>>,
    port_change_handler: RefCell<Option<SignalHandlerId>>,
    position_change_handler: RefCell<Option<SignalHandlerId>>,
    set_state_handler: RefCell<Option<SignalHandlerId>>,
    pub client_object: RefCell<Option<ClientObject>>,
}

#[glib::object_subclass]
impl ObjectSubclass for ClientRow {
    // `NAME` needs to match `class` attribute of template
    const NAME: &'static str = "ClientRow";
    const ABSTRACT: bool = false;

    type Type = super::ClientRow;
    type ParentType = adw::ExpanderRow;

    fn class_init(klass: &mut Self::Class) {
        klass.bind_template();
        klass.bind_template_callbacks();
    }

    fn instance_init(obj: &InitializingObject<Self>) {
        obj.init_template();
    }
}

impl ObjectImpl for ClientRow {
    fn constructed(&self) {
        self.parent_constructed();
        self.delete_button.connect_clicked(clone!(
            #[weak(rename_to = row)]
            self,
            move |button| {
                row.handle_client_delete(button);
            }
        ));
        let handler = self.hostname.connect_changed(clone!(
            #[weak(rename_to = row)]
            self,
            move |entry| {
                row.handle_hostname_changed(entry);
            }
        ));
        self.hostname_change_handler.replace(Some(handler));
        let handler = self.port.connect_changed(clone!(
            #[weak(rename_to = row)]
            self,
            move |entry| {
                row.handle_port_changed(entry);
            }
        ));
        self.port_change_handler.replace(Some(handler));
        let handler = self.position.connect_selected_notify(clone!(
            #[weak(rename_to = row)]
            self,
            move |position| {
                row.handle_position_changed(position);
            }
        ));
        self.position_change_handler.replace(Some(handler));
        let handler = self.enable_switch.connect_state_set(clone!(
            #[weak(rename_to = row)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |switch, state| {
                row.handle_activate_switch(state, switch);
                glib::Propagation::Proceed
            }
        ));
        self.set_state_handler.replace(Some(handler));
    }

    fn signals() -> &'static [glib::subclass::Signal] {
        static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
        SIGNALS.get_or_init(|| {
            vec![
                Signal::builder("request-activate")
                    .param_types([bool::static_type()])
                    .build(),
                Signal::builder("request-delete").build(),
                Signal::builder("request-dns").build(),
                Signal::builder("request-hostname-change")
                    .param_types([String::static_type()])
                    .build(),
                Signal::builder("request-port-change")
                    .param_types([u32::static_type()])
                    .build(),
                Signal::builder("request-position-change")
                    .param_types([u32::static_type()])
                    .build(),
            ]
        })
    }
}

#[gtk::template_callbacks]
impl ClientRow {
    #[template_callback]
    fn handle_activate_switch(&self, state: bool, _switch: &Switch) -> bool {
        self.obj().emit_by_name::<()>("request-activate", &[&state]);
        true // dont run default handler
    }

    #[template_callback]
    fn handle_request_dns(&self, _: &Button) {
        self.obj().emit_by_name::<()>("request-dns", &[]);
    }

    #[template_callback]
    fn handle_client_delete(&self, _button: &Button) {
        self.obj().emit_by_name::<()>("request-delete", &[]);
    }

    fn handle_port_changed(&self, port_entry: &Entry) {
        if let Ok(port) = port_entry.text().parse::<u16>() {
            self.obj()
                .emit_by_name::<()>("request-port-change", &[&(port as u32)]);
        }
    }

    fn handle_hostname_changed(&self, hostname_entry: &Entry) {
        self.obj()
            .emit_by_name::<()>("request-hostname-change", &[&hostname_entry.text()]);
    }

    fn handle_position_changed(&self, position: &ComboRow) {
        self.obj()
            .emit_by_name("request-position-change", &[&position.selected()])
    }

    pub(super) fn set_hostname(&self, hostname: Option<String>) {
        let position = self.hostname.position();
        let handler = self.hostname_change_handler.borrow();
        let handler = handler.as_ref().expect("signal handler");
        self.hostname.block_signal(handler);
        self.client_object
            .borrow_mut()
            .as_mut()
            .expect("client object")
            .set_property("hostname", hostname);
        self.hostname.unblock_signal(handler);
        self.hostname.set_position(position);
    }

    pub(super) fn set_port(&self, port: u16) {
        let position = self.port.position();
        let handler = self.port_change_handler.borrow();
        let handler = handler.as_ref().expect("signal handler");
        self.port.block_signal(handler);
        self.client_object
            .borrow_mut()
            .as_mut()
            .expect("client object")
            .set_port(port as u32);
        self.port.unblock_signal(handler);
        self.port.set_position(position);
    }

    pub(super) fn set_pos(&self, pos: Position) {
        let handler = self.position_change_handler.borrow();
        let handler = handler.as_ref().expect("signal handler");
        self.position.block_signal(handler);
        self.client_object
            .borrow_mut()
            .as_mut()
            .expect("client object")
            .set_position(pos.to_string());
        self.position.unblock_signal(handler);
    }

    pub(super) fn set_active(&self, active: bool) {
        let handler = self.set_state_handler.borrow();
        let handler = handler.as_ref().expect("signal handler");
        self.enable_switch.block_signal(handler);
        self.client_object
            .borrow_mut()
            .as_mut()
            .expect("client object")
            .set_active(active);
        self.enable_switch.unblock_signal(handler);
    }

    pub(super) fn set_dns_state(&self, resolved: bool) {
        if resolved {
            self.dns_button.set_css_classes(&["success"])
        } else {
            self.dns_button.set_css_classes(&["warning"])
        }
    }
}

impl WidgetImpl for ClientRow {}
impl BoxImpl for ClientRow {}
impl ListBoxRowImpl for ClientRow {}
impl PreferencesRowImpl for ClientRow {}
impl ExpanderRowImpl for ClientRow {}
