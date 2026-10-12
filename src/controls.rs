//! Side panel exposing the camera's hardware (V4L2) controls.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::devices::Camera;
use crate::settings::Settings;
use crate::v4l2;

// V4L2 control ids (linux/v4l2-controls.h).
const CID_BRIGHTNESS: u32 = 0x0098_0900;
const CID_CONTRAST: u32 = 0x0098_0901;
const CID_SATURATION: u32 = 0x0098_0902;
const CID_HUE: u32 = 0x0098_0903;
const CID_AUTO_WHITE_BALANCE: u32 = 0x0098_090c;
const CID_RED_BALANCE: u32 = 0x0098_090e;
const CID_BLUE_BALANCE: u32 = 0x0098_090f;
const CID_GAMMA: u32 = 0x0098_0910;
const CID_EXPOSURE: u32 = 0x0098_0911;
const CID_AUTOGAIN: u32 = 0x0098_0912;
const CID_GAIN: u32 = 0x0098_0913;
const CID_HUE_AUTO: u32 = 0x0098_0919;
const CID_WHITE_BALANCE_TEMPERATURE: u32 = 0x0098_091a;
const CID_SHARPNESS: u32 = 0x0098_091b;
const CID_BACKLIGHT_COMPENSATION: u32 = 0x0098_091c;
const CID_EXPOSURE_AUTO: u32 = 0x009a_0901;
const CID_EXPOSURE_ABSOLUTE: u32 = 0x009a_0902;
const CID_EXPOSURE_AUTO_PRIORITY: u32 = 0x009a_0903;
const CID_PAN_ABSOLUTE: u32 = 0x009a_0908;
const CID_TILT_ABSOLUTE: u32 = 0x009a_0909;
const CID_FOCUS_ABSOLUTE: u32 = 0x009a_090a;
const CID_FOCUS_AUTO: u32 = 0x009a_090c;
const CID_ZOOM_ABSOLUTE: u32 = 0x009a_090d;

/// Controls grouped by what they do, in display order. Anything a camera
/// offers that is not listed here ends up in "Other".
const GROUPS: &[(&str, &[u32])] = &[
    (
        "Exposure",
        &[
            CID_EXPOSURE_AUTO,
            CID_EXPOSURE_ABSOLUTE,
            CID_EXPOSURE,
            CID_EXPOSURE_AUTO_PRIORITY,
            CID_AUTOGAIN,
            CID_GAIN,
            CID_BACKLIGHT_COMPENSATION,
        ],
    ),
    (
        "Color",
        &[
            CID_AUTO_WHITE_BALANCE,
            CID_WHITE_BALANCE_TEMPERATURE,
            CID_RED_BALANCE,
            CID_BLUE_BALANCE,
            CID_SATURATION,
            CID_HUE_AUTO,
            CID_HUE,
        ],
    ),
    (
        "Image",
        &[CID_BRIGHTNESS, CID_CONTRAST, CID_GAMMA, CID_SHARPNESS],
    ),
    (
        "Focus &amp; Zoom",
        &[
            CID_FOCUS_AUTO,
            CID_FOCUS_ABSOLUTE,
            CID_ZOOM_ABSOLUTE,
            CID_PAN_ABSOLUTE,
            CID_TILT_ABSOLUTE,
        ],
    ),
];

/// Value as shown next to a slider, with a unit where the kernel defines one.
fn format_value(id: u32, value: i32) -> String {
    match id {
        // 100 µs units
        CID_EXPOSURE_ABSOLUTE => {
            let ms = value as f64 / 10.0;
            if ms < 10.0 {
                format!("{ms:.1} ms")
            } else {
                format!("{ms:.0} ms")
            }
        }
        CID_WHITE_BALANCE_TEMPERATURE => format!("{value} K"),
        _ => value.to_string(),
    }
}

struct SliderRow {
    row: adw::PreferencesRow,
    scale: gtk::Scale,
    value: gtk::Label,
}

enum RowKind {
    Switch(adw::SwitchRow),
    Combo(adw::ComboRow, Vec<i32>),
    Slider(SliderRow),
}

impl RowKind {
    fn widget(&self) -> gtk::Widget {
        match self {
            RowKind::Switch(r) => r.clone().upcast(),
            RowKind::Combo(r, _) => r.clone().upcast(),
            RowKind::Slider(s) => s.row.clone().upcast(),
        }
    }
}

type CloseHandler = Box<dyn Fn()>;

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
    reset_group: adw::PreferencesGroup,
    updating: Cell<bool>,
    on_close: RefCell<Option<CloseHandler>>,
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
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Close (K)")
            .build();
        close.add_css_class("flat");
        close.add_css_class("circular");
        header.pack_end(&close);
        widget.append(&header);

        let stack = gtk::Stack::builder().vexpand(true).build();
        let page = adw::PreferencesPage::new();
        // Reset sits at the bottom, spelled out, away from the header (an
        // icon-only reset in the corner was easy to mistake for "back").
        let reset_group = adw::PreferencesGroup::new();
        let reset = crate::compat::button_row("Reset All to Defaults", "edit-undo-symbolic");
        reset_group.add(&reset);
        page.add(&reset_group);
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
            reset_group,
            updating: Cell::new(false),
            on_close: RefCell::new(None),
        });
        reset.connect_activated(glib::clone!(
            #[weak]
            this,
            move |_| this.reset_all()
        ));
        close.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| {
                if let Some(f) = this.on_close.borrow().as_ref() {
                    f();
                }
            }
        ));
        this
    }

    /// Called when the panel's close button is pressed.
    pub fn connect_close(&self, f: impl Fn() + 'static) {
        *self.on_close.borrow_mut() = Some(Box::new(f));
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
            return;
        }
        self.stack.set_visible_child_name("controls");

        let controls = self.controls.borrow().clone();
        let known: Vec<u32> = GROUPS
            .iter()
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect();
        let mut sections: Vec<(&str, Vec<&v4l2::Ctrl>)> = GROUPS
            .iter()
            .map(|(title, ids)| {
                let members = ids
                    .iter()
                    .filter_map(|id| controls.iter().find(|c| c.id == *id))
                    .collect();
                (*title, members)
            })
            .collect();
        sections.push((
            "Other",
            controls.iter().filter(|c| !known.contains(&c.id)).collect(),
        ));

        // Groups go above the reset button, which stays last.
        self.page.remove(&self.reset_group);
        for (title, members) in sections {
            let group = adw::PreferencesGroup::builder().title(title).build();
            let mut any = false;
            for c in members {
                if let Some(row) = self.make_row(c) {
                    group.add(&row.widget());
                    self.rows.borrow_mut().push((c.id, row));
                    any = true;
                }
            }
            if any {
                self.page.add(&group);
                self.groups.borrow_mut().push(group);
            }
        }
        self.page.add(&self.reset_group);
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
            v4l2::TYPE_INTEGER => Some(RowKind::Slider(self.make_slider(c, &title))),
            _ => None,
        }
    }

    /// Name and value on one line, a full-width slider below: every slider
    /// starts at the same place whatever the length of its name.
    fn make_slider(self: &Rc<Self>, c: &v4l2::Ctrl, title: &str) -> SliderRow {
        let row = adw::PreferencesRow::builder()
            .title(title)
            .activatable(false)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_top(10)
            .margin_bottom(4)
            .margin_start(12)
            .margin_end(12)
            .build();
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name = gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let value = gtk::Label::new(None);
        value.add_css_class("dim-label");
        value.add_css_class("numeric");
        head.append(&name);
        head.append(&value);
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
            .draw_value(false)
            .hexpand(true)
            .build();
        content.append(&head);
        content.append(&scale);
        row.set_child(Some(&content));

        let ctrl = c.clone();
        let label = value.clone();
        let (min, st) = (c.minimum, c.step.max(1));
        scale.connect_value_changed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |s| {
                let v = min + ((s.value() - min as f64) / st as f64).round() as i32 * st;
                label.set_label(&format_value(ctrl.id, v));
                this.set(&ctrl, v);
            }
        ));
        SliderRow { row, scale, value }
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
            let auto = fresh.inactive();
            row.widget().set_sensitive(!auto);
            match row {
                RowKind::Switch(r) => r.set_active(value != 0),
                RowKind::Combo(r, values) => {
                    if let Some(i) = values.iter().position(|v| *v == value) {
                        r.set_selected(i as u32);
                    }
                }
                RowKind::Slider(s) => {
                    s.scale.set_value(value as f64);
                    // Greyed out because an automatic mode drives it: say so.
                    s.value.set_label(&if auto {
                        "Auto".to_string()
                    } else {
                        format_value(*id, value)
                    });
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_get_units() {
        assert_eq!(format_value(CID_EXPOSURE_ABSOLUTE, 156), "16 ms");
        assert_eq!(format_value(CID_EXPOSURE_ABSOLUTE, 33), "3.3 ms");
        assert_eq!(format_value(CID_WHITE_BALANCE_TEMPERATURE, 4600), "4600 K");
        assert_eq!(format_value(CID_BRIGHTNESS, 128), "128");
    }

    #[test]
    fn groups_do_not_overlap() {
        let mut ids: Vec<u32> = GROUPS
            .iter()
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n);
    }
}
