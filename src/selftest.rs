//! `--self-test DIR`: drives the real UI through its features with an isolated
//! config/output directory, logs the state and saves window screenshots.

use std::cell::RefCell;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{glib, graphene};

use crate::window::Window;

pub fn prepare_environment(dir: &Path) {
    let _ = std::fs::create_dir_all(dir.join("config/camera-studio"));
    // SAFETY: called before any thread is spawned.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
        std::env::set_var("XDG_CACHE_HOME", dir.join("cache"));
    }
    let settings = serde_json::json!({
        "photo-dir": dir.join("Pictures"),
        "video-dir": dir.join("Videos"),
        "shutter-sound": false,
        "show-fps": true,
        "window-width": 1100,
        "window-height": 700,
    });
    let _ = std::fs::write(
        dir.join("config/camera-studio/settings.json"),
        settings.to_string(),
    );
}

fn screenshot(w: &Window, path: &Path) -> bool {
    let win = &w.window;
    let (width, height) = (win.width(), win.height());
    let paintable = gtk::WidgetPaintable::new(Some(win));
    let snap = gtk::Snapshot::new();
    paintable.snapshot(&snap, width as f64, height as f64);
    let Some(node) = snap.to_node() else {
        return false;
    };
    let Some(renderer) = win.renderer() else {
        return false;
    };
    let tex = renderer.render_texture(
        &node,
        Some(&graphene::Rect::new(0.0, 0.0, width as f32, height as f32)),
    );
    tex.save_to_png(path).is_ok()
}

type Step = (u64, Box<dyn Fn(&Rc<Window>, &dyn Fn(&str))>);

pub fn run(app: &adw::Application, w: &Rc<Window>, dir: PathBuf) {
    let log_file = Rc::new(RefCell::new(
        std::fs::File::create(dir.join("selftest.log")).ok(),
    ));
    let t0 = Instant::now();
    let log = {
        let f = log_file.clone();
        move |line: &str| {
            let text = format!("[{:6.2}] {line}", t0.elapsed().as_secs_f64());
            println!("{text}");
            if let Some(f) = f.borrow_mut().as_mut() {
                let _ = writeln!(f, "{text}");
            }
        }
    };
    let state = |w: &Window| {
        format!(
            "camera={} running={} active={} fps={:.1} hw_decode={} recording={}",
            w.current
                .borrow()
                .as_ref()
                .map(|c| c.key.as_str())
                .unwrap_or("-"),
            w.camera.is_running(),
            w.camera.is_active(),
            w.camera.fps(),
            w.camera.uses_hw_decode(),
            w.camera.is_recording()
        )
    };
    let shot = {
        let dir = dir.clone();
        move |w: &Window, name: &str, log: &dyn Fn(&str)| {
            let ok = screenshot(w, &dir.join(format!("{name}.png")));
            log(&format!(
                "screenshot {name}: {}",
                if ok { "ok" } else { "FAILED" }
            ));
        }
    };
    let find = |w: &Window, part: &str| {
        w.devices
            .cameras
            .borrow()
            .iter()
            .find(|c| c.key.contains(part))
            .cloned()
    };
    let act = |w: &Window, name: &str, target: Option<&str>| {
        let _ = gtk::prelude::WidgetExt::activate_action(
            &w.window,
            name,
            target.map(|t| t.to_variant()).as_ref(),
        );
    };

    let mut steps: Vec<Step> = vec![
        (
            3000,
            Box::new({
                let shot = shot.clone();
                move |w, log| {
                    log(&format!("start: {}", state(w)));
                    shot(w, "01_start", log);
                }
            }),
        ),
        (
            200,
            Box::new(move |w, _| {
                if let Some(c) = find(w, "video2") {
                    w.start_camera(&c, None);
                }
            }),
        ),
        (
            3000,
            Box::new({
                let shot = shot.clone();
                move |w, log| {
                    log(&format!("ir: {}", state(w)));
                    shot(w, "02_ir_camera", log);
                    w.use_test_source();
                }
            }),
        ),
        (
            2500,
            Box::new(move |w, log| {
                log(&format!("test: {}", state(w)));
                act(w, "win.grid", None);
                act(w, "win.shutter", None);
            }),
        ),
        (
            150,
            Box::new({
                let shot = shot.clone();
                move |w, log| shot(w, "03_photo_flash", log)
            }),
        ),
        (
            1500,
            Box::new({
                let shot = shot.clone();
                move |w, log| {
                    shot(w, "04_photo_saved", log);
                    act(w, "win.effects", None);
                    if let Some(b) = w.effect_buttons.borrow().get("sepia") {
                        b.set_active(true);
                    }
                }
            }),
        ),
        (
            800,
            Box::new(move |w, _| {
                act(w, "win.zoom-in", None);
                act(w, "win.zoom-in", None);
            }),
        ),
        (
            1000,
            Box::new({
                let shot = shot.clone();
                move |w, log| {
                    shot(w, "05_effects_zoom", log);
                    log(&format!("effects+zoom: {}", state(w)));
                    act(w, "win.effects", None);
                    act(w, "win.zoom-reset", None);
                    if let Some(b) = w.effect_buttons.borrow().get("none") {
                        b.set_active(true);
                    }
                    act(w, "win.grid", None);
                    act(w, "win.capture-mode", Some("video"));
                }
            }),
        ),
        (1000, Box::new(move |w, _| act(w, "win.shutter", None))),
        (
            3000,
            Box::new({
                let shot = shot.clone();
                move |w, log| {
                    shot(w, "06_recording", log);
                    log(&format!("recording: {}", state(w)));
                }
            }),
        ),
        (200, Box::new(move |w, _| act(w, "win.shutter", None))),
        (
            3000,
            Box::new(move |w, log| {
                log(&format!("after stop: {}", state(w)));
                act(w, "win.capture-mode", Some("qr"));
            }),
        ),
        (
            1200,
            Box::new({
                let shot = shot.clone();
                move |w, log| {
                    shot(w, "07_qr", log);
                    act(w, "win.capture-mode", Some("photo"));
                    w.settings.update("burst", |d| d.burst = 3);
                    act(w, "win.shutter", None);
                }
            }),
        ),
        (
            2000,
            Box::new(move |w, _| {
                w.settings.update("burst", |d| d.burst = 1);
                act(w, "win.controls", None);
                if let Some(c) = find(w, "index0") {
                    w.controls.set_camera(Some(&c));
                }
            }),
        ),
        (
            1000,
            Box::new({
                let shot = shot.clone();
                move |w, log| {
                    shot(w, "08_controls", log);
                    act(w, "win.controls", None);
                    let cur = w.current.borrow().clone();
                    w.controls.set_camera(cur.as_ref());
                    act(w, "win.gallery", None);
                }
            }),
        ),
        (
            1500,
            Box::new({
                let shot = shot.clone();
                move |w, log| {
                    shot(w, "09_gallery", log);
                    w.open_latest();
                }
            }),
        ),
        (
            1500,
            Box::new({
                let shot = shot.clone();
                move |w, log| {
                    shot(w, "10_viewer", log);
                    w.viewer.select_first_video();
                }
            }),
        ),
        (1000, Box::new(move |w, _| w.viewer.player.toggle())),
        (
            1500,
            Box::new({
                let shot = shot.clone();
                move |w, log| {
                    shot(w, "11_video_player", log);
                    w.nav.pop_to_tag("camera");
                }
            }),
        ),
        (
            2500,
            Box::new(move |w, log| {
                log(&format!("back: {}", state(w)));
                act(w, "win.preferences", None);
            }),
        ),
        (
            1000,
            Box::new({
                let shot = shot.clone();
                move |w, log| {
                    shot(w, "12_preferences", log);
                    let _ =
                        gtk::prelude::WidgetExt::activate_action(&w.window, "app.shortcuts", None);
                }
            }),
        ),
        (
            1000,
            Box::new({
                let shot = shot.clone();
                move |w, log| {
                    shot(w, "13_shortcuts", log);
                    w.window.close();
                }
            }),
        ),
    ];

    // Real camera with a few GPU effects, right after the first screenshot.
    let effect_steps = ["sketch", "comic", "heat", "kaleido", "vintage", "night"]
        .into_iter()
        .map(|id| -> Step {
            let shot = shot.clone();
            (
                700,
                Box::new(move |w, _log| {
                    if let Some(b) = w.effect_buttons.borrow().get(id) {
                        b.set_active(true);
                    }
                    let w = w.clone();
                    let shot = shot.clone();
                    let name = format!("01_fx_{id}");
                    glib::timeout_add_local_once(Duration::from_millis(600), move || {
                        shot(&w, &name, &|_| {})
                    });
                }),
            )
        });
    let reset: Step = (
        700,
        Box::new(|w, _| {
            if let Some(b) = w.effect_buttons.borrow().get("none") {
                b.set_active(true);
            }
        }),
    );
    let tail = steps.split_off(1);
    steps.extend(effect_steps);
    steps.push(reset);
    steps.extend(tail);

    let mut at = 0u64;
    for (i, (delay, step)) in steps.into_iter().enumerate() {
        at += delay;
        let weak = Rc::downgrade(w);
        let log = log.clone();
        glib::timeout_add_local_once(Duration::from_millis(at), move || {
            if let Some(w) = weak.upgrade() {
                let t = Instant::now();
                step(&w, &log);
                log(&format!(
                    "step {:02} done in {:.2}s",
                    i + 1,
                    t.elapsed().as_secs_f64()
                ));
            }
        });
    }
    let app = app.clone();
    let dir2 = dir.clone();
    glib::timeout_add_local_once(Duration::from_millis(at + 4000), move || {
        let mut files: Vec<_> = walk(&dir2);
        files.sort();
        for (p, size) in files {
            log(&format!(
                "media: {} {size}",
                p.strip_prefix(&dir2).unwrap_or(&p).display()
            ));
        }
        app.quit();
    });
}

fn walk(dir: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    for sub in ["Pictures", "Videos"] {
        if let Ok(entries) = std::fs::read_dir(dir.join(sub)) {
            for e in entries.flatten() {
                out.push((e.path(), e.metadata().map(|m| m.len()).unwrap_or(0)));
            }
        }
    }
    out
}

fn cpu_ticks() -> u64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // Fields after the parenthesised command name; utime and stime are 14 and 15.
    let rest = stat.rsplit_once(')').map(|(_, r)| r).unwrap_or("");
    let f: Vec<&str> = rest.split_whitespace().collect();
    f.get(11).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0)
        + f.get(12).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0)
}

fn rss_mb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .map(|l| l.to_string())
        })
        .and_then(|l| {
            l.split_whitespace()
                .nth(1)
                .and_then(|v| v.parse::<u64>().ok())
        })
        .unwrap_or(0)
        / 1024
}

/// `--bench DIR`: CPU/RAM/FPS of the real camera in preview, effect and recording phases.
pub fn bench(app: &adw::Application, w: &Rc<Window>) {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    type Setup = Box<dyn Fn(&Rc<Window>)>;
    let phases: Vec<(&'static str, Setup)> = vec![
        ("preview", Box::new(|_| {})),
        (
            "Sketch effect + 2× zoom",
            Box::new(|w| {
                if let Some(b) = w.effect_buttons.borrow().get("sketch") {
                    b.set_active(true);
                }
                w.set_zoom(2.0);
            }),
        ),
        (
            "recording",
            Box::new(|w| {
                if let Some(b) = w.effect_buttons.borrow().get("none") {
                    b.set_active(true);
                }
                w.set_zoom(1.0);
                w.set_capture_mode("video", false);
                w.start_recording();
            }),
        ),
    ];
    let w = w.clone();
    let app = app.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_secs(5)).await;
        println!(
            "camera: {:?}, GPU decoding: {}",
            w.current.borrow().as_ref().map(|c| c.name.clone()),
            w.camera.uses_hw_decode()
        );
        for (name, setup) in phases {
            setup(&w);
            glib::timeout_future(Duration::from_secs(3)).await;
            let (t0, c0) = (Instant::now(), cpu_ticks());
            let mut fps = Vec::new();
            for _ in 0..10 {
                glib::timeout_future(Duration::from_secs(1)).await;
                fps.push(w.camera.fps());
            }
            let cpu = (cpu_ticks() - c0) as f64 / hz / t0.elapsed().as_secs_f64() * 100.0;
            let avg = fps.iter().sum::<f64>() / fps.len() as f64;
            println!(
                "{name:28} CPU {cpu:5.1}%  RAM {:4} MB  FPS {avg:4.1} (min {:.1})",
                rss_mb(),
                fps.iter().cloned().fold(f64::MAX, f64::min)
            );
        }
        w.stop_recording();
        glib::timeout_future(Duration::from_secs(3)).await;
        app.quit();
    });
}

/// `--probe DIR`: log the camera state every second for 14 s (used to test
/// error recovery, e.g. while another process holds the camera).
pub fn probe(app: &adw::Application, w: &Rc<Window>, dir: PathBuf) {
    let w = w.clone();
    let app = app.clone();
    glib::spawn_future_local(async move {
        for i in 1..=14 {
            glib::timeout_future(Duration::from_secs(1)).await;
            println!(
                "t={i:2}s camera={} running={} active={} fps={:.1}",
                w.current
                    .borrow()
                    .as_ref()
                    .map(|c| c.name.as_str())
                    .unwrap_or("-"),
                w.camera.is_running(),
                w.camera.is_active(),
                w.camera.fps()
            );
            if i == 3 || i == 14 {
                screenshot(&w, &dir.join(format!("probe_{i:02}.png")));
            }
            if i == 6 {
                // Worst case: every small side branch active at once.
                w.set_capture_mode("qr", false);
                let _ = gtk::prelude::WidgetExt::activate_action(&w.window, "win.effects", None);
            }
            if i == 10 {
                let bad = w.camera.check_full_resolution();
                println!(
                    "resolution with QR + effects strip: {}",
                    if bad.is_empty() {
                        "OK".to_string()
                    } else {
                        format!("FAIL {bad:?}")
                    }
                );
                screenshot(&w, &dir.join("probe_qr_effects.png"));
            }
            if i == 4 {
                let bad = w.camera.check_full_resolution();
                println!(
                    "main path resolution: {}",
                    if bad.is_empty() {
                        "OK".to_string()
                    } else {
                        format!("FAIL {bad:?}")
                    }
                );
                println!("{}", w.camera.debug_caps());
                if let Some(p) = w.view_paintable() {
                    println!(
                        "paintable intrinsic size: {}x{}",
                        p.intrinsic_width(),
                        p.intrinsic_height()
                    );
                }
            }
        }
        app.quit();
    });
}

/// `--record-test DIR`: record `secs` seconds from the default camera and quit
/// (used to check the first frames of recordings).
pub fn record_test(app: &adw::Application, w: &Rc<Window>, secs: u64) {
    let w = w.clone();
    let app = app.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_secs(4)).await;
        println!(
            "camera active={} hw_decode={}",
            w.camera.is_active(),
            w.camera.uses_hw_decode()
        );
        w.set_capture_mode("video", false);
        w.start_recording();
        glib::timeout_future(Duration::from_secs(secs)).await;
        if std::env::var_os("CAMERA_STUDIO_RECORD_INTERRUPT").is_some() {
            // Simulate the camera dying under a recording.
            println!("interrupting the camera");
            w.camera.stop();
        } else {
            w.stop_recording();
        }
        for _ in 0..50 {
            glib::timeout_future(Duration::from_millis(200)).await;
            if !w.camera.is_recording() {
                break;
            }
        }
        println!("recording finished: {}", !w.camera.is_recording());
        // After a failed recording the camera itself must still be running.
        glib::timeout_future(Duration::from_secs(2)).await;
        println!(
            "after: running={} active={} fps={:.1}",
            w.camera.is_running(),
            w.camera.is_active(),
            w.camera.fps()
        );
        app.quit();
    });
}

/// `--ui-tour DIR`: screenshots of every UI state with the real camera, at a
/// normal and a narrow window size (for design review).
pub fn ui_tour(app: &adw::Application, w: &Rc<Window>, dir: PathBuf) {
    let w = w.clone();
    let app = app.clone();
    glib::spawn_future_local(async move {
        let wait = |ms: u64| glib::timeout_future(Duration::from_millis(ms));
        let act = |w: &Window, name: &str, target: Option<&str>| {
            let _ = gtk::prelude::WidgetExt::activate_action(
                &w.window,
                name,
                target.map(|t| t.to_variant()).as_ref(),
            );
        };
        let shot = |w: &Window, name: &str| {
            screenshot(w, &dir.join(format!("{name}.png")));
        };
        for (suffix, width, height) in [("", 1100, 720), ("_narrow", 480, 760)] {
            w.window.set_default_size(width, height);
            wait(3500).await;

            shot(&w, &format!("a_photo{suffix}"));
            act(&w, "win.effects", None);
            wait(700).await;
            shot(&w, &format!("b_effects{suffix}"));
            act(&w, "win.effects", None);
            act(&w, "win.capture-mode", Some("video"));
            wait(700).await;
            shot(&w, &format!("c_video{suffix}"));
            w.start_recording();
            wait(2500).await;
            shot(&w, &format!("d_recording{suffix}"));
            w.stop_recording();
            wait(2500).await;
            act(&w, "win.capture-mode", Some("qr"));
            wait(1000).await;
            shot(&w, &format!("e_qr{suffix}"));
            act(&w, "win.capture-mode", Some("photo"));
            w.settings.update("timer", |d| d.timer = 3);
            act(&w, "win.shutter", None);
            wait(1200).await;
            shot(&w, &format!("f_countdown{suffix}"));
            wait(3000).await;
            w.settings.update("timer", |d| d.timer = 0);
            act(&w, "win.controls", None);
            wait(800).await;
            shot(&w, &format!("g_controls{suffix}"));
            act(&w, "win.controls", None);
            w.set_zoom(2.5);
            wait(600).await;
            shot(&w, &format!("h_zoom{suffix}"));
            w.set_zoom(1.0);
            w.open_gallery();
            wait(1500).await;
            shot(&w, &format!("i_gallery{suffix}"));
            w.open_latest();
            wait(1500).await;
            shot(&w, &format!("j_viewer{suffix}"));
            w.nav.pop_to_tag("camera");
            wait(500).await;
        }
        act(&w, "win.preferences", None);
        wait(1000).await;
        shot(&w, "k_preferences");
        if let Some(d) = w.window.visible_dialog() {
            d.close();
        }
        let _ = gtk::prelude::WidgetExt::activate_action(&w.window, "app.about", None);
        wait(1000).await;
        shot(&w, "l_about");
        println!("{}", crate::about::debug_info(Some(&w)));
        app.quit();
    });
}

/// `--snapshot DIR`: screenshot the window after CAMERA_STUDIO_SNAPSHOT_MS
/// (default 3000) and quit — e.g. to check how opened files are shown.
pub fn snapshot(app: &adw::Application, w: &Rc<Window>, dir: PathBuf) {
    let ms = std::env::var("CAMERA_STUDIO_SNAPSHOT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    let w = w.clone();
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(ms), move || {
        screenshot(&w, &dir.join("snapshot.png"));
        println!(
            "camera running={} viewer={:?}",
            w.camera.is_running(),
            w.viewer.current().map(|m| m.name())
        );
        if std::env::var_os("CAMERA_STUDIO_SNAPSHOT_BACK").is_none() {
            app.quit();
            return;
        }
        // Then go back to the camera page: the camera must start normally.
        w.nav.pop_to_tag("camera");
        glib::timeout_add_local_once(Duration::from_secs(4), move || {
            println!(
                "after going back: camera running={} active={} fps={:.1}",
                w.camera.is_running(),
                w.camera.is_active(),
                w.camera.fps()
            );
            app.quit();
        });
    });
}
