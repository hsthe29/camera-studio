//! Asynchronous thumbnails with memory and disk caches.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gst::prelude::*;
use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, gio, glib};

use crate::settings::cache_dir;
use crate::storage::classify;

pub const THUMB_SIZE: i32 = 320;

/// Pixels decoded in a worker thread (textures are built on the main thread).
struct Raw {
    width: i32,
    height: i32,
    format: gdk::MemoryFormat,
    stride: usize,
    bytes: glib::Bytes,
    duration: f64,
    from_disk_cache: bool,
}

#[derive(Clone)]
pub struct Thumb {
    pub texture: Option<gdk::Texture>,
    pub duration: f64,
}

type Callback = Box<dyn FnOnce(&Thumb)>;

pub struct ThumbnailCache {
    memory: RefCell<HashMap<PathBuf, Thumb>>,
    order: RefCell<VecDeque<PathBuf>>,
    pending: RefCell<HashMap<PathBuf, Vec<Callback>>>,
    capacity: usize,
}

fn cache_key(path: &Path) -> String {
    let meta = std::fs::metadata(path).ok();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    if let Some(m) = meta {
        m.len().hash(&mut h);
        m.modified().ok().hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

fn disk_dir() -> PathBuf {
    cache_dir().join("thumbnails")
}

fn load_image(path: &Path) -> Option<Raw> {
    let pixbuf = gdk_pixbuf::Pixbuf::from_file_at_scale(path, THUMB_SIZE, THUMB_SIZE, true).ok()?;
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    Some(Raw {
        width: pixbuf.width(),
        height: pixbuf.height(),
        format: if pixbuf.has_alpha() {
            gdk::MemoryFormat::R8g8b8a8
        } else {
            gdk::MemoryFormat::R8g8b8
        },
        stride: pixbuf.rowstride() as usize,
        bytes: pixbuf.read_pixel_bytes(),
        duration: 0.0,
        from_disk_cache: false,
    })
}

/// First meaningful frame of a video and its duration.
fn video_frame(path: &Path, width: i32) -> Option<Raw> {
    let desc = format!(
        "uridecodebin name=dec ! videoconvert ! videoscale ! \
         video/x-raw,format=RGBx,width={width},pixel-aspect-ratio=1/1 ! appsink name=sink sync=false max-buffers=1"
    );
    let pipeline = gst::parse::launch(&desc)
        .ok()?
        .downcast::<gst::Pipeline>()
        .ok()?;
    let uri = glib::filename_to_uri(path, None).ok()?;
    pipeline.by_name("dec")?.set_property("uri", uri.as_str());
    let sink = pipeline
        .by_name("sink")?
        .downcast::<gst_app::AppSink>()
        .ok()?;
    let result = (|| {
        pipeline.set_state(gst::State::Paused).ok()?;
        let (res, _, _) = pipeline.state(gst::ClockTime::from_seconds(5));
        res.ok()?;
        let duration = pipeline
            .query_duration::<gst::ClockTime>()
            .map(|d| d.seconds_f64())
            .unwrap_or(0.0);
        if duration > 0.0 {
            // The very first frames are often dark.
            let target = gst::ClockTime::from_seconds_f64((duration * 0.1).min(1.0));
            let _ = pipeline.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, target);
            let _ = pipeline.state(gst::ClockTime::from_seconds(5));
        }
        let sample = sink.try_pull_preroll(gst::ClockTime::from_seconds(2))?;
        let s = sample.caps()?.structure(0)?;
        let (w, h) = (s.get::<i32>("width").ok()?, s.get::<i32>("height").ok()?);
        let map = sample.buffer()?.map_readable().ok()?;
        Some(Raw {
            width: w,
            height: h,
            format: gdk::MemoryFormat::R8g8b8x8,
            stride: (w * 4) as usize,
            bytes: glib::Bytes::from(map.as_slice()),
            duration,
            from_disk_cache: false,
        })
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}

fn load_cached_png(file: &Path, meta: &Path) -> Option<Raw> {
    let pixbuf = gdk_pixbuf::Pixbuf::from_file(file).ok()?;
    let duration = std::fs::read_to_string(meta)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0.0);
    Some(Raw {
        width: pixbuf.width(),
        height: pixbuf.height(),
        format: if pixbuf.has_alpha() {
            gdk::MemoryFormat::R8g8b8a8
        } else {
            gdk::MemoryFormat::R8g8b8
        },
        stride: pixbuf.rowstride() as usize,
        bytes: pixbuf.read_pixel_bytes(),
        duration,
        from_disk_cache: true,
    })
}

fn produce(path: &Path) -> Option<Raw> {
    if classify(path)? {
        let key = cache_key(path);
        let png = disk_dir().join(format!("{key}.png"));
        let meta = disk_dir().join(format!("{key}.txt"));
        load_cached_png(&png, &meta).or_else(|| video_frame(path, THUMB_SIZE))
    } else {
        load_image(path)
    }
}

impl ThumbnailCache {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            memory: RefCell::new(HashMap::new()),
            order: RefCell::new(VecDeque::new()),
            pending: RefCell::new(HashMap::new()),
            capacity: 400,
        })
    }

    pub fn peek(&self, path: &Path) -> Option<Thumb> {
        self.memory.borrow().get(path).cloned()
    }

    pub fn put(&self, path: &Path, thumb: Thumb) {
        let mut order = self.order.borrow_mut();
        order.retain(|p| p != path);
        order.push_back(path.to_path_buf());
        self.memory.borrow_mut().insert(path.to_path_buf(), thumb);
        while order.len() > self.capacity {
            if let Some(old) = order.pop_front() {
                self.memory.borrow_mut().remove(&old);
            }
        }
    }

    pub fn forget(&self, path: &Path) {
        self.memory.borrow_mut().remove(path);
        self.order.borrow_mut().retain(|p| p != path);
    }

    pub fn request(self: &Rc<Self>, path: &Path, callback: impl FnOnce(&Thumb) + 'static) {
        if let Some(t) = self.peek(path) {
            callback(&t);
            return;
        }
        let first = {
            let mut pending = self.pending.borrow_mut();
            let list = pending.entry(path.to_path_buf()).or_default();
            list.push(Box::new(callback));
            list.len() == 1
        };
        if !first {
            return;
        }
        let this = self.clone();
        let path = path.to_path_buf();
        glib::spawn_future_local(async move {
            let p = path.clone();
            let raw = gio::spawn_blocking(move || produce(&p))
                .await
                .ok()
                .flatten();
            let thumb = match raw {
                Some(raw) => {
                    let texture: gdk::Texture = gdk::MemoryTexture::new(
                        raw.width, raw.height, raw.format, &raw.bytes, raw.stride,
                    )
                    .upcast();
                    let is_video = classify(&path) == Some(true);
                    if is_video && !raw.from_disk_cache {
                        let key = cache_key(&path);
                        let _ = std::fs::create_dir_all(disk_dir());
                        let _ = texture.save_to_png(disk_dir().join(format!("{key}.png")));
                        let _ = std::fs::write(
                            disk_dir().join(format!("{key}.txt")),
                            format!("{:.3}", raw.duration),
                        );
                    }
                    Thumb {
                        texture: Some(texture),
                        duration: raw.duration,
                    }
                }
                None => Thumb {
                    texture: None,
                    duration: 0.0,
                },
            };
            this.put(&path, thumb.clone());
            let callbacks = this.pending.borrow_mut().remove(&path).unwrap_or_default();
            for cb in callbacks {
                cb(&thumb);
            }
        });
    }
}
