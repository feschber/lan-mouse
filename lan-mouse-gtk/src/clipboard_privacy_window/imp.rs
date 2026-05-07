use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use adw::ComboRow;
use adw::PreferencesGroup;
use adw::prelude::*;
use adw::subclass::prelude::*;
use gio::ListStore;
use glib::object::Cast;
use glib::subclass::InitializingObject;
use gtk::{
    Box as GtkBox, Button, CompositeTemplate, EventControllerKey, Image, Label, ListBox, ListItem,
    Orientation, SignalListItemFactory, gdk, gio,
    glib::{self, Propagation, subclass::Signal},
    template_callbacks,
};
use lan_mouse_ipc::{HostKind, RunningApp};

use crate::running_app_object::RunningAppObject;

/// Cached display name + icon texture for one identifier. Used to
/// render the suppressed-apps list with the same icon+name
/// treatment as the picker; the cache lets the rebuild reuse the
/// running-apps icons without re-decoding the PNG bytes per row.
pub struct AppMetadata {
    display_name: String,
    icon: Option<gdk::Texture>,
}

#[derive(CompositeTemplate, Default)]
#[template(resource = "/de/feschber/LanMouse/clipboard_privacy_window.ui")]
pub struct ClipboardPrivacyWindow {
    #[template_child]
    pub entries_list: TemplateChild<ListBox>,
    #[template_child]
    pub empty_placeholder: TemplateChild<Label>,
    #[template_child]
    pub add_group: TemplateChild<PreferencesGroup>,
    #[template_child]
    pub app_picker: TemplateChild<ComboRow>,
    #[template_child]
    pub add_button: TemplateChild<Button>,
    /// Latest server-confirmed list (host-OS strings only). Rebuilt
    /// from scratch on every update — the list is small enough that
    /// diffing isn't worth the complexity.
    pub apps: RefCell<Vec<String>>,
    /// Last running-apps snapshot from
    /// `frontmost_app::list_running_apps`. Cached so a change to
    /// the suppression list (add/remove) can immediately re-apply
    /// the "filter out already-suppressed entries" rule against
    /// the picker, instead of waiting for the 5 s auto-refresh.
    pub last_running: RefCell<Vec<RunningApp>>,
    /// Display-name + icon-texture lookup keyed by identifier.
    /// Populated whenever `set_running_apps` runs (covers
    /// currently-running apps) and lazily extended via
    /// `input_capture::frontmost_app::lookup_app_metadata` when
    /// the suppressed-apps rebuild encounters an identifier we
    /// haven't seen before — typically apps the user added but
    /// aren't running right now.
    pub metadata: RefCell<HashMap<String, AppMetadata>>,
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
                Signal::builder("request-add")
                    .param_types([String::static_type()])
                    .build(),
                Signal::builder("request-remove")
                    .param_types([String::static_type()])
                    .build(),
            ]
        })
    }

    fn constructed(&self) {
        self.parent_constructed();

        // Picker title; subtitle gets rendered by the expression
        // (selected app's display name).
        let _ = HostKind::current();
        self.app_picker.set_title("App");
        self.add_group
            .set_description(Some("Pick a running app to suppress."));

        // Selected-display: use a closure expression that pulls
        // display_name off the RunningAppObject. Without this the
        // row's subtitle would be the GObject's debug repr.
        let expression = gtk::ClosureExpression::new::<String>(
            &[] as &[gtk::Expression],
            glib::closure!(|obj: Option<glib::Object>| {
                obj.and_then(|o| o.downcast::<RunningAppObject>().ok())
                    .map(|app| app.display_name())
                    .unwrap_or_default()
            }),
        );
        self.app_picker.set_expression(Some(&expression));

        // Popover rows: icon + label, single line. Each row has a
        // fixed minimum width (320 px) so the popover doesn't
        // shrink horizontally as the user types into the search
        // field — without this, filtering down to one short name
        // collapses the popover to ~120 px which is jarring.
        let factory = SignalListItemFactory::new();
        factory.connect_setup(|_, list_item| {
            let row = GtkBox::new(Orientation::Horizontal, 10);
            row.set_hexpand(true);
            row.set_size_request(320, -1);
            let image = Image::new();
            image.set_pixel_size(20);
            let label = Label::new(None);
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.append(&image);
            row.append(&label);
            list_item
                .downcast_ref::<ListItem>()
                .expect("ListItem")
                .set_child(Some(&row));
        });
        factory.connect_bind(|_, list_item| {
            let item = list_item
                .downcast_ref::<ListItem>()
                .expect("ListItem");
            let Some(obj) = item.item() else { return };
            let Ok(app) = obj.downcast::<RunningAppObject>() else {
                return;
            };
            let Some(child) = item.child() else { return };
            let Ok(row) = child.downcast::<GtkBox>() else { return };
            // first child = Image, next = Label
            let Some(first) = row.first_child() else { return };
            let Some(second) = first.next_sibling() else { return };
            if let Ok(image) = first.downcast::<Image>() {
                if let Some(texture) = app.icon() {
                    image.set_paintable(Some(&texture));
                } else {
                    image.set_icon_name(Some("application-x-executable"));
                }
            }
            if let Ok(label) = second.downcast::<Label>() {
                label.set_label(&app.display_name());
            }
        });
        self.app_picker.set_factory(Some(&factory));

        // Empty model + disabled state until the daemon answers
        // ListRunningApps.
        let empty: ListStore = ListStore::new::<RunningAppObject>();
        self.app_picker.set_model(Some(&empty));
        self.app_picker.set_sensitive(false);
        self.add_button.set_sensitive(false);

        // Close on Escape.
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
        let Some(obj) = self.app_picker.selected_item() else {
            return;
        };
        let Ok(app) = obj.downcast::<RunningAppObject>() else {
            return;
        };
        let value = app.identifier();
        if value.is_empty() {
            return;
        }
        self.obj().emit_by_name::<()>("request-add", &[&value]);
    }

    pub(super) fn rebuild_list(&self) {
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
        for value in apps.iter() {
            self.ensure_metadata_for(value);
            let metadata = self.metadata.borrow();
            let entry = metadata.get(value);
            let title = entry
                .map(|m| m.display_name.clone())
                .unwrap_or_else(|| value.clone());
            let row = adw::ActionRow::builder().title(&title).build();
            let icon_widget = Image::new();
            icon_widget.set_pixel_size(20);
            if let Some(texture) = entry.and_then(|m| m.icon.clone()) {
                icon_widget.set_paintable(Some(&texture));
            } else {
                icon_widget.set_icon_name(Some("application-x-executable"));
            }
            row.add_prefix(&icon_widget);
            let delete = Button::builder()
                .icon_name("user-trash-symbolic")
                .valign(gtk::Align::Center)
                .halign(gtk::Align::Center)
                .tooltip_text("Remove from suppression list")
                .build();
            delete.add_css_class("error");
            let weak_self = self.obj().downgrade();
            let value_clone = value.clone();
            delete.connect_clicked(move |_| {
                let Some(window) = weak_self.upgrade() else {
                    return;
                };
                window.emit_by_name::<()>("request-remove", &[&value_clone]);
            });
            row.add_suffix(&delete);
            self.entries_list.append(&row);
        }
    }

    /// Make sure the metadata cache has an entry for `identifier`.
    /// Cache hits are free; misses query Launch Services via
    /// `input_capture::frontmost_app::lookup_app_metadata` to find
    /// the .app on disk, derive a display name + icon, and store
    /// the result. If the identifier doesn't resolve (uninstalled
    /// app, unsupported platform), no entry is inserted and the
    /// caller falls back to displaying the raw identifier.
    fn ensure_metadata_for(&self, identifier: &str) {
        if self.metadata.borrow().contains_key(identifier) {
            return;
        }
        let Some(app) = input_capture::frontmost_app::lookup_app_metadata(identifier) else {
            return;
        };
        let icon = app.icon_png.as_ref().and_then(|bytes| {
            let glib_bytes = glib::Bytes::from(bytes);
            gdk::Texture::from_bytes(&glib_bytes).ok()
        });
        self.metadata.borrow_mut().insert(
            identifier.to_owned(),
            AppMetadata {
                display_name: app.display_name,
                icon,
            },
        );
    }

    /// True when the picker's popover is currently presented to
    /// the user. The auto-refresh timer skips updates in that
    /// state so the popover doesn't redraw mid-search and steal
    /// keyboard focus.
    pub(super) fn picker_is_open(&self) -> bool {
        // Walk the AdwComboRow's descendants for a Popover that's
        // mapped + visible. AdwComboRow doesn't expose its
        // internal popover directly so we discover it by traversal.
        fn find_open_popover(widget: &gtk::Widget) -> bool {
            if let Ok(popover) = widget.clone().downcast::<gtk::Popover>() {
                if popover.is_visible() {
                    return true;
                }
            }
            let mut child = widget.first_child();
            while let Some(c) = child {
                if find_open_popover(&c) {
                    return true;
                }
                child = c.next_sibling();
            }
            false
        }
        find_open_popover(self.app_picker.upcast_ref::<gtk::Widget>())
    }

    /// Refill the picker model with `running`. Already-suppressed
    /// apps are filtered so the user can't add a duplicate, and
    /// the previously-selected app (if it's still present) stays
    /// selected across refreshes so the auto-refresh timer doesn't
    /// reset the user's choice mid-click.
    pub(super) fn set_running_apps(&self, running: Vec<RunningApp>) {
        // Cache the snapshot so a later suppression-list change
        // (add/remove) can re-apply the picker filter without
        // waiting for the next auto-refresh tick.
        self.last_running.replace(running.clone());
        // Mirror running-apps metadata into the suppressed-list
        // cache so the entries list can render their icons + names
        // without a separate Launch Services round-trip per row.
        {
            let mut metadata = self.metadata.borrow_mut();
            for app in &running {
                let icon = app.icon_png.as_ref().and_then(|bytes| {
                    let glib_bytes = glib::Bytes::from(bytes);
                    gdk::Texture::from_bytes(&glib_bytes).ok()
                });
                metadata.insert(
                    app.identifier.clone(),
                    AppMetadata {
                        display_name: app.display_name.clone(),
                        icon,
                    },
                );
            }
        }
        // Re-render the suppressed list so any rows missing icon /
        // name pick up the freshly cached metadata.
        self.rebuild_list();
        let suppressed_lc: Vec<String> = self
            .apps
            .borrow()
            .iter()
            .map(|s| s.to_lowercase())
            .collect();
        let filtered: Vec<RunningApp> = running
            .into_iter()
            .filter(|a| {
                !suppressed_lc
                    .iter()
                    .any(|s| s == &a.identifier.to_lowercase())
            })
            .collect();

        // Remember selected identifier so we can reselect after refill.
        let prev_selected: Option<String> = self
            .app_picker
            .selected_item()
            .and_then(|o| o.downcast::<RunningAppObject>().ok())
            .map(|a| a.identifier());

        let store: ListStore = ListStore::new::<RunningAppObject>();
        for app in &filtered {
            store.append(&RunningAppObject::new(app));
        }
        self.app_picker.set_model(Some(&store));

        if filtered.is_empty() {
            self.app_picker.set_sensitive(false);
            self.add_button.set_sensitive(false);
            return;
        }
        self.app_picker.set_sensitive(true);
        self.add_button.set_sensitive(true);

        if let Some(prev) = prev_selected {
            if let Some(idx) = filtered.iter().position(|a| a.identifier == prev) {
                self.app_picker.set_selected(idx as u32);
            }
        }
    }
}
