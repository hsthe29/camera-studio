//! Widget drawing a video paintable: fit/fill, mirror, rule-of-thirds grid, blur.

use std::cell::{Cell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Viewfinder {
        pub paintable: RefCell<Option<gdk::Paintable>>,
        pub handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub mirror: Cell<bool>,
        pub fill: Cell<bool>,
        pub grid: Cell<bool>,
        pub qr_guide: Cell<bool>,
        pub blur: Cell<f32>,
        pub background: Cell<bool>,
        pub natural: Cell<(i32, i32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Viewfinder {
        const NAME: &'static str = "CsViewfinder";
        type Type = super::Viewfinder;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Viewfinder {
        fn constructed(&self) {
            self.parent_constructed();
            self.background.set(true);
            self.natural.set((640, 360));
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }

        fn dispose(&self) {
            self.obj().set_paintable(None);
        }
    }

    impl WidgetImpl for Viewfinder {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (w, h) = self.natural.get();
            let natural = if orientation == gtk::Orientation::Horizontal {
                w
            } else {
                h
            };
            (natural.min(64), natural, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (w, h) = (widget.width() as f32, widget.height() as f32);
            if self.background.get() {
                snapshot.append_color(&gdk::RGBA::BLACK, &graphene::Rect::new(0.0, 0.0, w, h));
            }
            let Some(paintable) = self.paintable.borrow().clone() else {
                return;
            };
            let Some((x, y, dw, dh)) = widget.video_rect() else {
                return;
            };
            let blur = self.blur.get();
            if blur > 0.0 {
                snapshot.push_blur(blur as f64);
            }
            snapshot.save();
            if self.mirror.get() {
                snapshot.translate(&graphene::Point::new(w, 0.0));
                snapshot.scale(-1.0, 1.0);
            }
            snapshot.translate(&graphene::Point::new(x, y));
            paintable.snapshot(snapshot, dw as f64, dh as f64);
            snapshot.restore();
            if blur > 0.0 {
                snapshot.pop();
            }
            if self.qr_guide.get() {
                draw_qr_guide(snapshot, (x.max(0.0), y.max(0.0), dw.min(w), dh.min(h)));
            }
            if self.grid.get() {
                let (gx, gy, gw, gh) = (x.max(0.0), y.max(0.0), dw.min(w), dh.min(h));
                let line = gdk::RGBA::new(1.0, 1.0, 1.0, 0.45);
                let shadow = gdk::RGBA::new(0.0, 0.0, 0.0, 0.25);
                for i in 1..3 {
                    let lx = (gx + gw * i as f32 / 3.0).round();
                    let ly = (gy + gh * i as f32 / 3.0).round();
                    snapshot.append_color(&shadow, &graphene::Rect::new(lx + 1.0, gy, 1.0, gh));
                    snapshot.append_color(&shadow, &graphene::Rect::new(gx, ly + 1.0, gw, 1.0));
                    snapshot.append_color(&line, &graphene::Rect::new(lx, gy, 1.0, gh));
                    snapshot.append_color(&line, &graphene::Rect::new(gx, ly, gw, 1.0));
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct Viewfinder(ObjectSubclass<imp::Viewfinder>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Viewfinder {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Viewfinder {
    /// A small fixed-size view (e.g. the gallery button) without black background.
    pub fn thumbnail(size: i32) -> Self {
        Self::fixed(size, size)
    }

    /// Fixed natural size whatever the paintable (Gtk.Picture would report the
    /// paintable's own size and make its container grow).
    pub fn fixed(width: i32, height: i32) -> Self {
        let v = Self::default();
        v.imp().natural.set((width, height));
        v.set_size_request(width, height);
        v.imp().background.set(false);
        v.set_fill(true);
        v
    }

    pub fn paintable(&self) -> Option<gdk::Paintable> {
        self.imp().paintable.borrow().clone()
    }

    pub fn set_paintable(&self, paintable: Option<&gdk::Paintable>) {
        let imp = self.imp();
        if let Some(old) = imp.paintable.borrow_mut().take() {
            for h in imp.handlers.borrow_mut().drain(..) {
                old.disconnect(h);
            }
        }
        if let Some(p) = paintable {
            let mut handlers = imp.handlers.borrow_mut();
            handlers.push(p.connect_invalidate_contents(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.queue_draw()
            )));
            handlers.push(p.connect_invalidate_size(glib::clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.queue_draw()
            )));
            *imp.paintable.borrow_mut() = Some(p.clone());
        }
        self.queue_draw();
    }

    pub fn video_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let paintable = self.imp().paintable.borrow().clone()?;
        let (w, h) = (self.width() as f32, self.height() as f32);
        let (pw, ph) = (
            paintable.intrinsic_width() as f32,
            paintable.intrinsic_height() as f32,
        );
        if w <= 0.0 || h <= 0.0 || pw <= 0.0 || ph <= 0.0 {
            return None;
        }
        let scale = if self.imp().fill.get() {
            (w / pw).max(h / ph)
        } else {
            (w / pw).min(h / ph)
        };
        let (dw, dh) = (pw * scale, ph * scale);
        // Snap to whole device pixels: drawing at a half-pixel offset makes the
        // GPU interpolate between pixels and blurs the whole picture.
        let ds = self
            .native()
            .and_then(|n| n.surface())
            .map(|s| s.scale() as f32)
            .unwrap_or(1.0)
            .max(1.0);
        let snap = |v: f32| (v * ds).round() / ds;
        let (dw, dh) = (snap(dw), snap(dh));
        Some((snap((w - dw) / 2.0), snap((h - dh) / 2.0), dw, dh))
    }

    fn set_flag(&self, cell: &Cell<bool>, value: bool) {
        if cell.get() != value {
            cell.set(value);
            self.queue_draw();
        }
    }

    pub fn set_mirror(&self, v: bool) {
        self.set_flag(&self.imp().mirror, v);
    }
    pub fn set_fill(&self, v: bool) {
        self.set_flag(&self.imp().fill, v);
    }
    pub fn set_grid(&self, v: bool) {
        self.set_flag(&self.imp().grid, v);
    }
    pub fn set_qr_guide(&self, v: bool) {
        self.set_flag(&self.imp().qr_guide, v);
    }
    pub fn set_blur(&self, radius: f32) {
        if self.imp().blur.get() != radius {
            self.imp().blur.set(radius);
            self.queue_draw();
        }
    }
}

/// Darken everything but a centred square and mark its corners (QR mode).
fn draw_qr_guide(snapshot: &gtk::Snapshot, (x, y, w, h): (f32, f32, f32, f32)) {
    // Whole pixels everywhere: fractional edges leave visible seams between
    // the darkened rectangles.
    let (x, y, w, h) = (x.round(), y.round(), w.round(), h.round());
    let side = (w.min(h) * 0.62).min(440.0).round();
    let (gx, gy) = (
        (x + (w - side) / 2.0).round(),
        (y + (h - side) / 2.0).round(),
    );
    let dim = gdk::RGBA::new(0.0, 0.0, 0.0, 0.45);
    for r in [
        graphene::Rect::new(x, y, w, gy - y),
        graphene::Rect::new(x, gy + side, w, y + h - gy - side),
        graphene::Rect::new(x, gy, gx - x, side),
        graphene::Rect::new(gx + side, gy, x + w - gx - side, side),
    ] {
        snapshot.append_color(&dim, &r);
    }
    let white = gdk::RGBA::WHITE;
    let (len, t) = ((side * 0.14).round(), 4.0);
    for (cx, cy, sx, sy) in [
        (gx, gy, 1.0, 1.0),
        (gx + side, gy, -1.0, 1.0),
        (gx, gy + side, 1.0, -1.0),
        (gx + side, gy + side, -1.0, -1.0),
    ] {
        let hx = if sx > 0.0 { cx } else { cx - len };
        let vy = if sy > 0.0 { cy } else { cy - len };
        let hy = if sy > 0.0 { cy } else { cy - t };
        let vx = if sx > 0.0 { cx } else { cx - t };
        snapshot.append_color(&white, &graphene::Rect::new(hx, hy, len, t));
        snapshot.append_color(&white, &graphene::Rect::new(vx, vy, t, len));
    }
}
