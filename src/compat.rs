//! Widgets that need libadwaita 1.7/1.8 (Ubuntu 26.04 and newer). With the
//! default `modern` feature they are the real thing; without it (libadwaita
//! 1.5, e.g. Ubuntu 24.04) they are rebuilt from older widgets.

use adw::prelude::*;

#[cfg(feature = "modern")]
pub type Spinner = adw::Spinner;
#[cfg(not(feature = "modern"))]
pub type Spinner = gtk::Spinner;

#[cfg(feature = "modern")]
pub type ButtonRow = adw::ButtonRow;
#[cfg(not(feature = "modern"))]
pub type ButtonRow = adw::ActionRow;

/// A centred busy indicator that ignores the pointer.
pub fn spinner() -> Spinner {
    #[cfg(feature = "modern")]
    let s = Spinner::builder();
    #[cfg(not(feature = "modern"))]
    let s = Spinner::builder().spinning(true);
    s.halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .can_target(false)
        .build()
}

/// A full-width row that acts like a button.
pub fn button_row(title: &str, icon: &str) -> ButtonRow {
    #[cfg(feature = "modern")]
    {
        ButtonRow::builder()
            .title(title)
            .start_icon_name(icon)
            .build()
    }
    #[cfg(not(feature = "modern"))]
    {
        let row = ButtonRow::builder()
            .title(title)
            .activatable(true)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(icon));
        row
    }
}

/// Pill-shaped single-choice switch, addressed by item name.
#[cfg(feature = "modern")]
#[derive(Clone)]
pub struct ToggleGroup(adw::ToggleGroup);

#[cfg(feature = "modern")]
impl ToggleGroup {
    pub fn new(items: &[(&str, &str)]) -> Self {
        let group = adw::ToggleGroup::builder()
            .valign(gtk::Align::Center)
            .build();
        group.add_css_class("round");
        for (name, label) in items {
            group.add(adw::Toggle::builder().name(*name).label(*label).build());
        }
        Self(group)
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.0.upcast_ref()
    }

    pub fn active_name(&self) -> Option<String> {
        self.0.active_name().map(|s| s.to_string())
    }

    pub fn set_active_name(&self, name: Option<&str>) {
        self.0.set_active_name(name);
    }

    pub fn connect_active_name_notify<F: Fn(&Self) + 'static>(&self, f: F) {
        let this = self.clone();
        self.0.connect_active_name_notify(move |_| f(&this));
    }
}

#[cfg(not(feature = "modern"))]
#[derive(Clone)]
pub struct ToggleGroup {
    root: gtk::Box,
    buttons: std::rc::Rc<Vec<(String, gtk::ToggleButton)>>,
}

#[cfg(not(feature = "modern"))]
impl ToggleGroup {
    pub fn new(items: &[(&str, &str)]) -> Self {
        let root = gtk::Box::builder().valign(gtk::Align::Center).build();
        root.add_css_class("linked");
        root.add_css_class("round");
        let mut buttons: Vec<(String, gtk::ToggleButton)> = Vec::new();
        for (name, label) in items {
            let button = gtk::ToggleButton::builder().label(*label).build();
            if let Some((_, first)) = buttons.first() {
                button.set_group(Some(first));
            }
            root.append(&button);
            buttons.push((name.to_string(), button));
        }
        Self {
            root,
            buttons: std::rc::Rc::new(buttons),
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    pub fn active_name(&self) -> Option<String> {
        self.buttons
            .iter()
            .find(|(_, b)| b.is_active())
            .map(|(n, _)| n.clone())
    }

    pub fn set_active_name(&self, name: Option<&str>) {
        match name {
            Some(name) => {
                if let Some((_, b)) = self.buttons.iter().find(|(n, _)| n == name) {
                    b.set_active(true);
                }
            }
            None => {
                // Buttons in a group can't be cleared directly; a hidden
                // sentinel isn't worth it, callers always pick a name.
            }
        }
    }

    pub fn connect_active_name_notify<F: Fn(&Self) + 'static>(&self, f: F) {
        let f = std::rc::Rc::new(f);
        for (_, button) in self.buttons.iter() {
            let this = self.clone();
            let f = f.clone();
            // Only the button that became active reports a change.
            button.connect_toggled(move |b| {
                if b.is_active() {
                    f(&this);
                }
            });
        }
    }
}

/// Keyboard shortcut overview: (section title, [(description, accelerator)]).
pub type ShortcutSections<'a> = &'a [(&'a str, &'a [(&'a str, &'a str)])];

#[cfg(feature = "modern")]
pub fn show_shortcuts(parent: Option<&gtk::Window>, sections: ShortcutSections) {
    let dialog = adw::ShortcutsDialog::new();
    for (title, items) in sections {
        let section = adw::ShortcutsSection::new(Some(title));
        for (label, accel) in *items {
            section.add(adw::ShortcutsItem::new(label, accel));
        }
        dialog.add(section);
    }
    dialog.present(parent);
}

#[cfg(not(feature = "modern"))]
pub fn show_shortcuts(parent: Option<&gtk::Window>, sections: ShortcutSections) {
    let page = adw::PreferencesPage::new();
    for (title, items) in sections {
        let group = adw::PreferencesGroup::builder().title(*title).build();
        for (label, accel) in *items {
            let row = adw::ActionRow::builder().title(gtk::glib::markup_escape_text(label)).build();
            let keys = gtk::ShortcutLabel::new(accel);
            keys.set_valign(gtk::Align::Center);
            row.add_suffix(&keys);
            group.add(&row);
        }
        page.add(&group);
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder()
        .title("Keyboard Shortcuts")
        .content_width(460)
        .content_height(560)
        .child(&toolbar)
        .build();
    dialog.present(parent);
}
