//! Persistent JSON settings in ~/.config/camera-studio/settings.json.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::glib;
use serde::{Deserialize, Serialize};

pub const APP_DIR: &str = "camera-studio";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Data {
    pub camera: Option<String>,
    pub modes: BTreeMap<String, String>,
    pub controls: BTreeMap<String, BTreeMap<String, i32>>,
    pub capture_mode: String,
    pub timer: u32,
    pub burst: u32,
    pub effect: String,
    pub grid: bool,
    pub mirror_preview: bool,
    pub mirror_output: bool,
    pub fill_frame: bool,
    pub show_fps: bool,
    pub shutter_sound: bool,
    pub jpeg_quality: u32,
    pub video_format: String,
    pub video_quality: String,
    pub record_audio: bool,
    pub audio_device: Option<String>,
    pub photo_dir: Option<String>,
    pub video_dir: Option<String>,
    pub window_width: i32,
    pub window_height: i32,
    pub window_maximized: bool,
    /// Use VA-API hardware decoding/encoding when available.
    pub hardware_acceleration: bool,
}

impl Default for Data {
    fn default() -> Self {
        Self {
            camera: None,
            modes: BTreeMap::new(),
            controls: BTreeMap::new(),
            capture_mode: "photo".into(),
            timer: 0,
            burst: 1,
            effect: "none".into(),
            grid: false,
            mirror_preview: true,
            mirror_output: false,
            fill_frame: false,
            show_fps: false,
            shutter_sound: true,
            jpeg_quality: 92,
            video_format: "mp4".into(),
            video_quality: "high".into(),
            record_audio: true,
            audio_device: None,
            photo_dir: None,
            video_dir: None,
            window_width: 1100,
            window_height: 720,
            window_maximized: false,
            hardware_acceleration: true,
        }
    }
}

pub const TIMER_CHOICES: [u32; 4] = [0, 3, 5, 10];
pub const BURST_CHOICES: [u32; 4] = [1, 3, 5, 10];

impl Data {
    /// Field-by-field decoding: one wrongly typed value only resets that
    /// value, not every setting (per-camera modes and controls included).
    pub fn lenient(value: serde_json::Value) -> Self {
        let mut base = serde_json::to_value(Data::default()).unwrap();
        if let (Some(fields), Some(stored)) = (base.as_object_mut(), value.as_object()) {
            for (key, v) in stored {
                if !fields.contains_key(key) {
                    continue;
                }
                let mut candidate = fields.clone();
                candidate.insert(key.clone(), v.clone());
                if serde_json::from_value::<Data>(serde_json::Value::Object(candidate.clone()))
                    .is_ok()
                {
                    *fields = candidate;
                }
            }
        }
        let mut data: Data = serde_json::from_value(base).unwrap_or_default();
        data.sanitize();
        data
    }

    /// Bring hand-edited or old values back into the ranges the UI offers.
    pub fn sanitize(&mut self) {
        if !TIMER_CHOICES.contains(&self.timer) {
            self.timer = 0;
        }
        if !BURST_CHOICES.contains(&self.burst) {
            self.burst = 1;
        }
        self.jpeg_quality = self.jpeg_quality.clamp(50, 100);
        if !["photo", "video", "qr"].contains(&self.capture_mode.as_str()) {
            self.capture_mode = "photo".into();
        }
        if !["mp4", "webm"].contains(&self.video_format.as_str()) {
            self.video_format = "mp4".into();
        }
        if !["low", "medium", "high"].contains(&self.video_quality.as_str()) {
            self.video_quality = "high".into();
        }
        self.window_width = self.window_width.clamp(360, 16384);
        self.window_height = self.window_height.clamp(480, 16384);
    }
}

pub fn config_dir() -> PathBuf {
    glib::user_config_dir().join(APP_DIR)
}

pub fn cache_dir() -> PathBuf {
    glib::user_cache_dir().join(APP_DIR)
}

type Listener = Box<dyn Fn(&str)>;

/// Shared settings handle. Cheap to clone.
#[derive(Clone)]
pub struct Settings(Rc<Inner>);

struct Inner {
    path: PathBuf,
    data: RefCell<Data>,
    listeners: RefCell<Vec<(u32, Rc<Listener>)>>,
    next_id: std::cell::Cell<u32>,
    save_source: RefCell<Option<glib::SourceId>>,
}

impl Settings {
    pub fn load() -> Self {
        Self::load_from(config_dir().join("settings.json"))
    }

    pub fn load_from(path: PathBuf) -> Self {
        let data = match std::fs::read_to_string(&path) {
            Err(_) => Data::default(),
            Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(value) => Data::lenient(value),
                Err(_) => {
                    // Unreadable: keep a copy before it gets overwritten.
                    let _ = std::fs::copy(&path, path.with_extension("json.bak"));
                    Data::default()
                }
            },
        };
        Self(Rc::new(Inner {
            path,
            data: RefCell::new(data),
            listeners: RefCell::new(Vec::new()),
            next_id: std::cell::Cell::new(1),
            save_source: RefCell::new(None),
        }))
    }

    pub fn get(&self) -> std::cell::Ref<'_, Data> {
        self.0.data.borrow()
    }

    /// Mutate settings; listeners are told which key changed (only if it did).
    pub fn update(&self, key: &str, f: impl FnOnce(&mut Data)) {
        let changed = {
            let mut data = self.0.data.borrow_mut();
            let before = data.clone();
            f(&mut data);
            *data != before
        };
        if changed {
            self.schedule_save();
            let listeners: Vec<_> = self
                .0
                .listeners
                .borrow()
                .iter()
                .map(|(_, l)| l.clone())
                .collect();
            for l in listeners {
                l(key);
            }
        }
    }

    /// Change without notifying listeners (for values nobody observes).
    pub fn update_quiet(&self, f: impl FnOnce(&mut Data)) {
        f(&mut self.0.data.borrow_mut());
        self.schedule_save();
    }

    pub fn connect_changed(&self, f: impl Fn(&str) + 'static) -> u32 {
        let id = self.0.next_id.get();
        self.0.next_id.set(id + 1);
        self.0
            .listeners
            .borrow_mut()
            .push((id, Rc::new(Box::new(f))));
        id
    }

    pub fn disconnect(&self, id: u32) {
        self.0.listeners.borrow_mut().retain(|(i, _)| *i != id);
    }

    fn schedule_save(&self) {
        if let Some(source) = self.0.save_source.borrow_mut().take() {
            source.remove();
        }
        let weak = Rc::downgrade(&self.0);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            if let Some(inner) = weak.upgrade() {
                inner.save_source.borrow_mut().take();
                Settings(inner).write();
            }
        });
        *self.0.save_source.borrow_mut() = Some(id);
    }

    pub fn save(&self) {
        if let Some(source) = self.0.save_source.borrow_mut().take() {
            source.remove();
        }
        self.write();
    }

    fn write(&self) {
        let result = (|| -> std::io::Result<()> {
            let dir = self.0.path.parent().unwrap();
            std::fs::create_dir_all(dir)?;
            let tmp = dir.join(".settings.json.tmp");
            let json = serde_json::to_string_pretty(&*self.0.data.borrow())
                .map_err(std::io::Error::other)?;
            std::fs::write(&tmp, json)?;
            std::fs::rename(&tmp, &self.0.path)
        })();
        if let Err(e) = result {
            eprintln!("camera-studio: cannot save the settings: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_older_settings_file() {
        let dir = std::env::temp_dir().join(format!("cs-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"timer": 5, "mirror-preview": false, "modes": {"cam": "1280x720@30/1"},
            "unknown-key": 1, "controls": {"cam": {"0x980900": 140}}}"#,
        )
        .unwrap();
        let s = Settings::load_from(path.clone());
        assert_eq!(s.get().timer, 5);
        assert!(!s.get().mirror_preview);
        assert_eq!(s.get().modes["cam"], "1280x720@30/1");
        assert_eq!(s.get().controls["cam"]["0x980900"], 140);
        assert_eq!(s.get().jpeg_quality, 92); // default for missing key
        std::fs::write(&path, "{broken").unwrap();
        assert_eq!(Settings::load_from(path.clone()).get().burst, 1);
        assert!(path.with_extension("json.bak").exists()); // the broken file was kept

        // One bad field only resets that field; out-of-range values are fixed.
        std::fs::write(
            &path,
            r#"{"timer": -1, "burst": 1000, "jpeg-quality": 250,
            "modes": {"cam": "640x480@30/1"}, "grid": true, "capture-mode": "zzz"}"#,
        )
        .unwrap();
        let s = Settings::load_from(path);
        assert_eq!(s.get().timer, 0);
        assert_eq!(s.get().burst, 1);
        assert_eq!(s.get().jpeg_quality, 100);
        assert_eq!(s.get().capture_mode, "photo");
        assert!(s.get().grid);
        assert_eq!(s.get().modes["cam"], "640x480@30/1");
        std::fs::remove_dir_all(dir).ok();
    }
}
