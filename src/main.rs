//! Camera Studio — a fast, GPU-accelerated camera app for the Linux desktop.

mod about;
mod camera;
mod controls;
mod devices;
mod effects;
mod gallery;
mod photo;
mod player;
mod preferences;
mod selftest;
mod settings;
mod sounds;
mod storage;
mod thumbnails;
mod v4l2;
mod viewfinder;
mod window;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

pub const APP_ID: &str = "io.github.hsthe29.CameraStudio";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Developer modes that drive the real UI (see selftest.rs).
#[derive(Clone, Copy)]
enum DevMode {
    SelfTest,
    Bench,
    Probe,
    RecordTest,
    UiTour,
}

struct Options {
    test_source: bool,
    dev: Option<(DevMode, PathBuf)>,
}

fn parse_args() -> Result<Options, glib::ExitCode> {
    let mut opts = Options {
        test_source: false,
        dev: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" => {
                println!("Camera Studio {VERSION}");
                return Err(glib::ExitCode::SUCCESS);
            }
            "-t" | "--test-source" => opts.test_source = true,
            "--self-test" | "--bench" | "--probe" | "--record-test" | "--ui-tour" => {
                let mode = match arg.as_str() {
                    "--self-test" => DevMode::SelfTest,
                    "--bench" => DevMode::Bench,
                    "--probe" => DevMode::Probe,
                    "--ui-tour" => DevMode::UiTour,
                    _ => DevMode::RecordTest,
                };
                let Some(dir) = args.next() else {
                    eprintln!("{arg} needs a directory");
                    return Err(glib::ExitCode::FAILURE);
                };
                opts.dev = Some((mode, PathBuf::from(dir)));
            }
            "-h" | "--help" => {
                println!(
                    "Usage: camera-studio [OPTION]\n\n  -t, --test-source   Add a test video source (no camera needed)\n      --version       Show the version\n  -h, --help          Show this help"
                );
                return Err(glib::ExitCode::SUCCESS);
            }
            other => {
                eprintln!("Unknown option: {other} (see --help)");
                return Err(glib::ExitCode::FAILURE);
            }
        }
    }
    Ok(opts)
}

/// Icons and other data: next to an installed binary, else the source tree.
fn data_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(prefix) = exe.parent().and_then(|p| p.parent())
    {
        let installed = prefix.join("share").join("camera-studio");
        if installed.exists() {
            return installed;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data")
}

fn main() -> glib::ExitCode {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(code) => return code,
    };
    if let Some((_, dir)) = &opts.dev {
        selftest::prepare_environment(dir);
    }
    if let Err(e) = gst::init() {
        eprintln!("Could not initialize GStreamer: {e}");
        return glib::ExitCode::FAILURE;
    }

    let app = adw::Application::builder().application_id(APP_ID).build();
    let window: Rc<RefCell<Option<Rc<window::Window>>>> = Rc::new(RefCell::new(None));
    let settings = Rc::new(RefCell::new(None::<settings::Settings>));
    let devices = Rc::new(RefCell::new(None::<Rc<devices::DeviceManager>>));

    app.connect_startup({
        let settings = settings.clone();
        let window_for_about = window.clone();
        move |app| {
            *settings.borrow_mut() = Some(settings::Settings::load());
            let display = gdk::Display::default().expect("no display");
            gtk::IconTheme::for_display(&display).add_search_path(data_dir().join("icons"));
            let provider = gtk::CssProvider::new();
            provider.load_from_string(include_str!("../data/style.css"));
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            // A viewfinder looks best on dark chrome.
            adw::StyleManager::default().set_color_scheme(adw::ColorScheme::PreferDark);
            setup_app_actions(app, &window_for_about);
        }
    });

    app.connect_activate({
        let (window, settings, devices) = (window.clone(), settings.clone(), devices.clone());
        let dev = opts.dev.clone();
        let test_source = opts.test_source || matches!(dev, Some((DevMode::SelfTest, _)));
        move |app| {
            if let Some(w) = window.borrow().as_ref() {
                w.window.present();
                return;
            }
            let dm = devices::DeviceManager::new(test_source);
            dm.start();
            *devices.borrow_mut() = Some(dm.clone());
            let w = window::Window::new(app, settings.borrow().clone().unwrap(), dm);
            w.window.present();
            let slot = window.clone();
            w.window.connect_destroy(move |_| {
                slot.borrow_mut().take();
            });
            match &dev {
                Some((DevMode::SelfTest, dir)) => selftest::run(app, &w, dir.clone()),
                Some((DevMode::Bench, _)) => selftest::bench(app, &w),
                Some((DevMode::Probe, dir)) => selftest::probe(app, &w, dir.clone()),
                Some((DevMode::RecordTest, _)) => selftest::record_test(app, &w, 4),
                Some((DevMode::UiTour, dir)) => selftest::ui_tour(app, &w, dir.clone()),
                None => {}
            }
            *window.borrow_mut() = Some(w);
        }
    });

    app.connect_shutdown({
        let (settings, devices) = (settings.clone(), devices.clone());
        move |_| {
            if let Some(d) = devices.borrow().as_ref() {
                d.stop();
            }
            if let Some(s) = settings.borrow().as_ref() {
                s.save();
            }
        }
    });

    let argv0 = std::env::args()
        .next()
        .unwrap_or_else(|| "camera-studio".into());
    app.run_with_args(&[argv0])
}

fn setup_app_actions(app: &adw::Application, window: &Rc<RefCell<Option<Rc<window::Window>>>>) {
    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            // Closing the window lets an active recording finish cleanly.
            match app.active_window() {
                Some(w) => w.close(),
                None => app.quit(),
            }
        }
    ));
    app.add_action(&quit);
    let about = gio::SimpleAction::new("about", None);
    let slot = window.clone();
    about.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| about::show(app.active_window().as_ref(), slot.borrow().as_deref())
    ));
    app.add_action(&about);
    let shortcuts = gio::SimpleAction::new("shortcuts", None);
    shortcuts.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| show_shortcuts(&app)
    ));
    app.add_action(&shortcuts);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("app.shortcuts", &["<Control>question", "<Control>F1"]);
    app.set_accels_for_action("win.preferences", &["<Control>comma"]);
    app.set_accels_for_action("win.gallery", &["<Control>o"]);
    app.set_accels_for_action("win.fullscreen", &["F11"]);
}

fn show_shortcuts(app: &adw::Application) {
    let dialog = adw::ShortcutsDialog::new();
    let sections: &[(&str, &[(&str, &str)])] = &[
        (
            "Capture",
            &[
                ("Take a photo / start or stop recording", "space"),
                ("Cancel timer, close panels", "Escape"),
                ("Photo mode", "p"),
                ("Video mode", "v"),
                ("QR code mode", "q"),
                ("Cycle self-timer", "t"),
                ("Cycle burst count", "b"),
            ],
        ),
        (
            "Viewfinder",
            &[
                ("Zoom in", "plus"),
                ("Zoom out", "minus"),
                ("Reset zoom", "0"),
                ("Toggle grid", "g"),
                ("Toggle mirror", "m"),
                ("Effects", "e"),
                ("Camera controls", "k"),
                ("Switch camera", "c"),
                ("Show FPS", "F12"),
                ("Fullscreen", "F11"),
            ],
        ),
        (
            "General",
            &[
                ("Gallery", "<Control>o"),
                ("Preferences", "<Control>comma"),
                ("Keyboard Shortcuts", "<Control>question"),
                ("Quit", "<Control>q"),
            ],
        ),
        (
            "Gallery",
            &[
                ("Previous / next item", "Left Right"),
                ("Move to trash", "Delete"),
                ("Play / pause video", "space"),
            ],
        ),
    ];
    for (title, items) in sections {
        let section = adw::ShortcutsSection::new(Some(title));
        for (label, accel) in *items {
            section.add(adw::ShortcutsItem::new(label, accel));
        }
        dialog.add(section);
    }
    dialog.present(app.active_window().as_ref());
}
