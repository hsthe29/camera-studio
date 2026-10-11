//! In-app video player. playbin picks the VA-API decoders automatically, and
//! frames reach GTK as GL textures through gtk4paintablesink.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use gst::prelude::*;
use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::storage::format_duration;
use crate::viewfinder::Viewfinder;

pub struct VideoPlayer {
    pub widget: gtk::Overlay,
    view: Viewfinder,
    playbin: RefCell<Option<gst::Element>>,
    watch: RefCell<Option<gst::bus::BusWatchGuard>>,
    tick: RefCell<Option<glib::SourceId>>,
    duration: Cell<Option<gst::ClockTime>>,
    playing: Cell<bool>,
    play_button: gtk::Button,
    scale: gtk::Scale,
    position: gtk::Label,
    total: gtk::Label,
    mute: gtk::ToggleButton,
}

impl VideoPlayer {
    pub fn new() -> Rc<Self> {
        let view = Viewfinder::default();
        let widget = gtk::Overlay::new();
        widget.set_child(Some(&view));
        let bar = gtk::Box::builder()
            .spacing(8)
            .valign(gtk::Align::End)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(12)
            .build();
        bar.add_css_class("toolbar");
        bar.add_css_class("osd");
        bar.add_css_class("player-bar");
        let play_button = gtk::Button::builder()
            .icon_name("media-playback-start-symbolic")
            .tooltip_text("Play / Pause (Space)")
            .build();
        play_button.add_css_class("flat");
        let position = gtk::Label::new(Some("00:00"));
        let total = gtk::Label::new(Some("--:--"));
        for l in [&position, &total] {
            l.add_css_class("numeric");
            l.add_css_class("caption");
        }
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.1);
        scale.set_hexpand(true);
        scale.set_draw_value(false);
        let mute = gtk::ToggleButton::builder()
            .icon_name("audio-volume-high-symbolic")
            .tooltip_text("Mute")
            .build();
        mute.add_css_class("flat");
        bar.append(&play_button);
        bar.append(&position);
        bar.append(&scale);
        bar.append(&total);
        bar.append(&mute);
        widget.add_overlay(&bar);

        let this = Rc::new(Self {
            widget,
            view,
            playbin: RefCell::new(None),
            watch: RefCell::new(None),
            tick: RefCell::new(None),
            duration: Cell::new(None),
            playing: Cell::new(false),
            play_button,
            scale,
            position,
            total,
            mute,
        });
        this.play_button.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.toggle()
        ));
        this.scale.connect_change_value(glib::clone!(
            #[weak]
            this,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, _, v| {
                this.seek(gst::ClockTime::from_seconds_f64(v.max(0.0)));
                this.position.set_label(&format_duration(v));
                glib::Propagation::Proceed
            }
        ));
        this.mute.connect_toggled(glib::clone!(
            #[weak]
            this,
            move |b| {
                b.set_icon_name(if b.is_active() {
                    "audio-volume-muted-symbolic"
                } else {
                    "audio-volume-high-symbolic"
                });
                if let Some(p) = this.playbin.borrow().as_ref() {
                    p.set_property("mute", b.is_active());
                }
            }
        ));
        let click = gtk::GestureClick::new();
        click.connect_released(glib::clone!(
            #[weak]
            this,
            move |_, _, _, _| this.toggle()
        ));
        this.view.add_controller(click);
        this
    }

    pub fn load(self: &Rc<Self>, path: &Path) {
        self.stop();
        let Ok(gtksink) = gst::ElementFactory::make("gtk4paintablesink").build() else {
            return;
        };
        let paintable = gtksink.property::<gdk::Paintable>("paintable");
        let has_gl = paintable
            .property::<Option<gdk::GLContext>>("gl-context")
            .is_some();
        let sink = if has_gl {
            gst::ElementFactory::make("glsinkbin")
                .property("sink", &gtksink)
                .build()
                .ok()
        } else {
            Some(gtksink)
        };
        let Ok(uri) = glib::filename_to_uri(path, None) else {
            return;
        };
        let Ok(playbin) = gst::ElementFactory::make("playbin")
            .property("uri", uri.as_str())
            .property("video-sink", &sink)
            .property("mute", self.mute.is_active())
            .build()
        else {
            return;
        };
        let weak = Rc::downgrade(self);
        let watch = playbin.bus().unwrap().add_watch_local(move |_, msg| {
            if let Some(this) = weak.upgrade() {
                this.on_message(msg);
            }
            glib::ControlFlow::Continue
        });
        *self.watch.borrow_mut() = watch.ok();
        self.view.set_paintable(Some(&paintable));
        self.duration.set(None);
        self.scale.set_value(0.0);
        self.position.set_label("00:00");
        self.total.set_label("--:--");
        let _ = playbin.set_state(gst::State::Paused); // preroll shows the first frame
        *self.playbin.borrow_mut() = Some(playbin);
    }

    pub fn stop(&self) {
        if let Some(id) = self.tick.borrow_mut().take() {
            id.remove();
        }
        if let Some(p) = self.playbin.borrow_mut().take() {
            let _ = p.set_state(gst::State::Null);
        }
        self.watch.borrow_mut().take();
        self.view.set_paintable(None);
        self.set_playing(false);
    }

    pub fn toggle(self: &Rc<Self>) {
        let Some(p) = self.playbin.borrow().clone() else {
            return;
        };
        let playing = !self.playing.get();
        let _ = p.set_state(if playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        });
        self.set_playing(playing);
        if playing && self.tick.borrow().is_none() {
            let weak = Rc::downgrade(self);
            *self.tick.borrow_mut() = Some(glib::timeout_add_local(
                Duration::from_millis(100),
                move || {
                    let Some(this) = weak.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    this.update_position();
                    glib::ControlFlow::Continue
                },
            ));
        } else if !playing && let Some(id) = self.tick.borrow_mut().take() {
            id.remove();
        }
    }

    fn set_playing(&self, playing: bool) {
        self.playing.set(playing);
        self.play_button.set_icon_name(if playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
    }

    fn seek(&self, pos: gst::ClockTime) {
        if let Some(p) = self.playbin.borrow().as_ref() {
            let pos = self.duration.get().map(|d| pos.min(d)).unwrap_or(pos);
            let _ = p.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE, pos);
        }
    }

    fn query_duration(&self) {
        let Some(p) = self.playbin.borrow().clone() else {
            return;
        };
        if let Some(d) = p
            .query_duration::<gst::ClockTime>()
            .filter(|d| !d.is_zero())
        {
            self.duration.set(Some(d));
            self.scale.set_range(0.0, d.seconds_f64());
            self.total.set_label(&format_duration(d.seconds_f64()));
        }
    }

    fn update_position(&self) {
        if self.duration.get().is_none() {
            self.query_duration();
        }
        let pos = self
            .playbin
            .borrow()
            .as_ref()
            .and_then(|p| p.query_position::<gst::ClockTime>());
        if let Some(pos) = pos {
            self.scale.set_value(pos.seconds_f64());
            self.position.set_label(&format_duration(pos.seconds_f64()));
        }
    }

    fn on_message(self: &Rc<Self>, msg: &gst::Message) {
        use gst::MessageView;
        match msg.view() {
            MessageView::Eos(_) => {
                if self.playing.get() {
                    self.toggle();
                }
                self.seek(gst::ClockTime::ZERO);
                self.scale.set_value(0.0);
                self.position.set_label("00:00");
            }
            MessageView::AsyncDone(_) | MessageView::DurationChanged(_) => {
                self.query_duration();
                self.update_position();
            }
            MessageView::Error(e) => {
                eprintln!("camera-studio: playback error: {}", e.error());
                self.stop();
            }
            _ => {}
        }
    }
}
