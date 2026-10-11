//! Camera discovery (with hotplug) and capture-mode parsing.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use gst::prelude::*;
use gtk::glib;

pub const TEST_KEY: &str = "test:pattern";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Mode {
    pub width: i32,
    pub height: i32,
    pub fps_n: i32,
    pub fps_d: i32,
    /// "image/jpeg", "video/x-raw", ...
    pub media: String,
    /// Raw pixel format, e.g. "YUY2".
    pub format: Option<String>,
}

impl Mode {
    pub fn fps(&self) -> f64 {
        if self.fps_d == 0 {
            0.0
        } else {
            self.fps_n as f64 / self.fps_d as f64
        }
    }

    pub fn key(&self) -> String {
        format!(
            "{}x{}@{}/{}",
            self.width, self.height, self.fps_n, self.fps_d
        )
    }

    pub fn label(&self) -> String {
        let fps = self.fps();
        let fps = if (fps - fps.round()).abs() < 0.01 {
            format!("{fps:.0}")
        } else {
            format!("{fps:.2}")
        };
        format!("{}×{} · {fps} fps", self.width, self.height)
    }

    pub fn short_label(&self) -> String {
        match self.height {
            2160 => "4K".into(),
            1440 | 1080 | 720 | 480 | 360 => format!("{}p", self.height),
            h => format!("{h}p"),
        }
    }

    pub fn caps_string(&self) -> String {
        let mut caps = format!(
            "{},width={},height={},framerate={}/{}",
            self.media, self.width, self.height, self.fps_n, self.fps_d
        );
        if self.media == "video/x-raw"
            && let Some(f) = &self.format
        {
            caps.push_str(&format!(",format={f}"));
        }
        caps
    }

    pub fn is_mjpeg(&self) -> bool {
        self.media == "image/jpeg"
    }
}

/// Preference when the same size/rate is offered in several encodings:
/// raw needs no decoding, MJPEG is cheap to decode (on the GPU even more so).
fn media_rank(media: &str) -> i32 {
    match media {
        "video/x-raw" => 3,
        "image/jpeg" => 2,
        _ => 0,
    }
}

fn parse_fractions(text: &str) -> Vec<(i32, i32)> {
    let Some(start) = text.find("framerate=(fraction)") else {
        return vec![(30, 1)];
    };
    let rest = &text[start + "framerate=(fraction)".len()..];
    let (value, is_range) = if rest.starts_with('{') {
        (
            &rest[..rest.find('}').map(|i| i + 1).unwrap_or(rest.len())],
            false,
        )
    } else if rest.starts_with('[') {
        (
            &rest[..rest.find(']').map(|i| i + 1).unwrap_or(rest.len())],
            true,
        )
    } else {
        (rest.split([',', ';']).next().unwrap_or(""), false)
    };
    let fracs: Vec<(i32, i32)> = value
        .split(|c: char| !(c.is_ascii_digit() || c == '/'))
        .filter_map(|t| {
            let (a, b) = t.split_once('/')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .collect();
    if is_range {
        if fracs.len() != 2 {
            return vec![(30, 1)];
        }
        let lo = fracs[0].0 as f64 / fracs[0].1.max(1) as f64;
        let hi = fracs[1].0 as f64 / fracs[1].1.max(1) as f64;
        let common: Vec<_> = [60, 30, 25, 24, 15]
            .iter()
            .filter(|&&n| lo <= n as f64 && n as f64 <= hi)
            .map(|&n| (n, 1))
            .collect();
        return if common.is_empty() {
            vec![fracs[1]]
        } else {
            common
        };
    }
    let v: Vec<_> = fracs.into_iter().filter(|f| f.0 > 0).collect();
    if v.is_empty() { vec![(30, 1)] } else { v }
}

fn parse_ints(text: &str, name: &str) -> Vec<i32> {
    let pat = format!("{name}=(int)");
    let Some(start) = text.find(&pat) else {
        return vec![];
    };
    let rest = &text[start + pat.len()..];
    let (value, is_range) = if rest.starts_with('{') {
        (
            &rest[..rest.find('}').map(|i| i + 1).unwrap_or(rest.len())],
            false,
        )
    } else if rest.starts_with('[') {
        (
            &rest[..rest.find(']').map(|i| i + 1).unwrap_or(rest.len())],
            true,
        )
    } else {
        (rest.split([',', ';']).next().unwrap_or(""), false)
    };
    let nums: Vec<i32> = value
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|t| t.parse().ok())
        .collect();
    if is_range {
        if nums.len() < 2 {
            return vec![];
        }
        let (lo, hi) = (nums[0], nums[1]);
        let candidates: &[i32] = if name == "width" {
            &[
                3840, 2560, 1920, 1280, 1024, 960, 800, 640, 480, 360, 320, 240,
            ]
        } else {
            &[2160, 1440, 1080, 720, 600, 540, 480, 360, 240]
        };
        return candidates
            .iter()
            .copied()
            .filter(|v| lo <= *v && *v <= hi)
            .collect();
    }
    nums
}

const STD_ASPECTS: [(i32, i32); 6] = [(16, 9), (4, 3), (3, 2), (16, 10), (5, 4), (1, 1)];

/// Best mode per resolution (highest fps, cheapest encoding), largest first.
pub fn parse_modes(caps: &gst::CapsRef) -> Vec<Mode> {
    let mut best: Vec<Mode> = Vec::new();
    for (s, features) in caps.iter_with_features() {
        if features.contains("memory:DMABuf") {
            continue;
        }
        let media = s.name().to_string();
        if media_rank(&media) == 0 {
            continue;
        }
        let text = s.to_string();
        let format = if media == "video/x-raw" {
            match s.get::<String>("format") {
                Ok(f) => Some(f),
                Err(_) => continue,
            }
        } else {
            None
        };
        let widths = parse_ints(&text, "width");
        let heights = parse_ints(&text, "height");
        if widths.is_empty() || heights.is_empty() {
            continue;
        }
        let ranged = text.contains("width=(int)[");
        let mut pairs = Vec::new();
        if !ranged && widths.len() == 1 && heights.len() == 1 {
            pairs.push((widths[0], heights[0]));
        } else {
            for &w in &widths {
                for &h in &heights {
                    if !ranged || STD_ASPECTS.iter().any(|(a, b)| w * b == h * a) {
                        pairs.push((w, h));
                    }
                }
            }
        }
        for (fps_n, fps_d) in parse_fractions(&text) {
            for &(w, h) in &pairs {
                let mode = Mode {
                    width: w,
                    height: h,
                    fps_n,
                    fps_d,
                    media: media.clone(),
                    format: format.clone(),
                };
                match best.iter_mut().find(|m| m.width == w && m.height == h) {
                    Some(cur) => {
                        if (mode.fps(), media_rank(&mode.media))
                            > (cur.fps(), media_rank(&cur.media))
                        {
                            *cur = mode;
                        }
                    }
                    None => best.push(mode),
                }
            }
        }
    }
    best.sort_by(|a, b| {
        (b.width * b.height, b.fps() as i64).cmp(&(a.width * a.height, a.fps() as i64))
    });
    best
}

/// Largest resolution that still runs at >= 24 fps, else the largest.
pub fn default_mode(modes: &[Mode]) -> Option<Mode> {
    modes
        .iter()
        .find(|m| m.fps() >= 24.0)
        .or(modes.first())
        .cloned()
}

#[derive(Clone, Debug)]
pub struct Camera {
    pub key: String,
    pub name: String,
    pub path: Option<String>,
    pub modes: Vec<Mode>,
    pub is_ir: bool,
}

impl Camera {
    pub fn is_test(&self) -> bool {
        self.key == TEST_KEY
    }
}

pub fn test_camera() -> Camera {
    let modes = [(1920, 1080), (1280, 720), (640, 480)]
        .iter()
        .map(|&(w, h)| Mode {
            width: w,
            height: h,
            fps_n: 30,
            fps_d: 1,
            media: "video/x-raw".into(),
            format: Some("I420".into()),
        })
        .collect();
    Camera {
        key: TEST_KEY.into(),
        name: "Test Source".into(),
        path: None,
        modes,
        is_ir: false,
    }
}

/// Prefer /dev/v4l/by-id names: they survive re-plugging and reboots.
fn stable_key(path: &str) -> String {
    let target = std::fs::canonicalize(path).ok();
    if let Ok(entries) = std::fs::read_dir("/dev/v4l/by-id") {
        let mut names: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        names.sort();
        for p in names {
            if target.is_some() && std::fs::canonicalize(&p).ok() == target {
                return format!("v4l2:{}", p.file_name().unwrap().to_string_lossy());
            }
        }
    }
    format!("v4l2:{path}")
}

fn camera_from_device(device: &gst::Device) -> Option<Camera> {
    let props = device.properties()?;
    let path = props
        .get::<String>("device.path")
        .or_else(|_| props.get::<String>("api.v4l2.path"))
        .ok()?;
    let caps = device.caps()?;
    let modes = parse_modes(&caps);
    if modes.is_empty() {
        return None;
    }
    let display = device.display_name().to_string();
    let card = props
        .get::<String>("v4l2.device.card")
        .or_else(|_| props.get::<String>("api.v4l2.cap.card"))
        .unwrap_or_else(|_| display.clone());
    let mut name = card.split(':').next().unwrap_or("").trim().to_string();
    if name.is_empty() {
        name = display;
    }
    let is_ir = modes.iter().all(|m| {
        m.media == "video/x-raw"
            && matches!(m.format.as_deref(), Some("GRAY8" | "GRAY16_LE" | "Y800"))
    });
    if is_ir {
        name.push_str(" (IR)");
    }
    Some(Camera {
        key: stable_key(&path),
        name,
        path: Some(path),
        modes,
        is_ir,
    })
}

type Listener = Box<dyn Fn()>;

/// Keeps an up-to-date list of V4L2 cameras.
pub struct DeviceManager {
    monitor: gst::DeviceMonitor,
    pub cameras: RefCell<Vec<Camera>>,
    pub include_test: std::cell::Cell<bool>,
    listeners: RefCell<Vec<Listener>>,
    refresh_source: RefCell<Option<glib::SourceId>>,
    _watch: RefCell<Option<gst::bus::BusWatchGuard>>,
}

impl DeviceManager {
    pub fn new(include_test: bool) -> Rc<Self> {
        let monitor = gst::DeviceMonitor::new();
        monitor.add_filter(Some("Video/Source"), None);
        // The PipeWire provider hides the V4L2 one; we want direct V4L2 access.
        monitor.set_show_all_devices(true);
        let this = Rc::new(Self {
            monitor,
            cameras: RefCell::new(Vec::new()),
            include_test: std::cell::Cell::new(include_test),
            listeners: RefCell::new(Vec::new()),
            refresh_source: RefCell::new(None),
            _watch: RefCell::new(None),
        });
        let weak = Rc::downgrade(&this);
        let watch = this
            .monitor
            .bus()
            .add_watch_local(move |_, msg| {
                use gst::MessageView;
                if let Some(this) = weak.upgrade()
                    && matches!(
                        msg.view(),
                        MessageView::DeviceAdded(_)
                            | MessageView::DeviceRemoved(_)
                            | MessageView::DeviceChanged(_)
                    )
                {
                    this.schedule_refresh();
                }
                glib::ControlFlow::Continue
            })
            .ok();
        *this._watch.borrow_mut() = watch;
        this
    }

    pub fn start(&self) {
        if self.monitor.start().is_err() {
            eprintln!("camera-studio: could not start the device monitor");
        }
        self.refresh();
    }

    pub fn stop(&self) {
        self.monitor.stop();
    }

    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    fn schedule_refresh(self: &Rc<Self>) {
        if self.refresh_source.borrow().is_some() {
            return;
        }
        // Several messages arrive per physical device; batch them.
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
            if let Some(this) = weak.upgrade() {
                this.refresh_source.borrow_mut().take();
                this.refresh();
            }
        });
        *self.refresh_source.borrow_mut() = Some(id);
    }

    pub fn refresh(&self) {
        let mut cams: Vec<Camera> = Vec::new();
        for device in self.monitor.devices() {
            if device.type_().name() != "GstV4l2Device" {
                continue;
            }
            if let Some(cam) = camera_from_device(&device)
                && !cams.iter().any(|c| c.key == cam.key)
            {
                cams.push(cam);
            }
        }
        cams.sort_by(|a, b| (a.is_ir, &a.path).cmp(&(b.is_ir, &b.path)));
        let names: Vec<String> = cams.iter().map(|c| c.name.clone()).collect();
        for cam in cams.iter_mut() {
            if names.iter().filter(|n| **n == cam.name).count() > 1 {
                let base = cam.path.as_deref().map(|p| {
                    Path::new(p)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                });
                cam.name = format!("{} ({})", cam.name, base.unwrap_or_default());
            }
        }
        if self.include_test.get() {
            cams.push(test_camera());
        }
        let changed = {
            let old = self.cameras.borrow();
            old.len() != cams.len()
                || old
                    .iter()
                    .zip(&cams)
                    .any(|(a, b)| a.key != b.key || a.path != b.path)
        };
        *self.cameras.borrow_mut() = cams;
        if changed {
            for l in self.listeners.borrow().iter() {
                l();
            }
        }
    }

    pub fn find(&self, key: Option<&str>) -> Option<Camera> {
        let key = key?;
        self.cameras.borrow().iter().find(|c| c.key == key).cloned()
    }

    pub fn preferred(&self, key: Option<&str>) -> Option<Camera> {
        self.find(key)
            .or_else(|| self.cameras.borrow().first().cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uvc_caps() {
        gst::init().unwrap();
        let caps = gst::Caps::from_str(
            "image/jpeg, width=1920, height=1080, framerate={ 30/1, 15/1 }; \
             image/jpeg, width=640, height=480, framerate={ 30/1, 15/1 }; \
             video/x-raw(memory:DMABuf), format=DMA_DRM, width=640, height=480, framerate=30/1; \
             video/x-raw, format=YUY2, width=640, height=480, framerate={ 30/1, 15/1 }; \
             video/x-raw, format=YUY2, width=1920, height=1080, framerate=5/1",
        )
        .unwrap();
        let modes = parse_modes(&caps);
        let sizes: Vec<_> = modes.iter().map(|m| (m.width, m.height)).collect();
        assert_eq!(sizes, [(1920, 1080), (640, 480)]);
        assert_eq!(
            (modes[0].media.as_str(), modes[0].fps()),
            ("image/jpeg", 30.0)
        );
        assert_eq!(modes[1].format.as_deref(), Some("YUY2"));
        assert_eq!(default_mode(&modes).unwrap(), modes[0]);
        assert_eq!(
            modes[0].caps_string(),
            "image/jpeg,width=1920,height=1080,framerate=30/1"
        );
    }

    #[test]
    fn ranged_caps() {
        gst::init().unwrap();
        let caps = gst::Caps::from_str(
            "video/x-raw, format=YUY2, width=[ 320, 1280 ], height=[ 240, 720 ], framerate=[ 1/1, 30/1 ]",
        )
        .unwrap();
        let modes = parse_modes(&caps);
        let sizes: Vec<_> = modes.iter().map(|m| (m.width, m.height)).collect();
        assert!(sizes.contains(&(1280, 720)));
        assert!(sizes.contains(&(640, 480)));
        assert!(!sizes.contains(&(1280, 240)));
        assert!(modes.iter().all(|m| m.fps() == 30.0));
    }

    #[test]
    fn labels() {
        let m = Mode {
            width: 1280,
            height: 720,
            fps_n: 30000,
            fps_d: 1001,
            media: "image/jpeg".into(),
            format: None,
        };
        assert_eq!(m.label(), "1280×720 · 29.97 fps");
        assert_eq!(m.short_label(), "720p");
    }

    use std::str::FromStr;
}
