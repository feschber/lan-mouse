use std::cell::RefCell;
use std::sync::OnceLock;

use adw::ComboRow;
use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::subclass::InitializingObject;
use gtk::{
    Button, CompositeTemplate, Entry, EventControllerKey, Label, ListBox, gdk,
    glib::{self, Propagation, subclass::Signal},
    template_callbacks,
};
use lan_mouse_ipc::AppIdent;

#[derive(CompositeTemplate, Default)]
#[template(resource = "/de/feschber/LanMouse/clipboard_privacy_window.ui")]
pub struct ClipboardPrivacyWindow {
    #[template_child]
    pub entries_list: TemplateChild<ListBox>,
    #[template_child]
    pub empty_placeholder: TemplateChild<Label>,
    #[template_child]
    pub kind_row: TemplateChild<ComboRow>,
    #[template_child]
    pub value_entry: TemplateChild<Entry>,
    #[template_child]
    pub add_button: TemplateChild<Button>,
    /// Latest server-confirmed list. Used to render `entries_list`
    /// from scratch on every update (the list is small enough that
    /// rebuild-on-update is simpler than diffing).
    pub apps: RefCell<Vec<AppIdent>>,
}

#[glib::object_subclass]
impl ObjectSubclass for ClipboardPrivacyWindow {
    const NAME: &'static str = "ClipboardPrivacyWindow";
    const ABSTRACT: bool = false;

    type Type = super::ClipboardPrivacyWindow;
    type ParentType = adw::Window;

    fn class_init(klass: &mut Self::Class) {
        klass.bind_template();
        klass.bind_template_callbacks();
    }

    fn instance_init(obj: &InitializingObject<Self>) {
        obj.init_template();
    }
}

#[template_callbacks]
impl ClipboardPrivacyWindow {}

impl ObjectImpl for ClipboardPrivacyWindow {
    fn signals() -> &'static [Signal] {
        static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
        SIGNALS.get_or_init(|| {
            vec![
                // Emitted when the user clicks Add. Carries the
                // serialized AppIdent as `(kind: u32, value: String)`
                // so the GObject signal machinery doesn't need a
                // boxed type for the enum.
                Signal::builder("request-add")
                    .param_types([u32::static_type(), String::static_type()])
                    .build(),
                // Emitted when the user clicks the trash button on
                // an existing entry. Same encoding as above.
                Signal::builder("request-remove")
                    .param_types([u32::static_type(), String::static_type()])
                    .build(),
            ]
        })
    }

    fn constructed(&self) {
        self.parent_constructed();

        // Close on Escape — same affordance as the other modals.
        let obj = self.obj();
        let key = EventControllerKey::new();
        let weak_close = obj.downgrade();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::Escape {
                if let Some(w) = weak_close.upgrade() {
                    w.close();
                }
                Propagation::Stop
            } else {
                Propagation::Proceed
            }
        });
        obj.add_controller(key);

        let weak_self = obj.downgrade();
        self.add_button.connect_clicked(move |_| {
            if let Some(window) = weak_self.upgrade() {
                window.imp().handle_add_clicked();
            }
        });
    }
}

impl WidgetImpl for ClipboardPrivacyWindow {}
impl WindowImpl for ClipboardPrivacyWindow {}
impl AdwWindowImpl for ClipboardPrivacyWindow {}

impl ClipboardPrivacyWindow {
    fn handle_add_clicked(&self) {
        let value = self.value_entry.text().as_str().trim().to_owned();
        if value.is_empty() {
            return;
        }
        let kind = self.kind_row.selected();
        self.obj()
            .emit_by_name::<()>("request-add", &[&kind, &value]);
        // Clear the entry so the next addition starts fresh; the
        // server-driven `set_apps` callback will rebuild the list
        // once the daemon confirms the add.
        self.value_entry.set_text("");
    }

    pub(super) fn rebuild_list(&self) {
        // GtkListBox::remove_all isn't available in older Gtk4 — drain
        // children one by one. The list is small (single-digit
        // entries in practice), so the cost is negligible.
        while let Some(child) = self.entries_list.first_child() {
            self.entries_list.remove(&child);
        }
        let apps = self.apps.borrow();
        if apps.is_empty() {
            self.empty_placeholder.set_visible(true);
            self.entries_list.set_visible(false);
            return;
        }
        self.empty_placeholder.set_visible(false);
        self.entries_list.set_visible(true);
        for (idx, app) in apps.iter().enumerate() {
            let row = adw::ActionRow::builder().title(app.label()).build();
            let delete = Button::builder()
                .icon_name("user-trash-symbolic")
                .valign(gtk::Align::Center)
                .tooltip_text("Remove from suppression list")
                .build();
            delete.add_css_class("flat");
            let weak_self = self.obj().downgrade();
            let app_clone = app.clone();
            delete.connect_clicked(move |_| {
                let Some(window) = weak_self.upgrade() else {
                    return;
                };
                let (kind, value) = app_ident_to_pair(&app_clone);
                window
                    .emit_by_name::<()>("request-remove", &[&kind, &value]);
            });
            row.add_suffix(&delete);
            self.entries_list.append(&row);
            // Avoid an unused-warning when the apps list is one
            // element — `idx` is otherwise dead.
            let _ = idx;
        }
    }
}

/// Encode an [`AppIdent`] as a (kind: u32, value: String) tuple for
/// the GObject signal channel. Mirrors the dropdown order in
/// `clipboard_privacy_window.ui`.
pub fn app_ident_to_pair(app: &AppIdent) -> (u32, String) {
    match app {
        AppIdent::MacBundle(s) => (0, s.clone()),
        AppIdent::WindowsExe(s) => (1, s.clone()),
        AppIdent::LinuxX11(s) => (2, s.clone()),
        AppIdent::LinuxWayland(s) => (3, s.clone()),
    }
}

/// Inverse of [`app_ident_to_pair`].
pub fn pair_to_app_ident(kind: u32, value: String) -> AppIdent {
    match kind {
        0 => AppIdent::MacBundle(value),
        1 => AppIdent::WindowsExe(value),
        2 => AppIdent::LinuxX11(value),
        _ => AppIdent::LinuxWayland(value),
    }
}
