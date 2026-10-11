//! Preferences dialog.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gst::prelude::*;
use gtk::gio;

use crate::camera::Accel;
use crate::settings::{Data, Settings};
use crate::storage;

/// (pulse device name or None for the default, label)
pub fn microphones() -> Vec<(Option<String>, String)> {
    let mut result = vec![(None, "System default".to_string())];
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Audio/Source"), None);
    monitor.set_show_all_devices(true);
    if monitor.start().is_ok() {
        for d in monitor.devices() {
            if d.type_().name() != "GstPulseDevice" {
                continue;
            }
            let Some(name) = gst::prelude::DeviceExt::properties(&d)
                .and_then(|p| p.get::<String>("device.name").ok())
            else {
                continue;
            };
            if name.ends_with(".monitor") {
                continue;
            }
            result.push((
                Some(name),
                gst::prelude::DeviceExt::display_name(&d).to_string(),
            ));
        }
        monitor.stop();
    }
    result
}

pub fn show(parent: &impl IsA<gtk::Widget>, settings: &Settings, accel: Accel) {
    let dialog = adw::PreferencesDialog::builder()
        .title("Preferences")
        .search_enabled(true)
        .build();
    let handlers: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));

    let switch = |key: &'static str,
                  title: &str,
                  subtitle: Option<&str>,
                  get: fn(&Data) -> bool,
                  set: fn(&mut Data, bool)| {
        let row = adw::SwitchRow::builder().title(title).build();
        if let Some(s) = subtitle {
            row.set_subtitle(s);
        }
        row.set_active(get(&settings.get()));
        let st = settings.clone();
        row.connect_active_notify(move |r| {
            let v = r.is_active();
            st.update(key, |d| set(d, v));
        });
        let st = settings.clone();
        let weak = row.downgrade();
        handlers
            .borrow_mut()
            .push(settings.connect_changed(move |k| {
                if let (true, Some(r)) = (k == key, weak.upgrade()) {
                    // Read first: set_active() re-enters Settings::update().
                    let value = get(&st.get());
                    r.set_active(value);
                }
            }));
        row
    };

    // --- General ---
    let general = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();
    let view = adw::PreferencesGroup::builder().title("Viewfinder").build();
    view.add(&switch(
        "mirror-preview",
        "Mirror Viewfinder",
        Some("Show the picture like a mirror, natural for front-facing webcams"),
        |d| d.mirror_preview,
        |d, v| d.mirror_preview = v,
    ));
    view.add(&switch(
        "fill-frame",
        "Fill Window",
        Some("Crop the edges so the picture covers the whole window"),
        |d| d.fill_frame,
        |d, v| d.fill_frame = v,
    ));
    view.add(&switch(
        "grid",
        "Show 3×3 Grid",
        Some("Helps composing with the rule of thirds"),
        |d| d.grid,
        |d, v| d.grid = v,
    ));
    view.add(&switch(
        "show-fps",
        "Show Frame Rate",
        None,
        |d| d.show_fps,
        |d, v| d.show_fps = v,
    ));
    general.add(&view);
    let sound = adw::PreferencesGroup::builder().title("Sound").build();
    sound.add(&switch(
        "shutter-sound",
        "Shutter Sound",
        Some("Play sounds when taking photos, counting down and recording"),
        |d| d.shutter_sound,
        |d, v| d.shutter_sound = v,
    ));
    general.add(&sound);
    let perf = adw::PreferencesGroup::builder()
        .title("Performance")
        .build();
    let hw = switch(
        "hardware-acceleration",
        "Hardware Acceleration (VA-API)",
        Some("Decode MJPEG and encode video on the GPU"),
        |d| d.hardware_acceleration,
        |d, v| d.hardware_acceleration = v,
    );
    let gpu = if accel.va_decode {
        "GPU support: JPEG decoding"
    } else {
        "No VA-API driver found"
    };
    let enc = match (accel.va_h264, accel.va_vp9) {
        (true, true) => ", H.264 and VP9 encoding",
        (true, false) => ", H.264 encoding",
        _ => "",
    };
    perf.set_description(Some(&format!(
        "{gpu}{enc}. Effects and zoom always run on the GPU (OpenGL)."
    )));
    perf.add(&hw);
    general.add(&perf);
    dialog.add(&general);

    // --- Photo ---
    let photo = adw::PreferencesPage::builder()
        .title("Photo")
        .icon_name("camera-photo-symbolic")
        .build();
    let pg = adw::PreferencesGroup::builder().title("Photos").build();
    let quality = adw::SpinRow::with_range(50.0, 100.0, 1.0);
    quality.set_title("JPEG Quality");
    quality.set_subtitle("Higher means better pictures but larger files");
    quality.set_value(settings.get().jpeg_quality as f64);
    let st = settings.clone();
    quality.connect_value_notify(move |r| {
        let v = r.value() as u32;
        st.update("jpeg-quality", |d| d.jpeg_quality = v);
    });
    pg.add(&quality);
    pg.add(&switch(
        "mirror-output",
        "Save Mirrored Photos and Videos",
        Some("Files match the mirrored viewfinder"),
        |d| d.mirror_output,
        |d, v| d.mirror_output = v,
    ));
    photo.add(&pg);
    let loc = adw::PreferencesGroup::builder().title("Storage").build();
    loc.add(&folder_row(&dialog, settings, "photo-dir", "Photos Folder"));
    loc.add(&folder_row(&dialog, settings, "video-dir", "Videos Folder"));
    photo.add(&loc);
    dialog.add(&photo);

    // --- Video ---
    let video = adw::PreferencesPage::builder()
        .title("Video")
        .icon_name("camera-video-symbolic")
        .build();
    let vg = adw::PreferencesGroup::builder().title("Recording").build();
    let formats = [
        ("mp4", "MP4 (H.264 + AAC)"),
        ("webm", "WebM (VP9/VP8 + Opus)"),
    ];
    let fmt = adw::ComboRow::builder()
        .title("Format")
        .model(&gtk::StringList::new(
            &formats.iter().map(|f| f.1).collect::<Vec<_>>(),
        ))
        .build();
    fmt.set_selected(
        formats
            .iter()
            .position(|f| f.0 == settings.get().video_format)
            .unwrap_or(0) as u32,
    );
    let st = settings.clone();
    fmt.connect_selected_notify(move |r| {
        let v = formats[r.selected() as usize].0.to_string();
        st.update("video-format", |d| d.video_format = v);
    });
    vg.add(&fmt);
    let qualities = [
        ("low", "Low (small files)"),
        ("medium", "Medium"),
        ("high", "High"),
    ];
    let q = adw::ComboRow::builder()
        .title("Quality")
        .model(&gtk::StringList::new(
            &qualities.iter().map(|f| f.1).collect::<Vec<_>>(),
        ))
        .build();
    q.set_selected(
        qualities
            .iter()
            .position(|f| f.0 == settings.get().video_quality)
            .unwrap_or(2) as u32,
    );
    let st = settings.clone();
    q.connect_selected_notify(move |r| {
        let v = qualities[r.selected() as usize].0.to_string();
        st.update("video-quality", |d| d.video_quality = v);
    });
    vg.add(&q);
    video.add(&vg);
    let ag = adw::PreferencesGroup::builder().title("Audio").build();
    ag.add(&switch(
        "record-audio",
        "Record Audio",
        Some("Record sound from the microphone"),
        |d| d.record_audio,
        |d, v| d.record_audio = v,
    ));
    let mics = microphones();
    let mic = adw::ComboRow::builder()
        .title("Microphone")
        .model(&gtk::StringList::new(
            &mics.iter().map(|m| m.1.as_str()).collect::<Vec<_>>(),
        ))
        .build();
    let current = settings.get().audio_device.clone();
    mic.set_selected(mics.iter().position(|m| m.0 == current).unwrap_or(0) as u32);
    mic.set_sensitive(settings.get().record_audio);
    let st = settings.clone();
    let names: Vec<Option<String>> = mics.iter().map(|m| m.0.clone()).collect();
    mic.connect_selected_notify(move |r| {
        let v = names.get(r.selected() as usize).cloned().flatten();
        st.update("audio-device", |d| d.audio_device = v);
    });
    let st = settings.clone();
    let weak = mic.downgrade();
    handlers
        .borrow_mut()
        .push(settings.connect_changed(move |k| {
            if let (true, Some(m)) = (k == "record-audio", weak.upgrade()) {
                m.set_sensitive(st.get().record_audio);
            }
        }));
    ag.add(&mic);
    video.add(&ag);
    dialog.add(&video);

    let st = settings.clone();
    dialog.connect_closed(move |_| {
        for h in handlers.borrow_mut().drain(..) {
            st.disconnect(h);
        }
    });
    dialog.present(Some(parent));
}

fn folder_row(
    dialog: &adw::PreferencesDialog,
    settings: &Settings,
    key: &'static str,
    title: &str,
) -> adw::ActionRow {
    let current = {
        let s = settings.clone();
        move || -> PathBuf {
            if key == "photo-dir" {
                storage::photo_dir(&s)
            } else {
                storage::video_dir(&s)
            }
        }
    };
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(current().to_string_lossy())
        .subtitle_selectable(true)
        .build();
    let choose = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text("Choose Folder")
        .build();
    choose.add_css_class("flat");
    let reset = gtk::Button::builder()
        .icon_name("edit-undo-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text("Use Default Folder")
        .build();
    reset.add_css_class("flat");
    let set = {
        let s = settings.clone();
        move |v: Option<String>| {
            s.update(key, |d| {
                if key == "photo-dir" {
                    d.photo_dir = v
                } else {
                    d.video_dir = v
                }
            });
        }
    };
    let (r, cur, st, title) = (row.clone(), current.clone(), set.clone(), title.to_string());
    let dialog_weak = dialog.downgrade();
    choose.connect_clicked(move |_| {
        let fd = gtk::FileDialog::builder()
            .title(&title)
            .modal(true)
            .initial_folder(&gio::File::for_path(cur()))
            .build();
        let (r, cur, st) = (r.clone(), cur.clone(), st.clone());
        let root = dialog_weak
            .upgrade()
            .and_then(|d| d.root())
            .and_downcast::<gtk::Window>();
        fd.select_folder(root.as_ref(), gio::Cancellable::NONE, move |res| {
            if let Ok(folder) = res
                && let Some(p) = folder.path()
            {
                st(Some(p.to_string_lossy().into_owned()));
                r.set_subtitle(&cur().to_string_lossy());
            }
        });
    });
    let (r, cur) = (row.clone(), current.clone());
    reset.connect_clicked(move |_| {
        set(None);
        r.set_subtitle(&cur().to_string_lossy());
    });
    row.add_suffix(&reset);
    row.add_suffix(&choose);
    row
}
