//! The live camera pipeline (GPU path).
//!
//! ```text
//! v4l2src ! caps ! queue(leaky) ! vajpegdec ! vapostproc ! DMABuf ! glupload
//!         ! glcolorconvert ! glshader (zoom + effect) ! tee
//!   tee ┬ queue(leaky,1) ! glsinkbin(gtk4paintablesink)   preview, zero copy
//!       ├ valve ! gldownload ! appsink                      photos (on demand)
//!       ├ valve ! glcolorscale ½ ! gldownload ! zbar        QR mode only
//!       └ [recording bin, linked while recording]
//!            gldownload ! DMABuf ! vapostproc ! vah264enc ! mp4mux ! filesink
//!            pulsesrc ! voaacenc ! mux
//! ```
//! Without VA-API the same layout is used with jpegdec / openh264enc.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use gst::prelude::*;
use gtk::{gdk, glib};

use crate::devices::{Camera, Mode};
use crate::effects;

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(8);
/// Width of the frames used for the effect previews.
pub const PREVIEW_WIDTH: i32 = 192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Busy,
    Permission,
    Disconnected,
    Timeout,
    Other,
}

pub enum Event {
    Started,
    Error(ErrorKind, String),
    Barcode(String, String),
    Photo(gst::Sample),
    /// Small unprocessed frame (no effect, no zoom) for the effect previews.
    EffectSource(gst::Sample),
    RecordingFinished {
        path: PathBuf,
        ok: bool,
        message: String,
    },
}

enum ThreadEvent {
    FirstFrame(u64),
    Photo(u64, gst::Sample),
    RawFrame(u64, gst::Sample),
}

#[derive(Clone, Debug)]
pub struct RecordOptions {
    pub path: PathBuf,
    pub container: String, // "mp4" | "webm"
    pub quality: String,   // "low" | "medium" | "high"
    pub audio: bool,
    pub audio_device: Option<String>,
    pub mirror: bool,
}

pub fn has_element(name: &str) -> bool {
    gst::ElementFactory::find(name).is_some()
}

/// What the GPU can do for us on this machine.
#[derive(Clone, Copy, Debug)]
pub struct Accel {
    pub va_decode: bool,
    pub va_h264: bool,
    pub va_vp9: bool,
}

impl Accel {
    pub fn detect(enabled: bool) -> Self {
        let va = enabled && has_element("vapostproc");
        Self {
            va_decode: va && has_element("vajpegdec"),
            va_h264: va && has_element("vah264enc"),
            va_vp9: va && has_element("vavp9enc"),
        }
    }
}

struct Recording {
    bin: gst::Bin,
    tee_pad: gst::Pad,
    path: PathBuf,
    started: Instant,
    stopping: bool,
    timeout: Option<glib::SourceId>,
}

struct Running {
    pipeline: gst::Pipeline,
    camera: Camera,
    mode: Mode,
    generation: u64,
    started: bool,
    used_hw_decode: bool,
    watchdog: Option<glib::SourceId>,
    _bus_watch: gst::bus::BusWatchGuard,
    recording: Option<Recording>,
    snap_pending: Arc<AtomicU32>,
    raw_pending: Arc<AtomicBool>,
    /// Set from the streaming thread as soon as the recording bin errors, so
    /// the tee stops feeding it before the error can travel upstream.
    rec_failed: Arc<AtomicBool>,
}

type EventHandler = Box<dyn Fn(Event)>;
type PaintableHandler = Box<dyn Fn(Option<gdk::Paintable>)>;

pub struct CameraPipeline {
    running: RefCell<Option<Running>>,
    on_event: RefCell<Option<EventHandler>>,
    on_paintable: RefCell<Option<PaintableHandler>>,
    tx: async_channel::Sender<ThreadEvent>,
    generation: Cell<u64>,
    effect: Cell<u32>,
    zoom: Cell<f64>,
    qr: Cell<bool>,
    accel: Cell<Accel>,
    /// Set when hardware decoding failed for the current camera; fall back to CPU.
    hw_decode_broken: Cell<bool>,
    last_camera: RefCell<Option<String>>,
    frames: Arc<AtomicU64>,
    fps: Cell<f64>,
    fps_stamp: Cell<(Instant, u64)>,
    first_frame_sent: Arc<AtomicBool>,
}

impl CameraPipeline {
    pub fn new(accel: Accel) -> Rc<Self> {
        let (tx, rx) = async_channel::unbounded();
        let this = Rc::new(Self {
            running: RefCell::new(None),
            on_event: RefCell::new(None),
            on_paintable: RefCell::new(None),
            tx,
            generation: Cell::new(0),
            effect: Cell::new(0),
            zoom: Cell::new(1.0),
            qr: Cell::new(false),
            accel: Cell::new(accel),
            hw_decode_broken: Cell::new(false),
            last_camera: RefCell::new(None),
            frames: Arc::new(AtomicU64::new(0)),
            fps: Cell::new(0.0),
            fps_stamp: Cell::new((Instant::now(), 0)),
            first_frame_sent: Arc::new(AtomicBool::new(false)),
        });
        let weak = Rc::downgrade(&this);
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                let Some(this) = weak.upgrade() else { break };
                this.handle_thread_event(ev);
            }
        });
        let weak = Rc::downgrade(&this);
        glib::timeout_add_local(Duration::from_millis(500), move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            this.update_fps();
            glib::ControlFlow::Continue
        });
        this
    }

    pub fn connect_event(&self, f: impl Fn(Event) + 'static) {
        *self.on_event.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_paintable(&self, f: impl Fn(Option<gdk::Paintable>) + 'static) {
        *self.on_paintable.borrow_mut() = Some(Box::new(f));
    }

    fn emit(&self, ev: Event) {
        if let Some(f) = self.on_event.borrow().as_ref() {
            f(ev);
        }
    }

    pub fn accel(&self) -> Accel {
        self.accel.get()
    }

    pub fn set_accel(&self, accel: Accel) {
        self.accel.set(accel);
        self.hw_decode_broken.set(false);
    }

    // -- state -------------------------------------------------------------------

    pub fn is_running(&self) -> bool {
        self.running.borrow().is_some()
    }

    /// Frames are flowing.
    pub fn is_active(&self) -> bool {
        self.running.borrow().as_ref().is_some_and(|r| r.started)
    }

    pub fn is_recording(&self) -> bool {
        self.running
            .borrow()
            .as_ref()
            .is_some_and(|r| r.recording.is_some())
    }

    pub fn recording_elapsed(&self) -> Option<Duration> {
        self.running
            .borrow()
            .as_ref()?
            .recording
            .as_ref()
            .filter(|r| !r.stopping)
            .map(|r| r.started.elapsed())
    }

    fn current_generation(&self) -> Option<u64> {
        self.running.borrow().as_ref().map(|r| r.generation)
    }

    pub fn uses_hw_decode(&self) -> bool {
        self.running
            .borrow()
            .as_ref()
            .is_some_and(|r| r.used_hw_decode)
    }

    pub fn fps(&self) -> f64 {
        self.fps.get()
    }

    fn update_fps(&self) {
        let (t0, n0) = self.fps_stamp.get();
        let n = self.frames.load(Ordering::Relaxed);
        let dt = t0.elapsed().as_secs_f64();
        if dt >= 0.9 {
            self.fps.set(if self.is_active() {
                (n - n0) as f64 / dt
            } else {
                0.0
            });
            self.fps_stamp.set((Instant::now(), n));
        }
    }

    // -- lifecycle --------------------------------------------------------------------

    pub fn start(self: &Rc<Self>, camera: &Camera, mode: &Mode) {
        self.stop();
        if self.last_camera.borrow().as_deref() != Some(camera.key.as_str()) {
            self.hw_decode_broken.set(false);
            *self.last_camera.borrow_mut() = Some(camera.key.clone());
        }
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        match self.build(camera, mode, generation) {
            Ok(running) => {
                let pipeline = running.pipeline.clone();
                *self.running.borrow_mut() = Some(running);
                if pipeline.set_state(gst::State::Playing).is_err() {
                    // The bus usually carries a more precise error; give it a moment.
                    let weak = Rc::downgrade(self);
                    glib::idle_add_local_once(move || {
                        if let Some(this) = weak.upgrade()
                            && this.current_generation() == Some(generation)
                            && !this.is_active()
                        {
                            this.fail(ErrorKind::Other, "Could not start the camera.".into());
                        }
                    });
                }
                let weak = Rc::downgrade(self);
                let id = glib::timeout_add_local_once(FIRST_FRAME_TIMEOUT, move || {
                    if let Some(this) = weak.upgrade() {
                        if let Some(r) = this.running.borrow_mut().as_mut() {
                            r.watchdog = None;
                        }
                        if this.is_running() && !this.is_active() {
                            this.fail(
                                ErrorKind::Timeout,
                                "The camera is not sending any frames. Try another camera or resolution."
                                    .into(),
                            );
                        }
                    }
                });
                if let Some(r) = self.running.borrow_mut().as_mut() {
                    r.watchdog = Some(id);
                }
            }
            Err(e) => self.emit(Event::Error(
                ErrorKind::Other,
                format!("Could not create the pipeline: {e:#}"),
            )),
        }
    }

    pub fn stop(&self) {
        let Some(mut running) = self.running.borrow_mut().take() else {
            return;
        };
        if let Some(w) = running.watchdog.take() {
            w.remove();
        }
        if let Some(mut rec) = running.recording.take() {
            if let Some(t) = rec.timeout.take() {
                t.remove();
            }
            // The camera is going away (error, unplug, …) but the recording
            // branch is still alive: let it finish the file properly.
            let saved = finalize_blocking(&running.pipeline, &mut rec, Duration::from_secs(3));
            self.emit(Event::RecordingFinished {
                path: rec.path,
                ok: saved,
                message: if saved {
                    "Camera stopped — video saved".into()
                } else {
                    "Recording interrupted; file may be incomplete".into()
                },
            });
        }
        let _ = running.pipeline.set_state(gst::State::Null);
        if let Some(f) = self.on_paintable.borrow().as_ref() {
            f(None);
        }
        self.fps.set(0.0);
    }

    fn fail(self: &Rc<Self>, kind: ErrorKind, message: String) {
        let (camera, mode, used_hw) = match self.running.borrow().as_ref() {
            Some(r) => (
                r.camera.clone(),
                r.mode.clone(),
                r.used_hw_decode && !r.started,
            ),
            None => return,
        };
        self.stop();
        if used_hw && kind == ErrorKind::Other {
            // Hardware decoding did not work with this camera: retry on the CPU.
            eprintln!(
                "camera-studio: hardware decoding failed ({message}); falling back to the CPU"
            );
            self.hw_decode_broken.set(true);
            self.start(&camera, &mode);
            return;
        }
        self.emit(Event::Error(kind, message));
    }

    fn build(self: &Rc<Self>, camera: &Camera, mode: &Mode, generation: u64) -> Result<Running> {
        let accel = self.accel.get();
        let hw_decode = accel.va_decode && mode.is_mjpeg() && !self.hw_decode_broken.get();
        let src = match &camera.path {
            None => {
                // CAMERA_STUDIO_TEST_PATTERN=smpte is handy for checking colours end to end.
                let pattern =
                    std::env::var("CAMERA_STUDIO_TEST_PATTERN").unwrap_or_else(|_| "ball".into());
                format!(
                    "videotestsrc name=src is-live=true pattern={pattern} ! {}",
                    mode.caps_string()
                )
            }
            Some(path) => format!("v4l2src name=src device={path} ! {}", mode.caps_string()),
        };
        let (w, h) = (mode.width, mode.height);
        // The full camera resolution is pinned at every step of the main path.
        // vapostproc and all GL filters can rescale, and a tee lets each branch
        // take part in choosing the size: without these capsfilters the small
        // QR / effect-preview branches dragged the *whole* stream down (e.g.
        // decoding at 192×108 and the shader stretching it back to 1080p).
        let size = format!("width={w},height={h}");
        let upload = if hw_decode {
            format!(
                "vajpegdec ! vapostproc name=decpp ! video/x-raw(memory:DMABuf),{size} ! glupload name=upload"
            )
        } else if mode.is_mjpeg() {
            "jpegdec idct-method=ifast ! videoconvert ! glupload name=upload".to_string()
        } else {
            "videoconvert ! glupload name=upload".to_string()
        };
        let desc = format!(
            "{src} ! queue name=decq leaky=downstream max-size-buffers=2 max-size-bytes=0 max-size-time=0 \
             ! {upload} ! glcolorconvert name=conv ! video/x-raw(memory:GLMemory),format=RGBA,{size} \
             ! tee name=raw allow-not-linked=true \
             raw. ! glshader name=fx ! video/x-raw(memory:GLMemory),format=RGBA,width={w},height={h} \
             ! tee name=t allow-not-linked=true \
             t. ! queue name=pq leaky=downstream max-size-buffers=1 max-size-bytes=0 max-size-time=0 \
             t. ! valve name=snapvalve drop=true ! queue leaky=downstream max-size-buffers=1 max-size-bytes=0 max-size-time=0 \
                ! gldownload ! video/x-raw,format=RGBA ! appsink name=snap sync=false async=false max-buffers=1 drop=true \
             t. ! valve name=qrvalve drop=true ! queue leaky=downstream max-size-buffers=1 max-size-bytes=0 max-size-time=0 \
                ! glcolorscale ! video/x-raw(memory:GLMemory),width={qw},height={qh} ! gldownload \
                ! videoconvert ! video/x-raw,format=I420 ! zbar cache=true attach-frame=false \
                ! fakesink sync=false async=false \
             raw. ! valve name=rawvalve drop=true ! queue leaky=downstream max-size-buffers=1 max-size-bytes=0 max-size-time=0 \
                ! glcolorscale ! video/x-raw(memory:GLMemory),width={pw},height={ph} ! gldownload \
                ! video/x-raw,format=RGBA ! appsink name=rawsnap sync=false async=false max-buffers=1 drop=true",
            qw = (w / 2) & !1,
            qh = (h / 2) & !1,
            pw = PREVIEW_WIDTH,
            ph = ((PREVIEW_WIDTH * h / w.max(1)) & !1).max(2),
        );
        let pipeline = gst::parse::launch(&desc)
            .context("parse")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("not a pipeline"))?;
        pipeline.set_property("name", "camera");

        // Preview: gtk4paintablesink shares GTK's GL context, so GL textures are
        // displayed without any copy.
        let gtksink = gst::ElementFactory::make("gtk4paintablesink")
            .property("sync", false)
            .build()
            .context("gtk4paintablesink (package gstreamer1.0-gtk4)")?;
        let paintable = gtksink.property::<gdk::Paintable>("paintable");
        let has_gl = paintable
            .property::<Option<gdk::GLContext>>("gl-context")
            .is_some();
        let sink: gst::Element = if has_gl {
            gst::ElementFactory::make("glsinkbin")
                .property("sink", &gtksink)
                .build()?
        } else {
            let bin = gst::Bin::new();
            let down = gst::ElementFactory::make("gldownload").build()?;
            let conv = gst::ElementFactory::make("videoconvert").build()?;
            bin.add_many([&down, &conv, &gtksink])?;
            gst::Element::link_many([&down, &conv, &gtksink])?;
            bin.add_pad(&gst::GhostPad::with_target(
                &down.static_pad("sink").unwrap(),
            )?)?;
            bin.upcast()
        };
        pipeline.add(&sink)?;
        pipeline.by_name("pq").unwrap().link(&sink)?;

        // Effect + zoom shader.
        let fx = pipeline.by_name("fx").unwrap();
        fx.set_property("fragment", effects::SHADER);
        fx.set_property("uniforms", self.uniforms());

        pipeline
            .by_name("qrvalve")
            .unwrap()
            .set_property("drop", !self.qr.get());

        // Everything upstream of "decq" has fixed caps; stop RECONFIGURE events
        // there so the capture thread never renegotiates while we change the
        // downstream (e.g. linking the recording branch).
        pipeline
            .by_name("decq")
            .unwrap()
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::EVENT_UPSTREAM, |_, info| {
                match info.event() {
                    Some(e) if e.type_() == gst::EventType::Reconfigure => {
                        gst::PadProbeReturn::Drop
                    }
                    _ => gst::PadProbeReturn::Ok,
                }
            });

        // Frame counter + first-frame notification.
        self.frames.store(0, Ordering::Relaxed);
        self.fps_stamp.set((Instant::now(), 0));
        self.first_frame_sent.store(false, Ordering::Relaxed);
        let frames = self.frames.clone();
        let first = self.first_frame_sent.clone();
        let tx = self.tx.clone();
        pipeline
            .by_name("pq")
            .unwrap()
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                frames.fetch_add(1, Ordering::Relaxed);
                if !first.swap(true, Ordering::Relaxed) {
                    let _ = tx.try_send(ThreadEvent::FirstFrame(generation));
                }
                gst::PadProbeReturn::Ok
            });

        // Photos: open the valve, take the requested number of frames, close it.
        let snap_pending = Arc::new(AtomicU32::new(0));
        let valve = pipeline.by_name("snapvalve").unwrap();
        let appsink = pipeline
            .by_name("snap")
            .unwrap()
            .downcast::<gst_app::AppSink>()
            .unwrap();
        let pending = snap_pending.clone();
        let tx = self.tx.clone();
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let took = pending
                        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                        .is_ok();
                    if took {
                        let _ = tx.try_send(ThreadEvent::Photo(generation, sample));
                    }
                    if pending.load(Ordering::Acquire) == 0 {
                        valve.set_property("drop", true);
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        // Effect previews: one small frame from before the shader, on request.
        let raw_pending = Arc::new(AtomicBool::new(false));
        let raw_valve = pipeline.by_name("rawvalve").unwrap();
        let raw_sink = pipeline
            .by_name("rawsnap")
            .unwrap()
            .downcast::<gst_app::AppSink>()
            .unwrap();
        let pending = raw_pending.clone();
        let tx = self.tx.clone();
        raw_sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if pending.swap(false, Ordering::AcqRel) {
                        let _ = tx.try_send(ThreadEvent::RawFrame(generation, sample));
                    }
                    raw_valve.set_property("drop", true);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        // Recording errors are flagged synchronously, in the thread that posts
        // them, before the failing element returns an error flow to the tee.
        let rec_failed = Arc::new(AtomicBool::new(false));
        let failed = rec_failed.clone();
        pipeline.bus().unwrap().set_sync_handler(move |_, msg| {
            if let gst::MessageView::Error(_) = msg.view()
                && msg.src().is_some_and(in_recording_bin_name)
            {
                failed.store(true, Ordering::Release);
            }
            gst::BusSyncReply::Pass
        });

        let weak = Rc::downgrade(self);
        let bus_watch = pipeline.bus().unwrap().add_watch_local(move |_, msg| {
            if let Some(this) = weak.upgrade() {
                this.on_bus_message(msg, generation);
            }
            glib::ControlFlow::Continue
        })?;

        if let Some(f) = self.on_paintable.borrow().as_ref() {
            f(Some(paintable));
        }
        Ok(Running {
            pipeline,
            camera: camera.clone(),
            mode: mode.clone(),
            generation,
            started: false,
            used_hw_decode: hw_decode,
            watchdog: None,
            _bus_watch: bus_watch,
            recording: None,
            snap_pending,
            raw_pending,
            rec_failed,
        })
    }

    fn handle_thread_event(self: &Rc<Self>, ev: ThreadEvent) {
        let current = self.running.borrow().as_ref().map(|r| r.generation);
        match ev {
            ThreadEvent::FirstFrame(g) if Some(g) == current => {
                if let Some(r) = self.running.borrow_mut().as_mut() {
                    r.started = true;
                    if let Some(w) = r.watchdog.take() {
                        w.remove();
                    }
                }
                self.emit(Event::Started);
            }
            ThreadEvent::Photo(g, sample) if Some(g) == current => self.emit(Event::Photo(sample)),
            ThreadEvent::RawFrame(g, sample) if Some(g) == current => {
                self.emit(Event::EffectSource(sample))
            }
            _ => {}
        }
    }

    // -- live controls ------------------------------------------------------------------

    fn uniforms(&self) -> gst::Structure {
        gst::Structure::builder("uniforms")
            .field("zoom", self.zoom.get() as f32)
            .field("effect", self.effect.get() as f32)
            .build()
    }

    fn push_uniforms(&self) {
        if let Some(r) = self.running.borrow().as_ref()
            && let Some(fx) = r.pipeline.by_name("fx")
        {
            fx.set_property("uniforms", self.uniforms());
        }
    }

    pub fn set_effect(&self, id: &str) {
        self.effect.set(effects::get(id).code);
        self.push_uniforms();
    }

    pub fn zoom(&self) -> f64 {
        self.zoom.get()
    }

    pub fn set_zoom(&self, zoom: f64) {
        self.zoom.set(zoom.clamp(1.0, 8.0));
        self.push_uniforms();
    }

    pub fn set_qr_enabled(&self, enabled: bool) {
        self.qr.set(enabled);
        if let Some(r) = self.running.borrow().as_ref() {
            r.pipeline
                .by_name("qrvalve")
                .unwrap()
                .set_property("drop", !enabled);
        }
    }

    /// Request `count` processed frames; they arrive as `Event::Photo`.
    pub fn request_photos(&self, count: u32) -> bool {
        let running = self.running.borrow();
        let Some(r) = running.as_ref().filter(|r| r.started) else {
            return false;
        };
        r.snap_pending.fetch_add(count, Ordering::AcqRel);
        r.pipeline
            .by_name("snapvalve")
            .unwrap()
            .set_property("drop", false);
        true
    }

    /// Every step of the main path must run at the camera resolution.
    /// Returns the offending steps (empty = OK). Used by the self tests.
    pub fn check_full_resolution(&self) -> Vec<String> {
        let running = self.running.borrow();
        let Some(r) = running.as_ref() else {
            return vec!["camera not running".into()];
        };
        let (w, h) = (r.mode.width, r.mode.height);
        let mut pads: Vec<(String, Option<gst::Pad>)> = ["decpp", "upload", "conv", "fx", "pq"]
            .iter()
            .map(|n| {
                (
                    n.to_string(),
                    r.pipeline.by_name(n).and_then(|e| e.static_pad("src")),
                )
            })
            .collect();
        // The preview sink itself (inside glsinkbin).
        let preview = r
            .pipeline
            .by_name("pq")
            .and_then(|q| q.static_pad("src"))
            .and_then(|p| p.peer());
        pads.push(("preview".into(), preview));
        let mut bad = Vec::new();
        for (name, pad) in pads {
            let Some(pad) = pad else { continue }; // e.g. no vapostproc on the CPU path
            let Some(caps) = pad.current_caps() else {
                bad.push(format!("{name}: not negotiated"));
                continue;
            };
            let s = caps.structure(0).unwrap();
            let size = (
                s.get::<i32>("width").unwrap_or(0),
                s.get::<i32>("height").unwrap_or(0),
            );
            if size != (w, h) {
                bad.push(format!("{name}: {}x{} (expected {w}x{h})", size.0, size.1));
            }
        }
        bad
    }

    /// Negotiated caps along the preview path (diagnostics for `--probe`).
    pub fn debug_caps(&self) -> String {
        use gst::prelude::*;
        let running = self.running.borrow();
        let Some(r) = running.as_ref() else {
            return "not running".into();
        };
        let mut out = Vec::new();
        for (name, pad) in [("fx", "src"), ("pq", "src")] {
            if let Some(caps) = r
                .pipeline
                .by_name(name)
                .and_then(|e| e.static_pad(pad))
                .and_then(|p| p.current_caps())
            {
                out.push(format!("{name}.{pad}: {caps}"));
            }
        }
        // Inside glsinkbin: every element's sink caps down to gtk4paintablesink.
        let mut it = r.pipeline.iterate_recurse();
        while let Ok(Some(e)) = it.next() {
            let f = e
                .factory()
                .map(|f| f.name().to_string())
                .unwrap_or_default();
            if [
                "gtk4paintablesink",
                "glcolorbalance",
                "glcolorconvert",
                "glupload",
            ]
            .contains(&f.as_str())
                && e.has_as_ancestor(&r.pipeline.by_name("pq").unwrap().parent().unwrap())
                && let Some(c) = e.static_pad("sink").and_then(|p| p.current_caps())
            {
                let s = c.structure(0).unwrap();
                out.push(format!(
                    "{} ({f}) sink: {}x{} {:?}",
                    e.name(),
                    s.get::<i32>("width").unwrap_or(0),
                    s.get::<i32>("height").unwrap_or(0),
                    c.features(0).map(|f| f.to_string())
                ));
            }
        }
        out.join("\n")
    }

    /// Request one small unprocessed frame; it arrives as `Event::EffectSource`.
    pub fn request_effect_source(&self) {
        let running = self.running.borrow();
        let Some(r) = running.as_ref().filter(|r| r.started) else {
            return;
        };
        r.raw_pending.store(true, Ordering::Release);
        r.pipeline
            .by_name("rawvalve")
            .unwrap()
            .set_property("drop", false);
    }

    // -- recording --------------------------------------------------------------------------

    pub fn start_recording(self: &Rc<Self>, opts: &RecordOptions) -> Result<()> {
        let mut running = self.running.borrow_mut();
        let r = running
            .as_mut()
            .filter(|r| r.started)
            .ok_or_else(|| anyhow!("Camera not ready"))?;
        if r.recording.is_some() {
            return Err(anyhow!("Already recording"));
        }
        let desc = recording_description(&r.mode, opts, self.accel.get());
        let bin = gst::parse::bin_from_description(&desc, true)
            .with_context(|| "Could not create the recorder".to_string())?;
        bin.set_property("message-forward", true);
        bin.set_property("name", format!("{RECBIN}{}", r.generation));
        r.rec_failed.store(false, Ordering::Release);
        bin.by_name("fsink")
            .unwrap()
            .set_property("location", opts.path.to_str().unwrap_or_default());
        if let (Some(asrc), Some(dev)) = (bin.by_name("asrc"), opts.audio_device.as_deref()) {
            asrc.set_property("device", dev);
        }
        let pipeline = r.pipeline.clone();
        pipeline.add(&bin)?;
        // Start the file at t=0: shift the running time of everything entering the bin.
        let now = pipeline
            .clock()
            .map(|c| c.time())
            .zip(pipeline.base_time())
            .map(|(t, b)| t.saturating_sub(b).nseconds() as i64)
            .unwrap_or(0);
        let sinkpad = bin
            .static_pad("sink")
            .ok_or_else(|| anyhow!("no sink pad"))?;
        sinkpad.set_offset(-now);
        if let Some(asrc) = bin.by_name("asrc") {
            asrc.static_pad("src").unwrap().set_offset(-now);
        }
        if bin.sync_state_with_parent().is_err() {
            let _ = bin.set_state(gst::State::Null);
            let _ = pipeline.remove(&bin);
            return Err(anyhow!("Could not start the recorder"));
        }
        let tee = pipeline.by_name("t").unwrap();
        let tee_pad = tee.request_pad_simple("src_%u").unwrap();
        // Frames captured slightly before `now` would get a negative running
        // time in the file and be dropped by the muxer *after* encoding — and
        // the first encoded frame is the only keyframe of the first GOP, so the
        // video would start with ~2 s of undecodable (green) frames. Drop them
        // here instead, before the encoder, so the first frame it sees is in
        // the recording and becomes the IDR frame.
        let start = gst::ClockTime::from_nseconds(now.max(0) as u64);
        tee_pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            let Some(pts) = info.buffer().and_then(|b| b.pts()) else {
                return gst::PadProbeReturn::Ok;
            };
            let running_time = pad.sticky_event::<gst::event::Segment>(0).and_then(|e| {
                e.segment()
                    .downcast_ref::<gst::ClockTime>()
                    .and_then(|s| s.to_running_time(pts))
            });
            match running_time {
                Some(rt) if rt < start => gst::PadProbeReturn::Drop,
                _ => gst::PadProbeReturn::Remove,
            }
        });
        // Once the recording branch has failed, stop feeding it: an error flow
        // returned to the tee would otherwise stop the camera itself.
        let failed = r.rec_failed.clone();
        tee_pad.add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            if failed.load(Ordering::Acquire) {
                gst::PadProbeReturn::Drop
            } else {
                gst::PadProbeReturn::Ok
            }
        });
        if let Err(e) = tee_pad.link(&sinkpad) {
            tee.release_request_pad(&tee_pad);
            let _ = bin.set_state(gst::State::Null);
            let _ = pipeline.remove(&bin);
            return Err(anyhow!("Could not link the recording branch: {e:?}"));
        }
        r.recording = Some(Recording {
            bin,
            tee_pad,
            path: opts.path.clone(),
            started: Instant::now(),
            stopping: false,
            timeout: None,
        });
        Ok(())
    }

    pub fn stop_recording(self: &Rc<Self>) {
        let mut running = self.running.borrow_mut();
        let Some(r) = running.as_mut() else { return };
        let Some(rec) = r.recording.as_mut() else {
            return;
        };
        if rec.stopping {
            return;
        }
        rec.stopping = true;
        let tee = r.pipeline.by_name("t").unwrap();
        let sinkpad = rec.bin.static_pad("sink").unwrap();
        rec.tee_pad
            .add_probe(gst::PadProbeType::IDLE, move |pad, _| {
                let _ = pad.unlink(&sinkpad);
                tee.release_request_pad(pad);
                sinkpad.send_event(gst::event::Eos::new());
                gst::PadProbeReturn::Remove
            });
        if let Some(asrc) = rec.bin.by_name("asrc") {
            asrc.send_event(gst::event::Eos::new());
        }
        let weak = Rc::downgrade(self);
        rec.timeout = Some(glib::timeout_add_local_once(
            Duration::from_secs(8),
            move || {
                if let Some(this) = weak.upgrade() {
                    // This source is finished: forget it before finishing.
                    if let Some(rec) = this
                        .running
                        .borrow_mut()
                        .as_mut()
                        .and_then(|r| r.recording.as_mut())
                    {
                        rec.timeout = None;
                    }
                    this.finish_recording(
                        false,
                        "Finishing the recording timed out; the file may be incomplete.".into(),
                    );
                }
            },
        ));
    }

    fn finish_recording(&self, ok: bool, message: String) {
        let rec = {
            let mut running = self.running.borrow_mut();
            let Some(r) = running.as_mut() else { return };
            let Some(mut rec) = r.recording.take() else {
                return;
            };
            if let Some(t) = rec.timeout.take() {
                t.remove();
            }
            if !rec.stopping {
                // Error while recording: detach from the tee first.
                let tee = r.pipeline.by_name("t").unwrap();
                if let Some(peer) = rec.tee_pad.peer() {
                    let _ = rec.tee_pad.unlink(&peer);
                }
                tee.release_request_pad(&rec.tee_pad);
            }
            let _ = rec.bin.set_state(gst::State::Null);
            let _ = r.pipeline.remove(&rec.bin);
            rec
        };
        self.emit(Event::RecordingFinished {
            path: rec.path,
            ok,
            message,
        });
    }

    // -- bus ------------------------------------------------------------------------------------

    fn in_recording_bin(&self, obj: Option<&gst::Object>) -> bool {
        let running = self.running.borrow();
        let Some(bin) = running
            .as_ref()
            .and_then(|r| r.recording.as_ref())
            .map(|rec| rec.bin.clone())
        else {
            return false;
        };
        obj.is_some_and(|o| o == bin.upcast_ref::<gst::Object>() || o.has_as_ancestor(&bin))
    }

    fn on_bus_message(self: &Rc<Self>, msg: &gst::Message, generation: u64) {
        use gst::MessageView;
        let Some(pipeline) = self
            .running
            .borrow()
            .as_ref()
            .filter(|r| r.generation == generation)
            .map(|r| r.pipeline.clone())
        else {
            return;
        };
        // Late messages from a recording branch that was already removed (one
        // failure often posts several errors) must not be taken for camera errors.
        if let Some(src) = msg.src()
            && src != pipeline.upcast_ref::<gst::Object>()
            && !src.has_as_ancestor(&pipeline)
        {
            return;
        }
        match msg.view() {
            MessageView::Error(err) => {
                if self.in_recording_bin(msg.src()) {
                    let text = err.error().to_string();
                    let src = msg.src().map(|s| s.name().to_string()).unwrap_or_default();
                    let message = if src == "asrc" {
                        format!("Microphone error, recording stopped: {text}")
                    } else {
                        format!("Recording error: {text}")
                    };
                    self.finish_recording(false, message);
                    return;
                }
                let started = self.is_active();
                let (kind, message) = classify_error(&err.error(), err.debug().as_deref(), started);
                self.fail(kind, message);
            }
            MessageView::Eos(_) => self.fail(
                ErrorKind::Disconnected,
                "The camera was disconnected.".into(),
            ),
            MessageView::Element(e) => {
                let Some(s) = e.structure() else { return };
                if s.name() == "barcode" {
                    let kind = s.get::<String>("type").unwrap_or_default();
                    let data = s.get::<String>("symbol").unwrap_or_default();
                    self.emit(Event::Barcode(kind, data));
                } else if s.name() == "GstBinForwarded"
                    && let Ok(inner) = s.get::<gst::Message>("message")
                    && matches!(inner.view(), MessageView::Eos(_))
                    && self.in_recording_bin(inner.src())
                {
                    self.finish_recording(true, String::new());
                }
            }
            MessageView::Latency(_) => {
                if let Some(r) = self.running.borrow().as_ref() {
                    let _ = r.pipeline.recalculate_latency();
                }
            }
            MessageView::Warning(w) => {
                eprintln!("camera-studio: GStreamer warning: {}", w.error());
            }
            _ => {}
        }
    }
}

fn bitrate(mode: &Mode, quality: &str) -> u32 {
    let bpp = match quality {
        // bits per pixel per frame; "high" ≈ 17 Mbps at 1080p30
        "low" => 0.07,
        "medium" => 0.14,
        _ => 0.27,
    };
    let rate = mode.width as f64 * mode.height as f64 * mode.fps().max(15.0) * bpp;
    rate.clamp(500_000.0, 25_000_000.0) as u32
}

pub fn recording_description(mode: &Mode, opts: &RecordOptions, accel: Accel) -> String {
    let bps = bitrate(mode, &opts.quality);
    let kbps = bps / 1000;
    let gop = (mode.fps() * 2.0).round().max(1.0) as u32;
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    let webm = opts.container == "webm";
    let hw = if webm { accel.va_vp9 } else { accel.va_h264 };
    // GPU encoders: the RGBA frame is read back once and vapostproc converts,
    // mirrors and uploads it on the GPU (cheapest measured path: letting the
    // encoder upload NV12 itself costs ~7% more CPU, and exporting the GL
    // texture as DMABuf yields black frames with the Intel driver).
    // CPU encoders: colour conversion and mirroring still run in GL.
    let video = if hw {
        let dir = if opts.mirror { "horiz" } else { "identity" };
        let enc = if webm {
            format!("vavp9enc rate-control=vbr target-usage=1 bitrate={kbps} key-int-max={gop}")
        } else {
            // target-usage=1 is the encoder's best-quality preset: at the same
            // bitrate it keeps noticeably more detail than the default (4).
            format!(
                "vah264enc rate-control=vbr target-usage=1 bitrate={kbps} key-int-max={gop} ! h264parse"
            )
        };
        format!(
            "gldownload ! video/x-raw,format=RGBA ! vapostproc video-direction={dir} \
             ! video/x-raw(memory:VAMemory),format=NV12 ! {enc}"
        )
    } else {
        let flip = if opts.mirror {
            "glvideoflip method=horizontal-flip ! "
        } else {
            ""
        };
        let enc = if webm {
            format!(
                "vp8enc deadline=1 cpu-used=8 threads={threads} end-usage=cbr target-bitrate={bps} \
                 keyframe-max-dist={gop} lag-in-frames=0"
            )
        } else {
            format!(
                "openh264enc complexity=low rate-control=bitrate bitrate={bps} gop-size={gop} \
                 multi-thread={threads} usage-type=camera ! h264parse"
            )
        };
        format!(
            "{flip}glcolorconvert ! video/x-raw(memory:GLMemory),format=I420 ! gldownload \
             ! video/x-raw,format=I420 ! {enc}"
        )
    };
    let mux = if webm {
        "webmmux name=mux".to_string()
    } else {
        // Fragmented while recording (a crash never loses the file), rewritten as a
        // regular MP4 with a complete index when the recording ends.
        "mp4mux name=mux fragment-duration=1000 fragment-mode=first-moov-then-finalise".to_string()
    };
    let mut desc = format!(
        "queue max-size-buffers=0 max-size-bytes=0 max-size-time=2000000000 leaky=downstream \
         ! {video} ! queue ! {mux} ! filesink name=fsink async=false"
    );
    if opts.audio {
        let aenc = if webm {
            "opusenc bitrate=128000"
        } else {
            "voaacenc bitrate=128000 ! aacparse"
        };
        desc.push_str(&format!(
            " pulsesrc name=asrc client-name=CameraStudio provide-clock=false do-timestamp=true \
             ! queue max-size-buffers=0 max-size-bytes=0 max-size-time=3000000000 \
             ! audioconvert ! audioresample ! audio/x-raw,channels=2 ! {aenc} ! queue ! mux."
        ));
    }
    desc
}

pub fn classify_error(
    err: &glib::Error,
    debug: Option<&str>,
    started: bool,
) -> (ErrorKind, String) {
    let text = format!("{} {}", err.message(), debug.unwrap_or("")).to_lowercase();
    if err.matches(gst::ResourceError::Busy) || text.contains("busy") {
        return (
            ErrorKind::Busy,
            "The camera is being used by another application (for example a browser or a video-call app).".into(),
        );
    }
    if err.matches(gst::ResourceError::NotAuthorized) || text.contains("permission") {
        return (
            ErrorKind::Permission,
            "No permission to use the camera. Check that you are in the 'video' group.".into(),
        );
    }
    if err.matches(gst::ResourceError::NotFound)
        || text.contains("no such")
        || (started && (text.contains("read") || text.contains("no device")))
    {
        return (
            ErrorKind::Disconnected,
            "The camera was disconnected.".into(),
        );
    }
    if text.contains("negotiat") {
        return (
            ErrorKind::Other,
            "The camera does not support the selected mode. Choose another resolution.".into(),
        );
    }
    (ErrorKind::Other, err.message().to_string())
}

/// Is there any microphone at all? (Recording silently fails otherwise.)
pub fn has_microphone() -> bool {
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Audio/Source"), None);
    if monitor.start().is_err() {
        return true; // unknown: let pulsesrc try
    }
    let found = monitor.devices().iter().any(|d| {
        d.properties()
            .and_then(|p| p.get::<String>("device.class").ok())
            .map(|c| c != "monitor")
            .unwrap_or(true)
    });
    monitor.stop();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode() -> Mode {
        Mode {
            width: 1920,
            height: 1080,
            fps_n: 30,
            fps_d: 1,
            media: "image/jpeg".into(),
            format: None,
        }
    }

    #[test]
    fn recording_descriptions_parse() {
        gst::init().unwrap();
        for container in ["mp4", "webm"] {
            for audio in [true, false] {
                for accel in [
                    Accel::detect(true),
                    Accel {
                        va_decode: false,
                        va_h264: false,
                        va_vp9: false,
                    },
                ] {
                    let opts = RecordOptions {
                        path: "/tmp/x".into(),
                        container: container.into(),
                        quality: "high".into(),
                        audio,
                        audio_device: None,
                        mirror: true,
                    };
                    let desc = recording_description(&mode(), &opts, accel);
                    gst::parse::bin_from_description(&desc, true).unwrap_or_else(|e| {
                        panic!("{container} audio={audio} {accel:?}: {e}\n{desc}")
                    });
                }
            }
        }
    }

    #[test]
    fn bitrate_bounds() {
        let small = Mode {
            width: 320,
            height: 240,
            ..mode()
        };
        assert_eq!(bitrate(&small, "low"), 500_000);
        assert!(bitrate(&mode(), "high") <= 25_000_000);
    }
}

const RECBIN: &str = "recbin";

fn in_recording_bin_name(obj: &gst::Object) -> bool {
    let mut cur = Some(obj.clone());
    while let Some(o) = cur {
        if o.name().starts_with(RECBIN) {
            return true;
        }
        cur = o.parent();
    }
    false
}

/// Send EOS through a recording branch and wait (blocking, bounded) until the
/// muxer has finished the file. Used when the camera stops under a recording.
fn finalize_blocking(pipeline: &gst::Pipeline, rec: &mut Recording, timeout: Duration) -> bool {
    use gst::MessageView;
    let sinkpad = rec.bin.static_pad("sink").unwrap();
    if !rec.stopping {
        rec.stopping = true;
        if let Some(peer) = rec.tee_pad.peer() {
            let _ = rec.tee_pad.unlink(&peer);
        }
        if let Some(tee) = rec.tee_pad.parent_element() {
            tee.release_request_pad(&rec.tee_pad);
        }
        sinkpad.send_event(gst::event::Eos::new());
        if let Some(asrc) = rec.bin.by_name("asrc") {
            asrc.send_event(gst::event::Eos::new());
        }
    }
    let Some(bus) = pipeline.bus() else {
        return false;
    };
    let deadline = Instant::now() + timeout;
    let mut done = false;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Some(msg) = bus.timed_pop_filtered(
            gst::ClockTime::from_nseconds(left.as_nanos() as u64),
            &[gst::MessageType::Element, gst::MessageType::Error],
        ) else {
            break;
        };
        let from_bin = |o: Option<&gst::Object>| {
            o.is_some_and(|o| {
                o.has_as_ancestor(&rec.bin) || o == rec.bin.upcast_ref::<gst::Object>()
            })
        };
        match msg.view() {
            MessageView::Element(e)
                if e.structure().is_some_and(|s| s.name() == "GstBinForwarded") =>
            {
                if let Ok(inner) = e.structure().unwrap().get::<gst::Message>("message")
                    && matches!(inner.view(), MessageView::Eos(_))
                    && from_bin(inner.src())
                {
                    done = true;
                    break;
                }
            }
            MessageView::Error(_) if from_bin(msg.src()) => break,
            _ => {}
        }
    }
    let _ = rec.bin.set_state(gst::State::Null);
    let _ = pipeline.remove(&rec.bin);
    done
}
