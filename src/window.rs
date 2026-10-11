//! Main window: viewfinder, capture controls, gallery navigation.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::camera::{self, CameraPipeline, ErrorKind, Event, RecordOptions};
use crate::controls::ControlsPanel;
use crate::devices::{self, Camera, DeviceManager, Mode};
use crate::effects;
use crate::gallery::{GalleryPage, ViewerPage};
use crate::photo;
use crate::preferences;
use crate::settings::Settings;
use crate::sounds::Sounds;
use crate::storage;
use crate::thumbnails::{Thumb, ThumbnailCache};
use crate::viewfinder::Viewfinder;

use crate::settings::{BURST_CHOICES, TIMER_CHOICES};
const ZOOM_STEPS: [f64; 10] = [1.0, 1.25, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0];
const MODES: [(&str, &str); 3] = [("photo", "Photo"), ("video", "Video"), ("qr", "QR Code")];

#[derive(Clone, Copy, PartialEq)]
enum Pending {
    Photos,
    Recording,
}

fn osd_button(icon: &str, tooltip: &str, toggle: bool, action: Option<&str>) -> gtk::Button {
    let b: gtk::Button = if toggle {
        gtk::ToggleButton::builder()
            .icon_name(icon)
            .build()
            .upcast()
    } else {
        gtk::Button::builder().icon_name(icon).build()
    };
    b.set_tooltip_text(Some(tooltip));
    b.add_css_class("osd-button");
    b.add_css_class("flat");
    b.set_focus_on_click(false);
    b.set_valign(gtk::Align::Center);
    if let Some(a) = action {
        b.set_action_name(Some(a));
    }
    b
}

/// Big round button of the bottom panel (effects, switch camera, …).
fn round_button(icon: &str, tooltip: &str, toggle: bool, action: &str) -> gtk::Button {
    let b = osd_button(icon, tooltip, toggle, Some(action));
    b.remove_css_class("osd-button");
    b.add_css_class("round-control");
    b
}

fn osd_menu_button(tooltip: &str) -> gtk::MenuButton {
    let b = gtk::MenuButton::builder()
        .tooltip_text(tooltip)
        .focus_on_click(false)
        .valign(gtk::Align::Center)
        .build();
    b.add_css_class("osd-button");
    b.add_css_class("flat");
    b
}

fn radio_menu(action: &str, items: &[(String, String)]) -> gio::Menu {
    let menu = gio::Menu::new();
    for (label, target) in items {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some(action), Some(&target.to_variant()));
        menu.append_item(&item);
    }
    menu
}

pub struct Window {
    pub window: adw::ApplicationWindow,
    pub settings: Settings,
    pub devices: Rc<DeviceManager>,
    pub camera: Rc<CameraPipeline>,
    sounds: Rc<Sounds>,
    thumbs: Rc<ThumbnailCache>,
    pub controls: Rc<ControlsPanel>,
    pub gallery: Rc<GalleryPage>,
    pub viewer: Rc<ViewerPage>,

    toasts: adw::ToastOverlay,
    camera_toasts: adw::ToastOverlay,
    pub nav: adw::NavigationView,
    camera_page: adw::NavigationPage,
    view: Viewfinder,
    split: adw::OverlaySplitView,
    flash: gtk::Box,
    countdown_box: gtk::Box,
    countdown_label: gtk::Label,
    spinner: adw::Spinner,
    status_box: gtk::Box,
    status_page: adw::StatusPage,
    test_button: gtk::Button,
    info: gtk::Label,
    camera_button: gtk::MenuButton,
    camera_label: gtk::Label,
    resolution_button: gtk::MenuButton,
    rec_indicator: gtk::Box,
    rec_dot: gtk::Box,
    rec_label: gtk::Label,
    timer_button: gtk::MenuButton,
    burst_button: gtk::MenuButton,
    qr_revealer: gtk::Revealer,
    qr_type: gtk::Label,
    qr_text: gtk::Label,
    qr_open: gtk::Button,
    effects_revealer: gtk::Revealer,
    pub effect_buttons: RefCell<HashMap<&'static str, gtk::ToggleButton>>,
    effect_pictures: HashMap<&'static str, Viewfinder>,
    effects_refresh: RefCell<Option<glib::SourceId>>,
    previews_busy: Cell<bool>,
    zoom_label: gtk::Button,
    mode_group: adw::ToggleGroup,
    gallery_thumb: Viewfinder,
    gallery_placeholder: gtk::Image,
    shutter: gtk::Button,
    snapshot_button: gtk::Button,
    switch_button: gtk::Button,

    pub current: RefCell<Option<Camera>>,
    mode: RefCell<Option<Mode>>,
    countdown: Cell<u32>,
    countdown_source: RefCell<Option<glib::SourceId>>,
    countdown_then: Cell<Pending>,
    burst_left: Cell<u32>,
    burst_source: RefCell<Option<glib::SourceId>>,
    retry_source: RefCell<Option<glib::SourceId>>,
    info_source: RefCell<Option<glib::SourceId>>,
    rec_tick: RefCell<Option<glib::SourceId>>,
    close_when_saved: Cell<bool>,
    page_visible: Cell<bool>,
    last_barcode: RefCell<Option<String>>,
    pinch_base: Cell<f64>,
    photos_in_flight: Cell<u32>,
    recording_saving: Cell<bool>,
}

impl Window {
    pub fn new(app: &adw::Application, settings: Settings, devices: Rc<DeviceManager>) -> Rc<Self> {
        let accel = camera::Accel::detect(settings.get().hardware_acceleration);
        let camera = CameraPipeline::new(accel);
        let thumbs = ThumbnailCache::new();
        let sounds = Sounds::new(settings.get().shutter_sound);
        sounds.preload("shutter");
        let controls = ControlsPanel::new(&settings);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Camera Studio")
            .default_width(settings.get().window_width)
            .default_height(settings.get().window_height)
            .build();
        window.set_size_request(360, 480);
        if settings.get().window_maximized {
            window.maximize();
        }
        let toasts = adw::ToastOverlay::new();
        let nav = adw::NavigationView::new();
        toasts.set_child(Some(&nav));
        window.set_content(Some(&toasts));

        // Layout of the camera page (nothing is drawn over the picture except
        // transient feedback):
        //
        //   top bar      camera · resolution · [rec] · timer burst grid mirror controls menu
        //   viewport     viewfinder + countdown/flash/status/QR card/toasts
        //   bottom panel effects strip (when open)
        //                zoom · Photo|Video|QR Code
        //                gallery · shutter · effects/switch

        // --- viewport ------------------------------------------------------------------------
        let view = Viewfinder::default();
        view.set_focusable(true);
        view.set_hexpand(true);
        view.set_vexpand(true);
        let viewport = gtk::Overlay::new();
        viewport.add_css_class("viewport");
        viewport.set_child(Some(&view));

        let flash = gtk::Box::builder().can_target(false).opacity(0.0).build();
        flash.add_css_class("flash");
        viewport.add_overlay(&flash);

        let countdown_label = gtk::Label::builder().hexpand(true).vexpand(true).build();
        countdown_label.add_css_class("countdown");
        countdown_label.add_css_class("numeric");
        let countdown_box = gtk::Box::builder()
            .can_target(false)
            .visible(false)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        countdown_box.add_css_class("countdown-bubble");
        countdown_box.append(&countdown_label);
        viewport.add_overlay(&countdown_box);

        let spinner = adw::Spinner::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .can_target(false)
            .build();
        spinner.set_size_request(48, 48);
        viewport.add_overlay(&spinner);

        // Bottom-left: the top centre is the recording badge's place.
        let info = gtk::Label::builder()
            .visible(false)
            .can_target(false)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::End)
            .margin_bottom(10)
            .margin_start(10)
            .build();
        info.add_css_class("info-label");
        info.add_css_class("numeric");
        viewport.add_overlay(&info);

        // QR result card, at the bottom of the picture.
        let qr_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .valign(gtk::Align::End)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        card.add_css_class("qr-card");
        card.append(
            &gtk::Image::builder()
                .icon_name("cs-qr-code-symbolic")
                .pixel_size(28)
                .valign(gtk::Align::Center)
                .build(),
        );
        let texts = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .build();
        let qr_type = gtk::Label::builder().xalign(0.0).build();
        qr_type.add_css_class("caption");
        qr_type.add_css_class("dim-label");
        let qr_text = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .lines(3)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(48)
            .build();
        qr_text.add_css_class("qr-text");
        texts.append(&qr_type);
        texts.append(&qr_text);
        card.append(&texts);
        let card_button = |icon: &str, tip: &str| {
            let b = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .valign(gtk::Align::Center)
                .build();
            b.add_css_class("circular");
            b
        };
        let qr_open = card_button("web-browser-symbolic", "Open link");
        let qr_copy = card_button("edit-copy-symbolic", "Copy");
        let qr_close = card_button("window-close-symbolic", "Close");
        qr_close.add_css_class("flat");
        card.append(&qr_open);
        card.append(&qr_copy);
        card.append(&qr_close);
        qr_revealer.set_child(Some(
            &adw::Clamp::builder().child(&card).maximum_size(560).build(),
        ));
        viewport.add_overlay(&qr_revealer);

        // Camera problems are shown over the picture only; the bars stay usable
        // (e.g. to pick another camera or open the gallery).
        let status_page = adw::StatusPage::builder().vexpand(true).build();
        status_page.add_css_class("compact");
        let status_buttons = gtk::Box::builder()
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        let retry = gtk::Button::with_label("Retry");
        retry.add_css_class("pill");
        retry.add_css_class("suggested-action");
        let test_button = gtk::Button::with_label("Use Test Source");
        test_button.add_css_class("pill");
        status_buttons.append(&retry);
        status_buttons.append(&test_button);
        status_page.set_child(Some(&status_buttons));
        let status_box = gtk::Box::builder().visible(false).build();
        status_box.add_css_class("status-overlay");
        status_box.append(&status_page);
        viewport.add_overlay(&status_box);

        // Toasts belong to the picture area so they never cover the shutter.
        let camera_toasts = adw::ToastOverlay::new();
        camera_toasts.set_child(Some(&viewport));
        camera_toasts.set_vexpand(true);

        // --- top bar -------------------------------------------------------------------------
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        top.add_css_class("top-bar");
        let start = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        start.append(&gtk::WindowControls::new(gtk::PackType::Start));
        let camera_button = osd_menu_button("Choose camera");
        let camera_label = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(18)
            .build();
        let camera_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        camera_content.append(&gtk::Image::from_icon_name("camera-web-symbolic"));
        camera_content.append(&camera_label);
        camera_button.set_child(Some(&camera_content));
        start.append(&camera_button);
        let resolution_button = osd_menu_button("Resolution");
        resolution_button.set_label("—");
        start.append(&resolution_button);
        top.append(&start);
        top.append(&gtk::Box::builder().hexpand(true).build());

        let rec_indicator = gtk::Box::builder()
            .spacing(8)
            .visible(false)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Start)
            .margin_top(10)
            .can_target(false)
            .build();
        rec_indicator.add_css_class("rec-indicator");
        let rec_dot = gtk::Box::builder().valign(gtk::Align::Center).build();
        rec_dot.add_css_class("rec-dot");
        let rec_label = gtk::Label::new(Some("00:00"));
        rec_label.add_css_class("numeric");
        rec_indicator.append(&rec_dot);
        rec_indicator.append(&rec_label);
        viewport.add_overlay(&rec_indicator);

        let end = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let timer_button = osd_menu_button("Self-timer (T)");
        timer_button.set_menu_model(Some(&radio_menu(
            "win.timer",
            &TIMER_CHOICES
                .iter()
                .map(|&s| {
                    let label = if s == 0 {
                        "Timer off".into()
                    } else {
                        format!("{s}-second timer")
                    };
                    (label, s.to_string())
                })
                .collect::<Vec<_>>(),
        )));
        end.append(&timer_button);
        let burst_button = osd_menu_button("Burst (B)");
        burst_button.set_menu_model(Some(&radio_menu(
            "win.burst",
            &BURST_CHOICES
                .iter()
                .map(|&n| {
                    let label = if n == 1 {
                        "Single shot".into()
                    } else {
                        format!("Burst of {n} photos")
                    };
                    (label, n.to_string())
                })
                .collect::<Vec<_>>(),
        )));
        end.append(&burst_button);
        let grid_button = osd_button("view-grid-symbolic", "Grid (G)", true, Some("win.grid"));
        end.append(&grid_button);
        let mirror_button = osd_button(
            "object-flip-horizontal-symbolic",
            "Mirror viewfinder (M)",
            true,
            Some("win.mirror"),
        );
        end.append(&mirror_button);
        end.append(&osd_button(
            "cs-sliders-symbolic",
            "Camera controls (K)",
            true,
            Some("win.controls"),
        ));
        let menu = gio::Menu::new();
        let view_section = gio::Menu::new();
        view_section.append(Some("Show Grid"), Some("win.grid"));
        view_section.append(Some("Mirror Viewfinder"), Some("win.mirror"));
        view_section.append(Some("Show FPS"), Some("win.show-fps"));
        view_section.append(Some("Fullscreen"), Some("win.fullscreen"));
        menu.append_section(None, &view_section);
        let app_section = gio::Menu::new();
        app_section.append(Some("Gallery"), Some("win.gallery"));
        app_section.append(Some("Preferences"), Some("win.preferences"));
        app_section.append(Some("Keyboard Shortcuts"), Some("app.shortcuts"));
        app_section.append(Some("About Camera Studio"), Some("app.about"));
        menu.append_section(None, &app_section);
        let quit_section = gio::Menu::new();
        quit_section.append(Some("Quit"), Some("app.quit"));
        menu.append_section(None, &quit_section);
        let main_menu = osd_menu_button("Main menu");
        main_menu.set_icon_name("open-menu-symbolic");
        main_menu.set_menu_model(Some(&menu));
        main_menu.set_primary(true);
        end.append(&main_menu);
        end.append(&gtk::WindowControls::new(gtk::PackType::End));
        top.append(&end);
        let top_handle = gtk::WindowHandle::builder().child(&top).build();

        // --- bottom panel ------------------------------------------------------------------------
        let bottom = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .build();
        bottom.add_css_class("bottom-panel");

        // Effects strip: every chip shows the current picture with that effect.
        let effects_revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .build();
        let strip = gtk::Box::builder().spacing(6).build();
        strip.add_css_class("effects-strip");
        let mut effect_buttons = HashMap::new();
        let mut effect_pictures = HashMap::new();
        let mut group: Option<gtk::ToggleButton> = None;
        for e in effects::EFFECTS {
            let chip = gtk::Box::new(gtk::Orientation::Vertical, 4);
            let picture = Viewfinder::fixed(84, 48);
            picture.set_halign(gtk::Align::Center);
            picture.add_css_class("effect-thumb");
            chip.append(&picture);
            chip.append(
                &gtk::Label::builder()
                    .label(e.label)
                    .max_width_chars(11)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build(),
            );
            let btn = gtk::ToggleButton::builder()
                .child(&chip)
                .focus_on_click(false)
                .tooltip_text(e.label)
                .build();
            btn.add_css_class("effect");
            if let Some(g) = &group {
                btn.set_group(Some(g));
            } else {
                group = Some(btn.clone());
            }
            strip.append(&btn);
            effect_buttons.insert(e.id, btn);
            effect_pictures.insert(e.id, picture);
        }
        let scroller = gtk::ScrolledWindow::builder()
            .child(&strip)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .build();
        // Vertical mouse wheel scrolls the strip horizontally.
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let adj = scroller.hadjustment();
        wheel.connect_scroll(move |_, _, dy| {
            adj.set_value(adj.value() + dy * 60.0);
            glib::Propagation::Stop
        });
        scroller.add_controller(wheel);
        effects_revealer.set_child(Some(&scroller));
        bottom.append(&effects_revealer);

        // Row 1: zoom · capture mode.
        let row1 = gtk::CenterBox::new();
        let zoom_chip = gtk::Box::builder()
            .spacing(0)
            .valign(gtk::Align::Center)
            .build();
        zoom_chip.add_css_class("chip");
        let zbtn = |icon: &str, tip: &str, action: &str| {
            let b = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .focus_on_click(false)
                .action_name(action)
                .build();
            b.add_css_class("flat");
            b.add_css_class("circular");
            b
        };
        let zoom_label = gtk::Button::builder()
            .label("1×")
            .focus_on_click(false)
            .tooltip_text("Reset zoom (0)")
            .action_name("win.zoom-reset")
            .build();
        zoom_label.add_css_class("flat");
        zoom_label.add_css_class("zoom-value");
        let zoom_out = zbtn("zoom-out-symbolic", "Zoom out (−)", "win.zoom-out");
        let zoom_in = zbtn("zoom-in-symbolic", "Zoom in (+)", "win.zoom-in");
        zoom_chip.append(&zoom_out);
        zoom_chip.append(&zoom_label);
        zoom_chip.append(&zoom_in);
        row1.set_start_widget(Some(&zoom_chip));
        let mode_group = adw::ToggleGroup::builder()
            .valign(gtk::Align::Center)
            .build();
        mode_group.add_css_class("round");
        mode_group.add_css_class("mode-switch");
        for (name, label) in MODES {
            mode_group.add(adw::Toggle::builder().name(name).label(label).build());
        }
        row1.set_center_widget(Some(&mode_group));
        bottom.append(&row1);

        // Row 2: gallery · shutter · effects/switch camera.
        let row2 = gtk::CenterBox::new();
        let gallery_thumb = Viewfinder::thumbnail(52);
        let gallery_placeholder = gtk::Image::builder()
            .icon_name("image-x-generic-symbolic")
            .pixel_size(22)
            .can_target(false)
            .build();
        gallery_placeholder.add_css_class("dim-label");
        let gallery_content = gtk::Overlay::new();
        gallery_content.set_child(Some(&gallery_thumb));
        gallery_content.add_overlay(&gallery_placeholder);
        let gallery_button = gtk::Button::builder()
            .child(&gallery_content)
            .tooltip_text("Gallery (Ctrl+O)")
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Start)
            .focus_on_click(false)
            .build();
        gallery_button.add_css_class("gallery-button");
        let left = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        left.set_size_request(150, -1);
        left.append(&gallery_button);
        row2.set_start_widget(Some(&left));
        let core = gtk::Box::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        core.add_css_class("shutter-core");
        let shutter = gtk::Button::builder()
            .child(&core)
            .tooltip_text("Take photo (Space)")
            .focus_on_click(false)
            .action_name("win.shutter")
            .build();
        shutter.add_css_class("shutter");
        row2.set_center_widget(Some(&shutter));
        let right = gtk::Box::builder()
            .spacing(10)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Center)
            .build();
        right.set_size_request(150, -1);
        let snapshot_button = round_button(
            "camera-photo-symbolic",
            "Take a photo while recording",
            false,
            "win.snapshot",
        );
        snapshot_button.set_visible(false);
        right.append(&snapshot_button);
        right.append(&round_button(
            "cs-filter-symbolic",
            "Effects (E)",
            true,
            "win.effects",
        ));
        let switch_button = round_button(
            "camera-switch-symbolic",
            "Switch camera (C)",
            false,
            "win.switch-camera",
        );
        right.append(&switch_button);
        row2.set_end_widget(Some(&right));
        bottom.append(&row2);

        let stage = gtk::Box::new(gtk::Orientation::Vertical, 0);
        stage.add_css_class("stage");
        stage.append(&top_handle);
        stage.append(&camera_toasts);
        stage.append(&bottom);

        let split = adw::OverlaySplitView::builder()
            .content(&stage)
            .sidebar(&controls.widget)
            .sidebar_position(gtk::PackType::End)
            .show_sidebar(false)
            .min_sidebar_width(300.0)
            .max_sidebar_width(380.0)
            .build();
        let camera_page = adw::NavigationPage::builder()
            .title("Camera")
            .tag("camera")
            .child(&split)
            .build();
        nav.add(&camera_page);

        // Breakpoints: only one is active at a time and, when several match,
        // the one added last wins — so the narrowest comes last and repeats
        // the wider one's setters.
        //  ≤ 860sp: the controls panel floats over the picture instead of squeezing it.
        //  ≤ 620sp: also hide secondary toggles (they are in the menu too),
        //           the camera name and the zoom −/+ buttons.
        let collapse =
            |bp: &adw::Breakpoint| bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        let medium =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 860sp").unwrap());
        collapse(&medium);
        window.add_breakpoint(medium);
        let narrow =
            adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 620sp").unwrap());
        collapse(&narrow);
        for widget in [
            grid_button.upcast_ref::<gtk::Widget>(),
            mirror_button.upcast_ref(),
            camera_label.upcast_ref(),
            zoom_out.upcast_ref(),
            zoom_in.upcast_ref(),
        ] {
            narrow.add_setter(widget, "visible", Some(&false.to_value()));
        }
        narrow.add_setter(&left, "width-request", Some(&64.to_value()));
        narrow.add_setter(&right, "width-request", Some(&64.to_value()));
        window.add_breakpoint(narrow);

        let gallery = GalleryPage::new(&thumbs, {
            let s = settings.clone();
            move || vec![storage::photo_dir(&s), storage::video_dir(&s)]
        });
        let toast_target = toasts.downgrade();
        let viewer = ViewerPage::new(&thumbs, move |text| {
            if let Some(t) = toast_target.upgrade() {
                t.add_toast(
                    adw::Toast::builder()
                        .title(glib::markup_escape_text(text))
                        .timeout(3)
                        .build(),
                );
            }
        });

        let this = Rc::new(Self {
            window,
            settings,
            devices,
            camera,
            sounds,
            thumbs,
            controls,
            gallery,
            viewer,
            toasts,
            camera_toasts,
            nav,
            camera_page,
            view,
            split,
            flash,
            countdown_box,
            countdown_label,
            spinner,
            status_box,
            status_page,
            test_button,
            info,
            camera_button,
            camera_label,
            resolution_button,
            rec_indicator,
            rec_dot,
            rec_label,
            timer_button,
            burst_button,
            qr_revealer,
            qr_type,
            qr_text,
            qr_open,
            effects_revealer,
            effect_buttons: RefCell::new(effect_buttons),
            effect_pictures,
            effects_refresh: RefCell::new(None),
            previews_busy: Cell::new(false),
            zoom_label,
            mode_group,
            gallery_thumb,
            gallery_placeholder,
            shutter,
            snapshot_button,
            switch_button,
            current: RefCell::new(None),
            mode: RefCell::new(None),
            countdown: Cell::new(0),
            countdown_source: RefCell::new(None),
            countdown_then: Cell::new(Pending::Photos),
            burst_left: Cell::new(0),
            burst_source: RefCell::new(None),
            retry_source: RefCell::new(None),
            info_source: RefCell::new(None),
            rec_tick: RefCell::new(None),
            close_when_saved: Cell::new(false),
            page_visible: Cell::new(true),
            last_barcode: RefCell::new(None),
            pinch_base: Cell::new(1.0),
            photos_in_flight: Cell::new(0),
            recording_saving: Cell::new(false),
        });

        this.connect_signals(&retry, &qr_copy, &qr_close, &gallery_button);
        this.setup_actions();
        this.setup_shortcuts();
        this.apply_settings();
        this.update_gallery_button();
        let weak = Rc::downgrade(&this);
        glib::idle_add_local_once(move || {
            if let Some(this) = weak.upgrade() {
                this.initial_start();
            }
        });
        this
    }

    // =========================================================================
    // wiring
    // =========================================================================

    fn connect_signals(
        self: &Rc<Self>,
        retry: &gtk::Button,
        qr_copy: &gtk::Button,
        qr_close: &gtk::Button,
        gallery_button: &gtk::Button,
    ) {
        let weak = Rc::downgrade(self);
        self.camera.connect_event(move |ev| {
            if let Some(this) = weak.upgrade() {
                this.on_camera_event(ev);
            }
        });
        let view = self.view.downgrade();
        self.camera.connect_paintable(move |p| {
            if let Some(v) = view.upgrade() {
                // Keep showing the last frame (blurred) until the new stream starts.
                if p.is_some() {
                    v.set_paintable(p.as_ref());
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.devices.connect_changed(move || {
            if let Some(this) = weak.upgrade() {
                this.on_devices_changed();
            }
        });
        let weak = Rc::downgrade(self);
        self.settings.connect_changed(move |key| {
            if let Some(this) = weak.upgrade() {
                this.on_setting_changed(key);
            }
        });
        retry.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.retry_camera()
        ));
        self.test_button.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.use_test_source()
        ));
        self.qr_open.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                if let Some(uri) = this.last_barcode.borrow().clone() {
                    gtk::UriLauncher::new(&uri).launch(
                        Some(&this.window),
                        gio::Cancellable::NONE,
                        |_| {},
                    );
                }
            }
        ));
        qr_copy.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                if let Some(text) = this.last_barcode.borrow().clone() {
                    this.window.clipboard().set_text(&text);
                    this.toast("Code copied", 1);
                }
            }
        ));
        qr_close.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.qr_dismiss()
        ));
        gallery_button.connect_clicked(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| this.open_latest()
        ));
        for (id, btn) in self.effect_buttons.borrow().iter() {
            let id: &'static str = id;
            btn.connect_toggled(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |b| {
                    if b.is_active() {
                        this.settings
                            .update("effect", |d| d.effect = id.to_string());
                        this.camera.set_effect(id);
                    }
                }
            ));
        }
        self.mode_group.connect_active_name_notify(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |g| {
                if let Some(name) = g.active_name()
                    && name != this.settings.get().capture_mode
                {
                    this.set_capture_mode(&name, false);
                }
            }
        ));
        self.split.connect_show_sidebar_notify(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |s| {
                if let Some(a) = this.action("controls") {
                    a.set_state(&s.shows_sidebar().to_variant());
                }
                if s.shows_sidebar() {
                    this.controls.refresh();
                }
            }
        ));
        self.camera_page.connect_showing(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                this.page_visible.set(true);
                if !this.camera.is_running() {
                    if this.current.borrow().is_some() {
                        this.restart_current();
                    } else {
                        this.initial_start();
                    }
                }
                let v = this.view.clone();
                glib::idle_add_local_once(move || {
                    v.grab_focus();
                });
            }
        ));
        self.camera_page.connect_hidden(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                this.page_visible.set(false);
                // A pending timer or burst must not fire into the gallery.
                this.cancel_countdown();
                this.stop_burst();
                if !this.camera.is_recording() {
                    // Release the camera (and the GPU/CPU) while browsing the gallery.
                    this.cancel_retry();
                    this.camera.stop();
                }
            }
        ));
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |controller, _, dy| {
                // Ctrl+wheel only: a plain wheel/touchpad scroll over the
                // picture zoomed in by accident and made everything soft.
                let ctrl = controller
                    .current_event_state()
                    .contains(gdk::ModifierType::CONTROL_MASK);
                if !ctrl {
                    return glib::Propagation::Proceed;
                }
                if dy != 0.0 {
                    this.set_zoom(this.camera.zoom() * if dy > 0.0 { 0.9 } else { 1.1 });
                }
                glib::Propagation::Stop
            }
        ));
        self.view.add_controller(scroll);
        let pinch = gtk::GestureZoom::new();
        pinch.connect_begin(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| this.pinch_base.set(this.camera.zoom())
        ));
        pinch.connect_scale_changed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, s| this.set_zoom(this.pinch_base.get() * s)
        ));
        self.view.add_controller(pinch);
        let click = gtk::GestureClick::new();
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_, n, _, _| {
                this.view.grab_focus();
                if n == 2 {
                    this.toggle_fullscreen();
                }
            }
        ));
        self.view.add_controller(click);
        self.gallery.connect_open(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |items, pos| {
                this.viewer.show_items(items, pos);
                this.nav.push(&this.viewer.page);
            }
        ));
        self.viewer.connect_deleted(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move || {
                this.gallery.refresh();
                this.update_gallery_button();
            }
        ));
        self.window.connect_close_request(glib::clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_| this.on_close_request()
        ));
    }

    fn action(&self, name: &str) -> Option<gio::SimpleAction> {
        self.window
            .lookup_action(name)
            .and_downcast::<gio::SimpleAction>()
    }

    fn setup_actions(self: &Rc<Self>) {
        let simple = |name: &str, f: fn(&Rc<Self>)| {
            let a = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(t) = weak.upgrade() {
                    f(&t);
                }
            });
            self.window.add_action(&a);
        };
        simple("shutter", |t| t.shutter_pressed());
        simple("snapshot", |t| t.take_photo(true, true));
        simple("escape", |t| t.on_escape());
        simple("switch-camera", |t| t.switch_camera());
        simple("zoom-in", |t| t.zoom_step(1));
        simple("zoom-out", |t| t.zoom_step(-1));
        simple("zoom-reset", |t| t.set_zoom(1.0));
        simple("fullscreen", |t| t.toggle_fullscreen());
        simple("gallery", |t| t.open_gallery());
        simple("preferences", |t| {
            preferences::show(&t.window, &t.settings, t.camera.accel())
        });
        simple("cycle-timer", |t| t.cycle_timer());
        simple("cycle-burst", |t| t.cycle_burst());

        let string_state = |name: &str, initial: String, f: fn(&Rc<Self>, &str)| {
            let a = gio::SimpleAction::new_stateful(
                name,
                Some(glib::VariantTy::STRING),
                &initial.to_variant(),
            );
            let weak = Rc::downgrade(self);
            a.connect_change_state(move |_, v| {
                if let (Some(t), Some(s)) = (weak.upgrade(), v.and_then(|v| v.get::<String>())) {
                    f(&t, &s);
                }
            });
            self.window.add_action(&a);
        };
        let s = self.settings.get().clone();
        string_state("capture-mode", s.capture_mode.clone(), |t, v| {
            t.set_capture_mode(v, false)
        });
        string_state("timer", s.timer.to_string(), |t, v| {
            let n = v.parse().unwrap_or(0);
            t.settings.update("timer", |d| d.timer = n);
        });
        string_state("burst", s.burst.to_string(), |t, v| {
            let n = v.parse().unwrap_or(1);
            t.settings.update("burst", |d| d.burst = n);
        });
        string_state("camera", String::new(), |t, key| {
            let cam = t.devices.find(Some(key));
            let same = t.current.borrow().as_ref().is_some_and(|c| c.key == key);
            if let Some(c) = cam
                && (!same || !t.camera.is_running())
            {
                t.start_camera(&c, None);
            }
        });
        string_state("resolution", String::new(), |t, key| {
            if t.camera.is_recording() {
                return;
            }
            let Some(cam) = t.current.borrow().clone() else {
                return;
            };
            if let Some(m) = cam.modes.iter().find(|m| m.key() == key).cloned()
                && t.mode.borrow().as_ref() != Some(&m)
            {
                let (k, mk) = (cam.key.clone(), m.key());
                t.settings.update_quiet(|d| {
                    d.modes.insert(k, mk);
                });
                t.start_camera(&cam, Some(m));
            }
        });
        let bool_state = |name: &str, initial: bool, f: fn(&Rc<Self>, bool)| {
            let a = gio::SimpleAction::new_stateful(name, None, &initial.to_variant());
            let weak = Rc::downgrade(self);
            a.connect_change_state(move |_, v| {
                if let (Some(t), Some(b)) = (weak.upgrade(), v.and_then(|v| v.get::<bool>())) {
                    f(&t, b);
                }
            });
            self.window.add_action(&a);
        };
        bool_state("grid", s.grid, |t, v| {
            t.settings.update("grid", |d| d.grid = v)
        });
        bool_state("mirror", s.mirror_preview, |t, v| {
            t.settings
                .update("mirror-preview", |d| d.mirror_preview = v)
        });
        bool_state("show-fps", s.show_fps, |t, v| {
            t.settings.update("show-fps", |d| d.show_fps = v)
        });
        bool_state("effects", false, |t, v| {
            t.action("effects").unwrap().set_state(&v.to_variant());
            t.effects_revealer.set_reveal_child(v);
            t.set_effect_previews_live(v);
        });
        bool_state("controls", false, |t, v| t.split.set_show_sidebar(v));
    }

    fn setup_shortcuts(self: &Rc<Self>) {
        let controller = gtk::ShortcutController::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let shortcuts: &[(&str, &str, Option<&str>)] = &[
            ("space", "win.shutter", None),
            ("Escape", "win.escape", None),
            ("p", "win.capture-mode", Some("photo")),
            ("v", "win.capture-mode", Some("video")),
            ("q", "win.capture-mode", Some("qr")),
            ("t", "win.cycle-timer", None),
            ("b", "win.cycle-burst", None),
            ("g", "win.grid", None),
            ("m", "win.mirror", None),
            ("e", "win.effects", None),
            ("k", "win.controls", None),
            ("c", "win.switch-camera", None),
            ("f", "win.fullscreen", None),
            ("plus|equal|KP_Add", "win.zoom-in", None),
            ("minus|KP_Subtract", "win.zoom-out", None),
            ("0|KP_0", "win.zoom-reset", None),
            ("F12", "win.show-fps", None),
        ];
        for &(trigger, action, target) in shortcuts {
            let weak = Rc::downgrade(self);
            let callback = gtk::CallbackAction::new(move |_, _| {
                let Some(this) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                // Single-key shortcuts must not steal keys from menus, the side panel or text fields.
                if let Some(focus) = gtk::prelude::GtkWindowExt::focus(&this.window)
                    && (focus.ancestor(gtk::Popover::static_type()).is_some()
                        || focus.is_ancestor(&this.controls.widget)
                        || focus.is::<gtk::Editable>())
                {
                    return glib::Propagation::Proceed;
                }
                let _ = gtk::prelude::WidgetExt::activate_action(
                    &this.window,
                    action,
                    target.map(|t| t.to_variant()).as_ref(),
                );
                glib::Propagation::Stop
            });
            controller.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(callback),
            ));
        }
        self.camera_page.add_controller(controller);
    }

    // =========================================================================
    // settings
    // =========================================================================

    fn apply_settings(self: &Rc<Self>) {
        let s = self.settings.get().clone();
        self.view.set_mirror(s.mirror_preview);
        self.view.set_grid(s.grid);
        self.view.set_fill(s.fill_frame);
        self.sounds.enabled.set(s.shutter_sound);
        self.update_timer_ui();
        self.update_burst_ui();
        let eff = effects::get(&s.effect).id;
        if let Some(b) = self.effect_buttons.borrow().get(eff) {
            b.set_active(true);
        }
        self.camera.set_effect(eff);
        self.set_capture_mode(&s.capture_mode, true);
        self.update_info();
        self.update_zoom_ui();
    }

    fn on_setting_changed(self: &Rc<Self>, key: &str) {
        let s = self.settings.get().clone();
        match key {
            "mirror-preview" => {
                self.view.set_mirror(s.mirror_preview);
                if let Some(a) = self.action("mirror") {
                    a.set_state(&s.mirror_preview.to_variant());
                }
            }
            "grid" => {
                self.view.set_grid(s.grid);
                if let Some(a) = self.action("grid") {
                    a.set_state(&s.grid.to_variant());
                }
            }
            "fill-frame" => self.view.set_fill(s.fill_frame),
            "shutter-sound" => self.sounds.enabled.set(s.shutter_sound),
            "show-fps" => {
                if let Some(a) = self.action("show-fps") {
                    a.set_state(&s.show_fps.to_variant());
                }
                self.update_info();
            }
            "timer" => self.update_timer_ui(),
            "burst" => self.update_burst_ui(),
            "photo-dir" | "video-dir" => self.update_gallery_button(),
            "hardware-acceleration" => {
                self.camera
                    .set_accel(camera::Accel::detect(s.hardware_acceleration));
                if !self.camera.is_recording() && self.camera.is_running() {
                    self.restart_current();
                }
            }
            _ => {}
        }
    }

    fn update_timer_ui(&self) {
        let sec = self.settings.get().timer;
        if let Some(a) = self.action("timer") {
            a.set_state(&sec.to_string().to_variant());
        }
        if sec > 0 {
            self.timer_button.set_label(&format!("{sec}s"));
            self.timer_button
                .set_tooltip_text(Some(&format!("Timer: {sec} s (T)")));
        } else {
            self.timer_button.set_icon_name("cs-timer-symbolic");
            self.timer_button.set_tooltip_text(Some("Timer: off (T)"));
        }
    }

    fn update_burst_ui(&self) {
        let n = self.settings.get().burst;
        if let Some(a) = self.action("burst") {
            a.set_state(&n.to_string().to_variant());
        }
        if n > 1 {
            self.burst_button.set_label(&format!("×{n}"));
            self.burst_button
                .set_tooltip_text(Some(&format!("Burst: {n} photos (B)")));
        } else {
            self.burst_button.set_icon_name("cs-burst-symbolic");
            self.burst_button.set_tooltip_text(Some("Single shot (B)"));
        }
    }

    fn cycle_timer(self: &Rc<Self>) {
        let cur = self.settings.get().timer;
        let next = TIMER_CHOICES
            [(TIMER_CHOICES.iter().position(|&v| v == cur).unwrap_or(0) + 1) % TIMER_CHOICES.len()];
        self.settings.update("timer", |d| d.timer = next);
        self.toast(
            &if next == 0 {
                "Timer off".into()
            } else {
                format!("Timer: {next} s")
            },
            1,
        );
    }

    fn cycle_burst(self: &Rc<Self>) {
        if self.settings.get().capture_mode != "photo" {
            return;
        }
        let cur = self.settings.get().burst;
        let next = BURST_CHOICES
            [(BURST_CHOICES.iter().position(|&v| v == cur).unwrap_or(0) + 1) % BURST_CHOICES.len()];
        self.settings.update("burst", |d| d.burst = next);
        self.toast(
            &if next == 1 {
                "Single shot".into()
            } else {
                format!("Burst: {next} photos")
            },
            1,
        );
    }

    // =========================================================================
    // camera management
    // =========================================================================

    fn initial_start(self: &Rc<Self>) {
        self.rebuild_camera_menu();
        // Opened to view files: leave the camera alone (not even its control
        // node) until the camera page is shown.
        if !self.page_visible.get() {
            return;
        }
        let key = self.settings.get().camera.clone();
        match self.devices.preferred(key.as_deref()) {
            Some(cam) => self.start_camera(&cam, None),
            None => self.show_status(
                "none",
                "Connect a webcam — it is picked up automatically when plugged in.",
            ),
        }
        self.view.grab_focus();
    }

    pub fn start_camera(self: &Rc<Self>, cam: &Camera, mode: Option<Mode>) {
        if self.camera.is_recording() {
            self.toast("Cannot switch cameras while recording", 3);
            return;
        }
        self.cancel_retry();
        self.cancel_countdown();
        self.stop_burst();
        let changed = self
            .current
            .borrow()
            .as_ref()
            .is_none_or(|c| c.key != cam.key);
        let mode = mode.or_else(|| {
            let saved = self.settings.get().modes.get(&cam.key).cloned();
            saved.and_then(|k| cam.modes.iter().find(|m| m.key() == k).cloned())
        });
        let Some(mode) = mode.or_else(|| devices::default_mode(&cam.modes)) else {
            return;
        };
        *self.current.borrow_mut() = Some(cam.clone());
        *self.mode.borrow_mut() = Some(mode.clone());
        if !cam.is_test() {
            let key = cam.key.clone();
            self.settings.update_quiet(|d| d.camera = Some(key));
        }
        self.rebuild_camera_menu();
        self.rebuild_resolution_menu();
        if changed || self.controls.camera_key().is_none() {
            self.controls.set_camera(Some(cam));
        }
        self.controls.apply_saved();
        self.hide_status();
        self.spinner.set_visible(true);
        self.view.set_blur(if self.view.paintable().is_some() {
            18.0
        } else {
            0.0
        });
        if changed {
            self.set_zoom(1.0);
        }
        if !self.page_visible.get() {
            return; // started when the camera page shows again
        }
        self.camera
            .set_qr_enabled(self.settings.get().capture_mode == "qr");
        self.camera.start(cam, &mode);
        self.update_info();
    }

    fn restart_current(self: &Rc<Self>) {
        let cam = self.current.borrow().clone();
        let mode = self.mode.borrow().clone();
        if let Some(c) = cam {
            self.start_camera(&c, mode);
        }
    }

    fn switch_camera(self: &Rc<Self>) {
        let cams = self.devices.cameras.borrow().clone();
        if cams.len() < 2 || self.camera.is_recording() {
            return;
        }
        let cur = self.current.borrow().as_ref().map(|c| c.key.clone());
        let idx = cams
            .iter()
            .position(|c| Some(&c.key) == cur.as_ref())
            .map(|i| i as i64)
            .unwrap_or(-1);
        let next = cams[((idx + 1) as usize) % cams.len()].clone();
        self.start_camera(&next, None);
    }

    pub fn use_test_source(self: &Rc<Self>) {
        self.devices.include_test.set(true);
        self.devices.refresh();
        let cam = self
            .devices
            .find(Some(devices::TEST_KEY))
            .unwrap_or_else(devices::test_camera);
        self.start_camera(&cam, None);
    }

    fn rebuild_camera_menu(&self) {
        let menu = gio::Menu::new();
        let cams = self.devices.cameras.borrow().clone();
        for c in &cams {
            let item = gio::MenuItem::new(Some(&c.name), None);
            item.set_action_and_target_value(Some("win.camera"), Some(&c.key.to_variant()));
            menu.append_item(&item);
        }
        if cams.is_empty() {
            menu.append(Some("No camera"), Some("win.none"));
        }
        self.camera_button.set_menu_model(Some(&menu));
        let current = self.current.borrow().clone();
        if let Some(a) = self.action("camera") {
            a.set_state(
                &current
                    .as_ref()
                    .map(|c| c.key.clone())
                    .unwrap_or_default()
                    .to_variant(),
            );
        }
        self.camera_label.set_label(
            current
                .as_ref()
                .map(|c| c.name.as_str())
                .unwrap_or("No camera"),
        );
        self.camera_button.set_tooltip_text(Some(
            &current
                .map(|c| format!("Camera: {}", c.name))
                .unwrap_or("Choose camera".into()),
        ));
        self.switch_button.set_visible(cams.len() > 1);
    }

    fn rebuild_resolution_menu(&self) {
        let menu = gio::Menu::new();
        if let Some(c) = self.current.borrow().as_ref() {
            for m in &c.modes {
                let item = gio::MenuItem::new(Some(&m.label()), None);
                item.set_action_and_target_value(
                    Some("win.resolution"),
                    Some(&m.key().to_variant()),
                );
                menu.append_item(&item);
            }
        }
        self.resolution_button.set_menu_model(Some(&menu));
        let mode = self.mode.borrow().clone();
        if let Some(a) = self.action("resolution") {
            a.set_state(
                &mode
                    .as_ref()
                    .map(|m| m.key())
                    .unwrap_or_default()
                    .to_variant(),
            );
        }
        self.resolution_button
            .set_label(&mode.as_ref().map(|m| m.short_label()).unwrap_or("—".into()));
        self.resolution_button.set_tooltip_text(Some(
            &mode
                .map(|m| format!("Resolution: {}", m.label()))
                .unwrap_or("Resolution".into()),
        ));
    }

    fn on_devices_changed(self: &Rc<Self>) {
        self.rebuild_camera_menu();
        let current = self.current.borrow().clone();
        match current {
            Some(cam) if !cam.is_test() => match self.devices.find(Some(&cam.key)) {
                None => {
                    if self.camera.is_recording() {
                        self.stop_recording();
                    }
                    self.camera.stop();
                    *self.current.borrow_mut() = None;
                    if let Some(other) = self.devices.preferred(None) {
                        self.toast("Camera disconnected — switched", 3);
                        self.start_camera(&other, None);
                    } else {
                        self.show_status(
                            "disconnected",
                            "Plug the camera back in — it reconnects automatically.",
                        );
                    }
                }
                Some(fresh) => {
                    *self.current.borrow_mut() = Some(fresh);
                    if !self.camera.is_running() && self.page_visible.get() {
                        self.restart_current();
                    }
                }
            },
            Some(_) => {}
            None => {
                let key = self.settings.get().camera.clone();
                if let Some(cam) = self.devices.preferred(key.as_deref())
                    && self.page_visible.get()
                {
                    self.start_camera(&cam, None);
                }
            }
        }
    }

    fn on_camera_event(self: &Rc<Self>, ev: Event) {
        match ev {
            Event::Started => {
                self.spinner.set_visible(false);
                self.view.set_blur(0.0);
                self.hide_status();
                self.update_info();
            }
            Event::Error(kind, message) => {
                self.spinner.set_visible(false);
                self.cancel_countdown();
                self.stop_burst();
                let k = match kind {
                    ErrorKind::Busy => "busy",
                    ErrorKind::Permission => "permission",
                    ErrorKind::Disconnected => "disconnected",
                    ErrorKind::Timeout => "timeout",
                    ErrorKind::Other => "other",
                };
                self.show_status(k, &message);
                if matches!(
                    kind,
                    ErrorKind::Busy | ErrorKind::Disconnected | ErrorKind::Timeout
                ) && self.current.borrow().is_some()
                {
                    self.schedule_retry();
                }
            }
            Event::Barcode(kind, data) => self.on_barcode(&kind, &data),
            Event::Photo(sample) => self.on_photo_sample(sample),
            Event::EffectSource(sample) => self.render_effect_previews(sample),
            Event::RecordingFinished { path, ok, message } => {
                self.on_recording_finished(path, ok, message)
            }
        }
    }

    fn schedule_retry(self: &Rc<Self>) {
        self.cancel_retry();
        let weak = Rc::downgrade(self);
        *self.retry_source.borrow_mut() = Some(glib::timeout_add_local_once(
            Duration::from_secs(3),
            move || {
                let Some(this) = weak.upgrade() else { return };
                this.retry_source.borrow_mut().take();
                if this.page_visible.get() && !this.camera.is_running() {
                    let cam = this.current.borrow().clone();
                    let mode = this.mode.borrow().clone();
                    if let (Some(c), Some(m)) = (cam, mode) {
                        this.camera
                            .set_qr_enabled(this.settings.get().capture_mode == "qr");
                        this.camera.start(&c, &m);
                    }
                }
            },
        ));
    }

    fn cancel_retry(&self) {
        if let Some(id) = self.retry_source.borrow_mut().take() {
            id.remove();
        }
    }

    fn retry_camera(self: &Rc<Self>) {
        if self.current.borrow().is_none() {
            let key = self.settings.get().camera.clone();
            match self.devices.preferred(key.as_deref()) {
                Some(c) => self.start_camera(&c, None),
                None => self.toast("Still no camera found", 3),
            }
        } else {
            self.restart_current();
        }
    }

    fn show_status(&self, kind: &str, message: &str) {
        let (icon, title) = match kind {
            "busy" => ("camera-disabled-symbolic", "Camera Busy"),
            "permission" => ("dialog-warning-symbolic", "No Permission"),
            "disconnected" => ("camera-disabled-symbolic", "Camera Disconnected"),
            "timeout" => ("dialog-warning-symbolic", "Camera Not Responding"),
            "none" => ("camera-disabled-symbolic", "No Camera Found"),
            _ => ("dialog-warning-symbolic", "Cannot Open Camera"),
        };
        self.status_page.set_icon_name(Some(icon));
        self.status_page.set_title(title);
        let retrying =
            matches!(kind, "busy" | "disconnected" | "timeout") && self.current.borrow().is_some();
        let text = if retrying {
            format!("{message}\nRetrying automatically…")
        } else {
            message.to_string()
        };
        self.status_page
            .set_description(Some(&glib::markup_escape_text(&text)));
        self.test_button.set_visible(kind == "none");
        self.status_box.set_visible(true);
        self.spinner.set_visible(false);
    }

    fn hide_status(&self) {
        self.status_box.set_visible(false);
    }

    fn update_info(self: &Rc<Self>) {
        let show = self.settings.get().show_fps;
        self.info.set_visible(show);
        if show && self.info_source.borrow().is_none() {
            let weak = Rc::downgrade(self);
            *self.info_source.borrow_mut() = Some(glib::timeout_add_local(
                Duration::from_millis(500),
                move || {
                    let Some(this) = weak.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    if !this.settings.get().show_fps {
                        this.info_source.borrow_mut().take();
                        return glib::ControlFlow::Break;
                    }
                    this.update_info_text();
                    glib::ControlFlow::Continue
                },
            ));
            self.update_info_text();
        } else if !show && let Some(id) = self.info_source.borrow_mut().take() {
            id.remove();
        }
    }

    fn update_info_text(&self) {
        let mut parts = vec![format!("{:.1} fps", self.camera.fps())];
        if let Some(m) = self.mode.borrow().as_ref() {
            parts.push(format!("{}×{}", m.width, m.height));
            let decode = if m.is_mjpeg() {
                if !self.camera.is_active() {
                    "MJPEG"
                } else if self.camera.uses_hw_decode() {
                    "MJPEG · GPU"
                } else {
                    "MJPEG · CPU"
                }
            } else {
                m.format.as_deref().unwrap_or("raw")
            };
            parts.push(decode.to_string());
        }
        if self.camera.zoom() > 1.0 {
            parts.push(format!("zoom {:.2}×", self.camera.zoom()));
        }
        self.info.set_label(&parts.join(" · "));
    }

    // =========================================================================
    // capture
    // =========================================================================

    fn shutter_pressed(self: &Rc<Self>) {
        let mode = self.settings.get().capture_mode.clone();
        if self.countdown_source.borrow().is_some() {
            self.cancel_countdown();
            self.toast("Timer cancelled", 1);
            return;
        }
        if mode == "qr" {
            self.qr_dismiss();
            return;
        }
        if mode == "video" && self.camera.is_recording() {
            self.stop_recording();
            return;
        }
        if !self.camera.is_active() {
            self.toast("Camera not ready", 2);
            return;
        }
        let then = if mode == "video" {
            Pending::Recording
        } else {
            Pending::Photos
        };
        let delay = self.settings.get().timer;
        if delay > 0 {
            self.start_countdown(delay, then);
        } else {
            self.run_pending(then);
        }
    }

    fn run_pending(self: &Rc<Self>, what: Pending) {
        match what {
            Pending::Recording => self.start_recording(),
            Pending::Photos => {
                let n = self.settings.get().burst.max(1);
                self.take_photo(true, true);
                if n > 1 {
                    self.burst_left.set(n - 1);
                    let weak = Rc::downgrade(self);
                    *self.burst_source.borrow_mut() = Some(glib::timeout_add_local(
                        Duration::from_millis(150),
                        move || {
                            let Some(this) = weak.upgrade() else {
                                return glib::ControlFlow::Break;
                            };
                            if this.burst_left.get() == 0 {
                                this.burst_source.borrow_mut().take();
                                return glib::ControlFlow::Break;
                            }
                            this.burst_left.set(this.burst_left.get() - 1);
                            this.take_photo(true, false);
                            glib::ControlFlow::Continue
                        },
                    ));
                }
            }
        }
    }

    pub fn take_photo(self: &Rc<Self>, sound: bool, flash: bool) {
        // The file name is reserved only when the frame arrives (on_photo_sample):
        // requests lost to a camera restart then leave nothing behind and can
        // never shift later photos onto the wrong name.
        if !self.camera.request_photos(1) {
            self.stop_burst();
            self.toast("Camera not ready", 2);
            return;
        }
        if sound {
            self.sounds.play("shutter");
        }
        if flash {
            self.flash();
        }
    }

    fn on_photo_sample(self: &Rc<Self>, sample: gst::Sample) {
        let path = match storage::unique_path(&storage::photo_dir(&self.settings), "IMG", "jpg") {
            Ok(p) => p,
            Err(e) => {
                self.toast(&format!("Could not save the photo: {e}"), 3);
                return;
            }
        };
        if let Some(tex) = photo::sample_to_texture(&sample) {
            self.set_gallery_thumb(Some(tex.upcast_ref()));
        }
        let (quality, mirror, name) = {
            let s = self.settings.get();
            (
                s.jpeg_quality,
                s.mirror_output && s.mirror_preview,
                self.current.borrow().as_ref().map(|c| c.name.clone()),
            )
        };
        self.photos_in_flight.set(self.photos_in_flight.get() + 1);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let p = path.clone();
            let result = gio::spawn_blocking(move || {
                photo::encode_jpeg(&sample, &p, quality, mirror, name.as_deref())
                    .map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|_| Err("encoder thread failed".into()));
            this.photos_in_flight.set(this.photos_in_flight.get() - 1);
            match result {
                Ok(()) => {
                    this.thumbs.forget(&path);
                    if this.burst_left.get() == 0 {
                        let p = path.clone();
                        this.toast_action("Photo saved", "View", 2, move |t| t.open_media(&p));
                    }
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&path);
                    this.toast(&format!("Could not save the photo: {e}"), 3);
                    this.update_gallery_button();
                }
            }
            if this.close_when_saved.get()
                && this.photos_in_flight.get() == 0
                && !this.camera.is_recording()
            {
                this.window.close();
            }
        });
    }

    fn flash(&self) {
        let target = adw::PropertyAnimationTarget::new(&self.flash, "opacity");
        let anim = adw::TimedAnimation::new(&self.flash, 0.85, 0.0, 280, target);
        anim.set_easing(adw::Easing::EaseOutCubic);
        anim.play();
    }

    fn start_countdown(self: &Rc<Self>, seconds: u32, then: Pending) {
        self.countdown.set(seconds);
        self.countdown_then.set(then);
        self.countdown_label.set_label(&seconds.to_string());
        self.countdown_box.set_visible(true);
        self.shutter.add_css_class("countdown");
        self.sounds.play("tick");
        let weak = Rc::downgrade(self);
        *self.countdown_source.borrow_mut() =
            Some(glib::timeout_add_local(Duration::from_secs(1), move || {
                let Some(this) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let left = this.countdown.get().saturating_sub(1);
                this.countdown.set(left);
                if left == 0 {
                    this.countdown_source.borrow_mut().take();
                    this.countdown_box.set_visible(false);
                    this.shutter.remove_css_class("countdown");
                    if this.camera.is_active() {
                        this.run_pending(this.countdown_then.get());
                    }
                    return glib::ControlFlow::Break;
                }
                this.countdown_label.set_label(&left.to_string());
                this.sounds.play("tick");
                glib::ControlFlow::Continue
            }));
    }

    fn cancel_countdown(&self) {
        if let Some(id) = self.countdown_source.borrow_mut().take() {
            id.remove();
        }
        self.countdown_box.set_visible(false);
        self.shutter.remove_css_class("countdown");
    }

    fn stop_burst(&self) {
        self.burst_left.set(0);
        if let Some(id) = self.burst_source.borrow_mut().take() {
            id.remove();
        }
    }

    // --- video -------------------------------------------------------------------------

    pub fn start_recording(self: &Rc<Self>) {
        if self.camera.is_recording() {
            return;
        }
        let s = self.settings.get().clone();
        let container = if s.video_format == "webm" {
            "webm"
        } else {
            "mp4"
        };
        // Test hook (see selftest.rs): record to a given path, e.g. /dev/full.
        let forced = std::env::var_os("CAMERA_STUDIO_RECORD_TO").map(PathBuf::from);
        let path = match forced.map(Ok).unwrap_or_else(|| {
            storage::unique_path(&storage::video_dir(&self.settings), "VID", container)
        }) {
            Ok(p) => p,
            Err(e) => {
                self.toast(&format!("Could not create the video file: {e}"), 3);
                return;
            }
        };
        let mut audio = s.record_audio;
        if audio && !camera::has_microphone() {
            audio = false;
            self.toast("No microphone — recording without sound", 3);
        }
        let opts = RecordOptions {
            path: path.clone(),
            container: container.into(),
            quality: s.video_quality.clone(),
            audio,
            audio_device: s.audio_device.clone(),
            mirror: s.mirror_output && s.mirror_preview,
        };
        if let Err(e) = self.camera.start_recording(&opts) {
            let _ = std::fs::remove_file(&path);
            self.toast(&format!("{e:#}"), 4);
            return;
        }
        self.sounds.play("record-start");
        self.set_recording_ui(true);
    }

    pub fn stop_recording(self: &Rc<Self>) {
        if !self.camera.is_recording() || self.recording_saving.get() {
            return;
        }
        self.recording_saving.set(true);
        self.sounds.play("record-stop");
        self.rec_indicator.add_css_class("saving");
        self.rec_label.set_label("Saving…");
        self.camera.stop_recording();
    }

    fn on_recording_finished(self: &Rc<Self>, path: PathBuf, ok: bool, message: String) {
        self.recording_saving.set(false);
        self.set_recording_ui(false);
        if ok {
            self.thumbs.forget(&path);
            let p = path.clone();
            self.toast_action("Video saved", "View", 3, move |t| t.open_media(&p));
        } else {
            // Only ever delete our own empty regular file.
            if std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() == 0) {
                let _ = std::fs::remove_file(&path);
            }
            self.toast(
                if message.is_empty() {
                    "Could not save the video"
                } else {
                    &message
                },
                4,
            );
        }
        self.update_gallery_button();
        if self.close_when_saved.get() {
            self.window.close();
        } else if !self.page_visible.get() {
            self.camera.stop();
        }
    }

    fn set_recording_ui(self: &Rc<Self>, recording: bool) {
        self.rec_indicator.set_visible(recording);
        self.rec_indicator.remove_css_class("saving");
        self.shutter
            .remove_css_class(if recording { "video" } else { "recording" });
        self.shutter
            .add_css_class(if recording { "recording" } else { "video" });
        self.shutter.set_tooltip_text(Some(if recording {
            "Stop recording (Space)"
        } else {
            "Start recording (Space)"
        }));
        self.snapshot_button.set_visible(recording);
        for w in [
            self.mode_group.upcast_ref::<gtk::Widget>(),
            self.camera_button.upcast_ref(),
            self.resolution_button.upcast_ref(),
            self.switch_button.upcast_ref(),
            self.timer_button.upcast_ref(),
        ] {
            w.set_sensitive(!recording);
        }
        if recording {
            self.rec_label.set_label("00:00");
            let weak = Rc::downgrade(self);
            *self.rec_tick.borrow_mut() = Some(glib::timeout_add_local(
                Duration::from_millis(250),
                move || {
                    let Some(this) = weak.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    let Some(elapsed) = this.camera.recording_elapsed() else {
                        this.rec_tick.borrow_mut().take();
                        return glib::ControlFlow::Break;
                    };
                    let secs = elapsed.as_secs_f64();
                    this.rec_label.set_label(&storage::format_duration(secs));
                    this.rec_dot
                        .set_opacity(if ((secs * 2.0) as u64).is_multiple_of(2) {
                            1.0
                        } else {
                            0.25
                        });
                    glib::ControlFlow::Continue
                },
            ));
        } else if let Some(id) = self.rec_tick.borrow_mut().take() {
            id.remove();
        }
    }

    // --- modes -----------------------------------------------------------------------------

    pub fn set_capture_mode(self: &Rc<Self>, mode: &str, initial: bool) {
        let mode = if MODES.iter().any(|(m, _)| *m == mode) {
            mode
        } else {
            "photo"
        };
        if self.camera.is_recording() && !initial {
            let cur = self.settings.get().capture_mode.clone();
            self.mode_group.set_active_name(Some(&cur));
            return;
        }
        self.cancel_countdown();
        self.stop_burst();
        self.settings
            .update("capture-mode", |d| d.capture_mode = mode.to_string());
        if let Some(a) = self.action("capture-mode") {
            a.set_state(&mode.to_variant());
        }
        if self.mode_group.active_name().as_deref() != Some(mode) {
            self.mode_group.set_active_name(Some(mode));
        }
        for c in ["video", "qr", "recording"] {
            self.shutter.remove_css_class(c);
        }
        if mode != "photo" {
            self.shutter.add_css_class(mode);
        }
        self.shutter.set_tooltip_text(Some(match mode {
            "video" => "Start recording (Space)",
            "qr" => "Scan again (Space)",
            _ => "Take photo (Space)",
        }));
        self.burst_button.set_visible(mode == "photo");
        self.timer_button.set_visible(mode != "qr");
        self.view.set_qr_guide(mode == "qr");
        self.camera.set_qr_enabled(mode == "qr");
        if mode != "qr" {
            self.qr_dismiss();
        }
    }

    fn on_barcode(&self, kind: &str, data: &str) {
        if self.settings.get().capture_mode != "qr" || data.is_empty() {
            return;
        }
        if self.last_barcode.borrow().as_deref() == Some(data) && self.qr_revealer.reveals_child() {
            return;
        }
        *self.last_barcode.borrow_mut() = Some(data.to_string());
        self.qr_type.set_label(match kind {
            "QR-Code" => "QR Code",
            "EAN-13" => "EAN-13 barcode",
            other => other,
        });
        self.qr_text.set_label(data);
        let lower = data.to_lowercase();
        self.qr_open.set_visible(
            ["http://", "https://", "mailto:", "tel:", "geo:", "sms:"]
                .iter()
                .any(|p| lower.starts_with(p)),
        );
        self.qr_revealer.set_reveal_child(true);
        self.sounds.play("detected");
    }

    fn qr_dismiss(&self) {
        self.qr_revealer.set_reveal_child(false);
        *self.last_barcode.borrow_mut() = None;
    }

    // --- zoom ---------------------------------------------------------------------------------

    fn zoom_step(self: &Rc<Self>, dir: i32) {
        let cur = self.camera.zoom();
        let next = if dir > 0 {
            ZOOM_STEPS
                .iter()
                .copied()
                .find(|z| *z > cur + 1e-3)
                .unwrap_or(8.0)
        } else {
            ZOOM_STEPS
                .iter()
                .rev()
                .copied()
                .find(|z| *z < cur - 1e-3)
                .unwrap_or(1.0)
        };
        self.set_zoom(next);
    }

    pub fn set_zoom(&self, zoom: f64) {
        let zoom = zoom.clamp(1.0, 8.0);
        if (zoom - self.camera.zoom()).abs() < 1e-3 {
            return;
        }
        self.camera.set_zoom(zoom);
        self.update_zoom_ui();
    }

    fn update_zoom_ui(&self) {
        let z = self.camera.zoom();
        let label = if (z - z.round()).abs() < 0.05 {
            format!("{z:.0}×")
        } else {
            format!("{z:.1}×")
        };
        self.zoom_label.set_label(&label);
        // Make an active digital zoom obvious: it softens photos and videos.
        if z > 1.001 {
            self.zoom_label.add_css_class("zoomed");
            self.zoom_label
                .set_tooltip_text(Some("Digital zoom on — click to reset to 1× (0)"));
        } else {
            self.zoom_label.remove_css_class("zoomed");
            self.zoom_label
                .set_tooltip_text(Some("Zoom: Ctrl + scroll, pinch or +/−"));
        }
    }

    fn on_escape(self: &Rc<Self>) {
        if self.countdown_source.borrow().is_some() {
            self.cancel_countdown();
        } else if self.burst_left.get() > 0 {
            self.stop_burst();
        } else if self.qr_revealer.reveals_child() {
            self.qr_dismiss();
        } else if self.effects_revealer.reveals_child() {
            let _ = gtk::prelude::WidgetExt::activate_action(&self.window, "win.effects", None);
        } else if self.split.shows_sidebar() {
            self.split.set_show_sidebar(false);
        } else if self.window.is_fullscreen() {
            self.window.unfullscreen();
        }
    }

    fn toggle_fullscreen(&self) {
        if self.window.is_fullscreen() {
            self.window.unfullscreen();
        } else {
            self.window.fullscreen();
        }
    }

    // =========================================================================
    // gallery
    // =========================================================================

    fn media_dirs(&self) -> Vec<PathBuf> {
        vec![
            storage::photo_dir(&self.settings),
            storage::video_dir(&self.settings),
        ]
    }

    pub fn update_gallery_button(self: &Rc<Self>) {
        let media = storage::list_media(&self.media_dirs());
        let Some(latest) = media.first().map(|m| m.path.clone()) else {
            self.set_gallery_thumb(None);
            return;
        };
        let thumb = self.gallery_thumb.clone();
        self.thumbs.request(&latest, move |t: &Thumb| {
            thumb.set_paintable(t.texture.as_ref().map(|t| t.upcast_ref()));
        });
    }

    pub fn open_gallery(self: &Rc<Self>) {
        if self.nav.visible_page().as_ref() != Some(&self.camera_page) {
            return;
        }
        self.gallery.refresh();
        self.nav.push(&self.gallery.page);
    }

    pub fn open_latest(self: &Rc<Self>) {
        self.gallery.refresh();
        let items = self.gallery.items();
        if items.is_empty() {
            self.nav.push(&self.gallery.page);
            return;
        }
        self.viewer.show_items(items, 0);
        self.show_viewer();
    }

    /// Open media files handed over by the desktop (e.g. "Open With" in Files).
    ///
    /// The viewer opens straight away and starts playing; the camera is not
    /// turned on until the user goes back to the camera page. With a single
    /// file, the other photos and videos of its folder can be browsed too.
    pub fn open_files(self: &Rc<Self>, files: &[gio::File]) {
        let paths: Vec<PathBuf> = files.iter().filter_map(|f| f.path()).collect();
        let as_media = |p: &PathBuf| -> Option<storage::MediaFile> {
            let meta = std::fs::metadata(p).ok()?;
            Some(storage::MediaFile {
                path: p.clone(),
                is_video: storage::classify(p)?,
                mtime: meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                size: meta.len(),
            })
        };
        let (items, index) = match paths.as_slice() {
            [] => return,
            [single] => {
                let siblings = single
                    .parent()
                    .map(|d| storage::list_media(&[d.to_path_buf()]))
                    .unwrap_or_default();
                match siblings.iter().position(|m| m.path == *single) {
                    Some(i) => (siblings, i),
                    None => (as_media(single).into_iter().collect(), 0),
                }
            }
            many => (many.iter().filter_map(as_media).collect(), 0),
        };
        if items.is_empty() {
            self.toast("This file type is not supported", 3);
            return;
        }
        // Mark the camera page hidden *before* the deferred camera start runs.
        self.page_visible.set(false);
        self.viewer.show_items(items, index);
        if self.viewer.page.parent().is_none() {
            self.nav
                .replace(&[self.camera_page.clone(), self.viewer.page.clone()]);
        }
        self.viewer.play_current();
    }

    /// Show the viewer with the gallery below it (so "back" lands in the gallery).
    fn show_viewer(&self) {
        if self.viewer.page.parent().is_some() {
            return; // already visible
        }
        if self.gallery.page.parent().is_some() {
            self.nav.pop_to_page(&self.gallery.page);
            self.nav.push(&self.viewer.page);
        } else {
            self.nav.replace(&[
                self.camera_page.clone(),
                self.gallery.page.clone(),
                self.viewer.page.clone(),
            ]);
        }
    }

    fn open_media(self: &Rc<Self>, path: &std::path::Path) {
        self.gallery.kind.set_active_name(Some("all"));
        self.gallery.refresh();
        let items = self.gallery.items();
        let Some(index) = items.iter().position(|m| m.path == path) else {
            return;
        };
        self.viewer.show_items(items, index);
        self.show_viewer();
    }

    /// Camera / acceleration / storage part of the About → Troubleshooting report.
    pub fn debug_report(&self) -> Vec<String> {
        let yes = |b: bool| if b { "yes" } else { "no" };
        let accel = self.camera.accel();
        let s = self.settings.get().clone();
        let renderer = self
            .window
            .renderer()
            .map(|r| r.type_().name().to_string())
            .unwrap_or_else(|| "-".into());
        let mut out = vec![
            "Acceleration".to_string(),
            format!(
                "- VA-API: JPEG decoding {}, H.264 encoding {}, VP9 encoding {} (setting: {})",
                yes(accel.va_decode),
                yes(accel.va_h264),
                yes(accel.va_vp9),
                if s.hardware_acceleration { "on" } else { "off" }
            ),
            format!(
                "- Display: gtk4paintablesink {}, renderer {renderer}",
                yes(camera::has_element("gtk4paintablesink"))
            ),
            String::new(),
            "Camera".to_string(),
        ];
        for c in self.devices.cameras.borrow().iter() {
            let largest = c.modes.first().map(|m| m.label()).unwrap_or_default();
            out.push(format!(
                "- {} [{}] — {} modes, largest {largest}",
                c.name,
                c.path.as_deref().unwrap_or("test"),
                c.modes.len()
            ));
        }
        match (self.current.borrow().as_ref(), self.mode.borrow().as_ref()) {
            (Some(c), Some(m)) if self.camera.is_active() => {
                let decode = if !m.is_mjpeg() {
                    m.format.clone().unwrap_or_else(|| "raw".into())
                } else if self.camera.uses_hw_decode() {
                    "MJPEG, GPU decoding".into()
                } else {
                    "MJPEG, CPU decoding".into()
                };
                out.push(format!(
                    "- In use: {} · {} · {decode} · {:.1} fps",
                    c.name,
                    m.label(),
                    self.camera.fps()
                ));
                let bad = self.camera.check_full_resolution();
                out.push(format!(
                    "- Processing resolution: {}",
                    if bad.is_empty() {
                        "full".to_string()
                    } else {
                        bad.join("; ")
                    }
                ));
            }
            (Some(c), _) => out.push(format!("- Selected: {} (no picture yet)", c.name)),
            _ => out.push("- No camera selected".into()),
        }
        out.push(String::new());
        out.push("Storage".into());
        out.push(format!(
            "- Photos: {}",
            storage::photo_dir(&self.settings).display()
        ));
        out.push(format!(
            "- Videos: {}",
            storage::video_dir(&self.settings).display()
        ));
        out.push(format!(
            "- Settings: {}",
            crate::settings::config_dir()
                .join("settings.json")
                .display()
        ));
        out
    }

    pub fn view_paintable(&self) -> Option<gdk::Paintable> {
        self.view.paintable()
    }

    fn set_gallery_thumb(&self, paintable: Option<&gdk::Paintable>) {
        self.gallery_thumb.set_paintable(paintable);
        self.gallery_placeholder.set_visible(paintable.is_none());
    }

    // =========================================================================
    // effect previews
    // =========================================================================

    /// While the effects strip is open, refresh its previews every 2 s.
    fn set_effect_previews_live(self: &Rc<Self>, live: bool) {
        if let Some(id) = self.effects_refresh.borrow_mut().take() {
            id.remove();
        }
        if !live {
            return;
        }
        self.camera.request_effect_source();
        let weak = Rc::downgrade(self);
        *self.effects_refresh.borrow_mut() =
            Some(glib::timeout_add_local(Duration::from_secs(2), move || {
                let Some(this) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if !this.previews_busy.get() {
                    this.camera.request_effect_source();
                }
                glib::ControlFlow::Continue
            }));
    }

    fn render_effect_previews(self: &Rc<Self>, sample: gst::Sample) {
        if self.previews_busy.replace(true) {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || {
                effects::render_previews(&sample).map_err(|e| e.to_string())
            })
            .await;
            this.previews_busy.set(false);
            match result {
                Ok(Ok(previews)) => {
                    for p in previews {
                        if let Some(picture) = this.effect_pictures.get(p.id) {
                            let texture = gdk::MemoryTexture::new(
                                p.width,
                                p.height,
                                gdk::MemoryFormat::R8g8b8a8,
                                &glib::Bytes::from_owned(p.pixels),
                                p.stride,
                            );
                            picture.set_paintable(Some(texture.upcast_ref()));
                        }
                    }
                }
                Ok(Err(e)) => {
                    eprintln!("camera-studio: cannot render the effect previews: {e}")
                }
                Err(_) => {}
            }
        });
    }

    // =========================================================================
    // misc
    // =========================================================================

    /// Toasts go inside the picture on the camera page (never over the
    /// shutter) and window-wide on the gallery pages.
    fn toast_target(&self) -> &adw::ToastOverlay {
        if self.page_visible.get() {
            &self.camera_toasts
        } else {
            &self.toasts
        }
    }

    /// Toast titles never wrap, so a long one would not fit a narrow window:
    /// long messages show their beginning and a "Details" button instead.
    pub fn toast(&self, text: &str, timeout: u32) {
        const MAX: usize = 42;
        let toast = adw::Toast::builder().timeout(timeout).build();
        if text.chars().count() <= MAX {
            toast.set_title(&glib::markup_escape_text(text));
        } else {
            let head: String = text.chars().take(MAX - 1).collect();
            toast.set_title(&glib::markup_escape_text(&format!("{}…", head.trim_end())));
            toast.set_button_label(Some("Details"));
            let full = text.to_string();
            let parent = self.window.downgrade();
            toast.connect_button_clicked(move |_| {
                let dialog = adw::AlertDialog::new(Some("Message"), Some(&full));
                dialog.add_response("ok", "Close");
                dialog.present(parent.upgrade().as_ref());
            });
        }
        self.toast_target().add_toast(toast);
    }

    fn toast_action(
        self: &Rc<Self>,
        text: &str,
        label: &str,
        timeout: u32,
        f: impl Fn(&Rc<Self>) + 'static,
    ) {
        let toast = adw::Toast::builder()
            .title(glib::markup_escape_text(text))
            .timeout(timeout)
            .button_label(label)
            .build();
        let weak = Rc::downgrade(self);
        toast.connect_button_clicked(move |_| {
            if let Some(t) = weak.upgrade() {
                f(&t);
            }
        });
        self.toast_target().add_toast(toast);
    }

    fn on_close_request(self: &Rc<Self>) -> glib::Propagation {
        if self.camera.is_recording() {
            self.close_when_saved.set(true);
            self.stop_recording();
            self.toast("Saving the video before quitting…", 3);
            return glib::Propagation::Stop;
        }
        if self.photos_in_flight.get() > 0 {
            self.close_when_saved.set(true);
            return glib::Propagation::Stop;
        }
        let maximized = self.window.is_maximized();
        let (w, h) = (self.window.width(), self.window.height());
        let fullscreen = self.window.is_fullscreen();
        self.settings.update_quiet(|d| {
            if !maximized && !fullscreen && w > 0 && h > 0 {
                d.window_width = w;
                d.window_height = h;
            }
            d.window_maximized = maximized;
        });
        self.cancel_retry();
        self.cancel_countdown();
        self.stop_burst();
        self.camera.stop();
        self.viewer.player.stop();
        self.controls.set_camera(None);
        self.sounds.shutdown();
        self.settings.save();
        glib::Propagation::Proceed
    }
}
