//! Side panel exposing the camera's hardware (V4L2) controls.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::devices::Camera;
use crate::settings::Settings;
use crate::v4l2;

const USER_CLASS: u32 = 0x0098_0000;

enum RowKind {
    Switch(adw::SwitchRow),
    Combo(adw::ComboRow, Vec<i32>),
    Scale(adw::ActionRow, gtk::Scale),
}

impl RowKind {
    fn widget(&self) -> gtk::Widget {
        match self {
            RowKind::Switch(r) => r.clone().upcast(),
            RowKind::Combo(r, _) => r.clone().upcast(),
            RowKind::Scale(r, _) => r.clone().upcast(),
        }
    }
}

pub struct ControlsPanel {
    pub widget: gtk::Box,
    settings: Settings,
    camera: RefCell<Option<Camera>>,
    device: RefCell<Option<v4l2::Device>>,
    controls: RefCell<Vec<v4l2::Ctrl>>,
    rows: RefCell<Vec<(u32, RowKind)>>,
    groups: RefCell<Vec<adw::PreferencesGroup>>,
    page: adw::PreferencesPage,
    stack: gtk::Stack,
    reset: gtk::Button,
    updating: Cell<bool>,
}

impl ControlsPanel {
    pub fn new(settings: &Settings) -> Rc<Self> {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.add_css_class("controls-panel");
        let header = adw::HeaderBar::builder()
            .show_start_title_buttons(false)
            .show_end_title_buttons(false)
            .build();
        header.add_css_class("flat");
        header.set_title_widget(Some(&adw::WindowTitle::new("Camera Controls", "")));
        let reset = gtk::Button::builder()
            .icon_name("edit-undo-symbolic")
            .tooltip_text("Reset to Defaults")
            .build();
        header.pack_start(&reset);
        widget.append(&header);
        let stack = gtk::Stack::builder().vexpand(true).build();
        let page = adw::PreferencesPage::new();
        stack.add_named(&page, Some("controls"));
        let empty = adw::StatusPage::builder()
            .icon_name("camera-web-symbolic")
            .title("No Controls")
            .description("This camera has no hardware controls.")
            .build();
        empty.add_css_class("compact");
        stack.add_named(&empty, Some("empty"));
        widget.append(&stack);
        let this = Rc::new(Self {
            widget,
            settings: settings.clone(),
            camera: RefCell::new(None),
            device: RefCell::new(None),
            controls: RefCell::new(Vec::new()),
            rows: RefCell::new(Vec::new()),
            groups: RefCell::new(Vec::new()),
            page,
            stack,
            reset,
            updating: Cell::new(false),
        });
        this.reset.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.reset_all()
        ));
        this
    }

    pub fn camera_key(&self) -> Option<String> {
        self.camera.borrow().as_ref().map(|c| c.key.clone())
    }

    pub fn set_camera(self: &Rc<Self>, camera: Option<&Camera>) {
        *self.device.borrow_mut() = None;
        *self.camera.borrow_mut() = camera.cloned();
        let mut controls = Vec::new();
        if let Some(path) = camera.and_then(|c| c.path.as_deref()) {
            match v4l2::Device::open(path) {
                Ok(dev) => {
                    controls = dev.controls();
                    *self.device.borrow_mut() = Some(dev);
                }
                Err(e) => eprintln!("camera-studio: cannot read the camera controls: {e}"),
            }
        }
        *self.controls.borrow_mut() = controls;
        self.rebuild();
    }

    /// Re-apply the values chosen last time (UVC cameras reset on replug).
    pub fn apply_saved(&self) {
        let Some(key) = self.camera_key() else { return };
        let saved = self
            .settings
            .get()
            .controls
            .get(&key)
            .cloned()
            .unwrap_or_default();
        if saved.is_empty() {
            return;
        }
        if let Some(dev) = self.device.borrow().as_ref() {
            // Mode switches first so that dependent values become writable.
            let mut ordered = self.controls.borrow().clone();
            ordered.sort_by_key(|c| c.type_ == v4l2::TYPE_INTEGER);
            for c in ordered {
                if let Some(v) = saved.get(&c.key()) {
                    let _ = dev.set(c.id, *v);
                }
            }
        }
        self.refresh();
    }

    fn rebuild(self: &Rc<Self>) {
        for g in self.groups.borrow_mut().drain(..) {
            self.page.remove(&g);
        }
        self.rows.borrow_mut().clear();
        if self.controls.borrow().is_empty() {
            self.stack.set_visible_child_name("empty");
            self.reset.set_sensitive(false);
            return;
        }
        self.stack.set_visible_child_name("controls");
        self.reset.set_sensitive(true);
        let camera_group = adw::PreferencesGroup::builder()
            .title("Exposure &amp; Focus")
            .build();
        let image_group = adw::PreferencesGroup::builder().title("Image").build();
        for c in self.controls.borrow().iter() {
            let Some(row) = self.make_row(c) else {
                continue;
            };
            let target = if c.id & 0xFFFF_0000 == USER_CLASS {
                &image_group
            } else {
                &camera_group
            };
            target.add(&row.widget());
            self.rows.borrow_mut().push((c.id, row));
        }
        for g in [camera_group, image_group] {
            self.page.add(&g);
            self.groups.borrow_mut().push(g);
        }
        self.refresh();
    }

    fn make_row(self: &Rc<Self>, c: &v4l2::Ctrl) -> Option<RowKind> {
        let title = v4l2::control_label(&c.name).to_string();
        let ctrl = c.clone();
        match c.type_ {
            v4l2::TYPE_BOOLEAN => {
                let row = adw::SwitchRow::builder().title(&title).build();
                row.connect_active_notify(glib::clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |r| this.set(&ctrl, r.is_active() as i32)
                ));
                Some(RowKind::Switch(row))
            }
            v4l2::TYPE_MENU | v4l2::TYPE_INTEGER_MENU if !c.menu.is_empty() => {
                let labels: Vec<&str> = c.menu.iter().map(|(_, l)| v4l2::menu_label(l)).collect();
                let row = adw::ComboRow::builder()
                    .title(&title)
                    .model(&gtk::StringList::new(&labels))
                    .build();
                let values: Vec<i32> = c.menu.iter().map(|(i, _)| *i).collect();
                let vals = values.clone();
                row.connect_selected_notify(glib::clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |r| {
                        if let Some(v) = vals.get(r.selected() as usize) {
                            this.set(&ctrl, *v);
                        }
                    }
                ));
                Some(RowKind::Combo(row, values))
            }
            v4l2::TYPE_INTEGER => {
                let row = adw::ActionRow::builder().title(&title).build();
                let step = c.step.max(1) as f64;
                let adj = gtk::Adjustment::new(
                    c.default as f64,
                    c.minimum as f64,
                    c.maximum as f64,
                    step,
                    step * 10.0,
                    0.0,
                );
                let scale = gtk::Scale::builder()
                    .adjustment(&adj)
                    .digits(0)
                    .draw_value(true)
                    .value_pos(gtk::PositionType::Left)
                    .hexpand(true)
                    .valign(gtk::Align::Center)
                    .build();
                scale.set_size_request(150, -1);
                scale.add_mark(c.default as f64, gtk::PositionType::Bottom, None);
                let (min, st) = (c.minimum, c.step.max(1));
                scale.connect_value_changed(glib::clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |s| {
                        let v = min + ((s.value() - min as f64) / st as f64).round() as i32 * st;
                        this.set(&ctrl, v);
                    }
                ));
                row.add_suffix(&scale);
                Some(RowKind::Scale(row, scale))
            }
            _ => None,
        }
    }

    fn set(self: &Rc<Self>, c: &v4l2::Ctrl, value: i32) {
        if self.updating.get() {
            return;
        }
        let ok = match self.device.borrow().as_ref() {
            Some(dev) => match dev.set(c.id, value) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("camera-studio: cannot set {}: {e}", c.name);
                    false
                }
            },
            None => false,
        };
        if ok && let Some(key) = self.camera_key() {
            let ckey = c.key();
            self.settings.update_quiet(|d| {
                d.controls.entry(key).or_default().insert(ckey, value);
            });
        }
        // Auto modes toggle the "inactive" flag of related controls.
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move || this.refresh()
        ));
    }

    pub fn refresh(&self) {
        let device = self.device.borrow();
        let Some(dev) = device.as_ref() else { return };
        self.updating.set(true);
        for (id, row) in self.rows.borrow().iter() {
            let (Some(fresh), Ok(value)) = (dev.query(*id), dev.get(*id)) else {
                continue;
            };
            row.widget().set_sensitive(!fresh.inactive());
            match row {
                RowKind::Switch(r) => r.set_active(value != 0),
                RowKind::Combo(r, values) => {
                    if let Some(i) = values.iter().position(|v| *v == value) {
                        r.set_selected(i as u32);
                    }
                }
                RowKind::Scale(_, s) => s.set_value(value as f64),
            }
        }
        self.updating.set(false);
    }

    fn reset_all(&self) {
        if let Some(dev) = self.device.borrow().as_ref() {
            let mut ordered = self.controls.borrow().clone();
            ordered.sort_by_key(|c| c.type_ == v4l2::TYPE_INTEGER);
            for c in ordered {
                let _ = dev.set(c.id, c.default);
            }
        }
        if let Some(key) = self.camera_key() {
            self.settings.update_quiet(|d| {
                d.controls.remove(&key);
            });
        }
        self.refresh();
    }
}
