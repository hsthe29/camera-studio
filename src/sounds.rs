//! Short UI sounds through GStreamer, prerolled so they start instantly.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gst::prelude::*;
use gtk::glib;

const DIRS: &[&str] = &[
    "/usr/share/sounds/freedesktop/stereo",
    "/usr/share/sounds/Yaru/stereo",
];

fn files(name: &str) -> &'static [&'static str] {
    match name {
        "shutter" => &["camera-shutter.oga"],
        "tick" => &["audio-volume-change.oga", "bell.oga"],
        "record-start" => &["message.oga", "bell.oga"],
        "record-stop" => &["complete.oga", "message.oga"],
        "detected" => &["message-new-instant.oga", "message.oga"],
        _ => &[],
    }
}

fn find(name: &str) -> Option<PathBuf> {
    files(name)
        .iter()
        .flat_map(|f| DIRS.iter().map(move |d| PathBuf::from(d).join(f)))
        .find(|p| p.exists())
}

struct Player {
    playbin: gst::Element,
    _watch: gst::bus::BusWatchGuard,
    release: RefCell<Option<glib::SourceId>>,
}

#[derive(Default)]
pub struct Sounds {
    pub enabled: Cell<bool>,
    players: RefCell<HashMap<&'static str, Rc<Player>>>,
}

impl Sounds {
    pub fn new(enabled: bool) -> Rc<Self> {
        let s = Rc::new(Self::default());
        s.enabled.set(enabled);
        s
    }

    fn get(&self, name: &'static str) -> Option<Rc<Player>> {
        if let Some(p) = self.players.borrow().get(name) {
            return Some(p.clone());
        }
        let path = find(name)?;
        let playbin = gst::ElementFactory::make("playbin")
            .property("uri", glib::filename_to_uri(&path, None).ok()?.as_str())
            .property_from_str("flags", "audio")
            .build()
            .ok()?;
        let weak_bin = playbin.downgrade();
        let release: RefCell<Option<glib::SourceId>> = RefCell::new(None);
        let watch = playbin
            .bus()?
            .add_watch_local(move |_, msg| {
                if let (gst::MessageView::Eos(_), Some(bin)) = (msg.view(), weak_bin.upgrade()) {
                    let _ = bin.set_state(gst::State::Paused);
                    let _ = bin.seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
                }
                glib::ControlFlow::Continue
            })
            .ok()?;
        let player = Rc::new(Player {
            playbin,
            _watch: watch,
            release,
        });
        self.players.borrow_mut().insert(name, player.clone());
        Some(player)
    }

    pub fn preload(&self, name: &'static str) {
        if let Some(p) = self.get(name) {
            let _ = p.playbin.set_state(gst::State::Paused);
        }
    }

    pub fn play(&self, name: &'static str) {
        if !self.enabled.get() {
            return;
        }
        let Some(p) = self.get(name) else { return };
        if let Some(id) = p.release.borrow_mut().take() {
            id.remove();
        }
        let (_, state, _) = p.playbin.state(gst::ClockTime::ZERO);
        if state >= gst::State::Paused {
            let _ = p.playbin.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::ClockTime::ZERO,
            );
        }
        let _ = p.playbin.set_state(gst::State::Playing);
        // Give the audio device back if the sound is not reused soon.
        let weak = Rc::downgrade(&p);
        *p.release.borrow_mut() = Some(glib::timeout_add_local_once(
            Duration::from_secs(30),
            move || {
                if let Some(p) = weak.upgrade() {
                    p.release.borrow_mut().take();
                    let _ = p.playbin.set_state(gst::State::Ready);
                }
            },
        ));
    }

    pub fn shutdown(&self) {
        for p in self.players.borrow().values() {
            if let Some(id) = p.release.borrow_mut().take() {
                id.remove();
            }
            let _ = p.playbin.set_state(gst::State::Null);
        }
        self.players.borrow_mut().clear();
    }
}
