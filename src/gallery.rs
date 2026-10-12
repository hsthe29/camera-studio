//! Gallery grid and media viewer pages.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::compat::ToggleGroup;
use crate::player::VideoPlayer;
use crate::storage::{self, MediaFile};
use crate::thumbnails::ThumbnailCache;

type DirsFn = Box<dyn Fn() -> Vec<PathBuf>>;
type OpenHandler = Box<dyn Fn(Vec<MediaFile>, usize)>;
type ViewerAction = Box<dyn Fn(&Rc<ViewerPage>)>;

pub struct GalleryPage {
    pub page: adw::NavigationPage,
    store: gio::ListStore,
    filter: gtk::CustomFilter,
    filtered: gtk::FilterListModel,
    stack: gtk::Stack,
    count: gtk::Label,
    pub kind: ToggleGroup,
    kind_value: Rc<RefCell<String>>,
    thumbs: Rc<ThumbnailCache>,
    dirs: DirsFn,
    monitors: RefCell<Vec<gio::FileMonitor>>,
    watched: RefCell<Vec<PathBuf>>,
    refresh_source: RefCell<Option<glib::SourceId>>,
    on_open: RefCell<Option<OpenHandler>>,
}

fn media_of(obj: &glib::Object) -> MediaFile {
    obj.downcast_ref::<glib::BoxedAnyObject>()
        .unwrap()
        .borrow::<MediaFile>()
        .clone()
}

impl GalleryPage {
    pub fn new(thumbs: &Rc<ThumbnailCache>, dirs: impl Fn() -> Vec<PathBuf> + 'static) -> Rc<Self> {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let kind_value = Rc::new(RefCell::new("all".to_string()));
        let kv = kind_value.clone();
        let filter = gtk::CustomFilter::new(move |obj| {
            let m = media_of(obj);
            match kv.borrow().as_str() {
                "photo" => !m.is_video,
                "video" => m.is_video,
                _ => true,
            }
        });
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));

        let factory = gtk::SignalListItemFactory::new();
        let grid = gtk::GridView::builder()
            .model(&gtk::NoSelection::new(Some(filtered.clone())))
            .factory(&factory)
            .single_click_activate(true)
            .max_columns(10)
            .min_columns(2)
            .build();
        grid.add_css_class("gallery-grid");
        let scrolled = gtk::ScrolledWindow::builder()
            .child(&grid)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("image-x-generic-symbolic")
            .title("No Photos or Videos")
            .description("Photos and videos you take will appear here.")
            .build();
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(&scrolled, Some("grid"));
        stack.add_named(&empty, Some("empty"));

        let header = adw::HeaderBar::new();
        let kind = ToggleGroup::new(&[("all", "All"), ("photo", "Photos"), ("video", "Videos")]);
        kind.set_active_name(Some("all"));
        header.set_title_widget(Some(kind.widget()));
        let folder = gtk::Button::builder()
            .icon_name("folder-open-symbolic")
            .tooltip_text("Open Folder")
            .build();
        header.pack_end(&folder);
        let count = gtk::Label::new(None);
        count.add_css_class("dim-label");
        count.add_css_class("caption");
        header.pack_start(&count);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        let page = adw::NavigationPage::builder()
            .title("Gallery")
            .tag("gallery")
            .child(&toolbar)
            .build();

        let this = Rc::new(Self {
            page,
            store,
            filter,
            filtered,
            stack,
            count,
            kind,
            kind_value,
            thumbs: thumbs.clone(),
            dirs: Box::new(dirs),
            monitors: RefCell::new(Vec::new()),
            watched: RefCell::new(Vec::new()),
            refresh_source: RefCell::new(None),
            on_open: RefCell::new(None),
        });

        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let tile = gtk::Overlay::new();
            tile.add_css_class("gallery-tile");
            tile.set_overflow(gtk::Overflow::Hidden);
            let picture = gtk::Picture::builder()
                .content_fit(gtk::ContentFit::Cover)
                .can_shrink(true)
                .build();
            picture.set_size_request(168, 126);
            tile.set_child(Some(&picture));
            let badge = gtk::Box::builder()
                .spacing(4)
                .halign(gtk::Align::Start)
                .valign(gtk::Align::End)
                .build();
            badge.add_css_class("video-badge");
            badge.append(
                &gtk::Image::builder()
                    .icon_name("media-playback-start-symbolic")
                    .pixel_size(12)
                    .build(),
            );
            let duration = gtk::Label::new(None);
            duration.add_css_class("caption");
            duration.add_css_class("numeric");
            badge.append(&duration);
            tile.add_overlay(&badge);
            item.set_child(Some(&tile));
        });
        factory.connect_bind(glib::clone!(
            #[weak]
            this,
            move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                let tile = item.child().and_downcast::<gtk::Overlay>().unwrap();
                let picture = tile.child().and_downcast::<gtk::Picture>().unwrap();
                let badge = tile.last_child().and_downcast::<gtk::Box>().unwrap();
                let duration = badge.last_child().and_downcast::<gtk::Label>().unwrap();
                let media = media_of(&item.item().unwrap());
                let key = media.path.to_string_lossy().into_owned();
                tile.set_widget_name(&key);
                tile.set_tooltip_text(Some(&media.name()));
                badge.set_visible(media.is_video);
                duration.set_label("");
                picture.set_paintable(None::<&gdk::Paintable>);
                let is_video = media.is_video;
                this.thumbs.request(&media.path, move |t| {
                    if tile.widget_name() != key {
                        return; // recycled for another item
                    }
                    picture.set_paintable(t.texture.as_ref());
                    if is_video && t.duration > 0.0 {
                        duration.set_label(&storage::format_duration(t.duration));
                    }
                });
            }
        ));
        factory.connect_unbind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            if let Some(tile) = item.child().and_downcast::<gtk::Overlay>() {
                tile.set_widget_name("");
            }
        });
        grid.connect_activate(glib::clone!(
            #[weak]
            this,
            move |_, pos| {
                let items = this.items();
                if let Some(cb) = this.on_open.borrow().as_ref() {
                    cb(items, pos as usize);
                }
            }
        ));
        this.kind.connect_active_name_notify(glib::clone!(
            #[weak]
            this,
            move |g| {
                *this.kind_value.borrow_mut() =
                    g.active_name().unwrap_or_else(|| "all".into());
                this.filter.changed(gtk::FilterChange::Different);
                this.update_state();
            }
        ));
        folder.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |b| {
                let dirs = (this.dirs)();
                let target = if this.kind_value.borrow().as_str() == "video" {
                    dirs[1].clone()
                } else {
                    dirs[0].clone()
                };
                let _ = std::fs::create_dir_all(&target);
                let root = b.root().and_downcast::<gtk::Window>();
                gtk::FileLauncher::new(Some(&gio::File::for_path(target))).launch(
                    root.as_ref(),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
        ));
        this
    }

    pub fn connect_open(&self, f: impl Fn(Vec<MediaFile>, usize) + 'static) {
        *self.on_open.borrow_mut() = Some(Box::new(f));
    }

    pub fn items(&self) -> Vec<MediaFile> {
        (0..self.filtered.n_items())
            .filter_map(|i| self.filtered.item(i))
            .map(|o| media_of(&o))
            .collect()
    }

    pub fn refresh(self: &Rc<Self>) {
        let dirs = (self.dirs)();
        self.watch(&dirs);
        let media = storage::list_media(&dirs);
        let old: Vec<PathBuf> = (0..self.store.n_items())
            .map(|i| media_of(&self.store.item(i).unwrap()).path)
            .collect();
        if old != media.iter().map(|m| m.path.clone()).collect::<Vec<_>>() {
            let objs: Vec<glib::BoxedAnyObject> =
                media.into_iter().map(glib::BoxedAnyObject::new).collect();
            self.store.splice(0, self.store.n_items(), &objs);
        }
        self.update_state();
    }

    fn update_state(&self) {
        let n = self.filtered.n_items();
        self.stack
            .set_visible_child_name(if n > 0 { "grid" } else { "empty" });
        let total = self.store.n_items();
        let videos = (0..total)
            .filter(|&i| media_of(&self.store.item(i).unwrap()).is_video)
            .count() as u32;
        self.count
            .set_label(&format!("{} photos · {} videos", total - videos, videos));
    }

    fn watch(self: &Rc<Self>, dirs: &[PathBuf]) {
        if *self.watched.borrow() == dirs {
            return;
        }
        for m in self.monitors.borrow_mut().drain(..) {
            m.cancel();
        }
        *self.watched.borrow_mut() = dirs.to_vec();
        for d in dirs {
            let _ = std::fs::create_dir_all(d);
            if let Ok(mon) = gio::File::for_path(d)
                .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            {
                mon.connect_changed(glib::clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_, _, _, _| {
                        if this.refresh_source.borrow().is_none() {
                            let weak = Rc::downgrade(&this);
                            *this.refresh_source.borrow_mut() = Some(glib::timeout_add_local_once(
                                Duration::from_millis(500),
                                move || {
                                    if let Some(this) = weak.upgrade() {
                                        this.refresh_source.borrow_mut().take();
                                        this.refresh();
                                    }
                                },
                            ));
                        }
                    }
                ));
                self.monitors.borrow_mut().push(mon);
            }
        }
    }
}

pub struct ViewerPage {
    pub page: adw::NavigationPage,
    items: RefCell<Vec<MediaFile>>,
    index: Cell<usize>,
    token: Cell<u64>,
    title: adw::WindowTitle,
    picture: gtk::Picture,
    pub player: Rc<VideoPlayer>,
    stack: gtk::Stack,
    prev: gtk::Button,
    next: gtk::Button,
    copy_action: gio::SimpleAction,
    thumbs: Rc<ThumbnailCache>,
    toast: Box<dyn Fn(&str)>,
    on_deleted: RefCell<Option<Box<dyn Fn()>>>,
}

impl ViewerPage {
    pub fn new(thumbs: &Rc<ThumbnailCache>, toast: impl Fn(&str) + 'static) -> Rc<Self> {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .build();
        let player = VideoPlayer::new();
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .build();
        stack.add_named(&picture, Some("photo"));
        stack.add_named(&player.widget, Some("video"));
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&stack));
        overlay.add_css_class("viewer");
        let nav = |icon: &str, tip: &str, align: gtk::Align| {
            let b = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .halign(align)
                .valign(gtk::Align::Center)
                .build();
            for c in ["circular", "osd", "nav-button"] {
                b.add_css_class(c);
            }
            overlay.add_overlay(&b);
            b
        };
        let prev = nav("go-previous-symbolic", "Previous (←)", gtk::Align::Start);
        let next = nav("go-next-symbolic", "Next (→)", gtk::Align::End);

        let header = adw::HeaderBar::new();
        let title = adw::WindowTitle::new("", "");
        header.set_title_widget(Some(&title));
        let delete = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Move to Trash (Delete)")
            .build();
        header.pack_end(&delete);
        let menu = gio::Menu::new();
        menu.append(Some("Open With Another App"), Some("viewer.open"));
        menu.append(Some("Show in Folder"), Some("viewer.folder"));
        menu.append(Some("Copy Image"), Some("viewer.copy"));
        menu.append(Some("Copy Path"), Some("viewer.copy-path"));
        header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .menu_model(&menu)
                .tooltip_text("More")
                .build(),
        );
        let open = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Open With Another App")
            .build();
        header.pack_end(&open);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&overlay));
        let page = adw::NavigationPage::builder()
            .title("Viewer")
            .tag("viewer")
            .child(&toolbar)
            .build();

        let actions = gio::SimpleActionGroup::new();
        let copy_action = gio::SimpleAction::new("copy", None);
        let this = Rc::new(Self {
            page,
            items: RefCell::new(Vec::new()),
            index: Cell::new(0),
            token: Cell::new(0),
            title,
            picture,
            player,
            stack,
            prev,
            next,
            copy_action: copy_action.clone(),
            thumbs: thumbs.clone(),
            toast: Box::new(toast),
            on_deleted: RefCell::new(None),
        });
        let add = |name: &str, f: ViewerAction| {
            let a = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(&this);
            a.connect_activate(move |_, _| {
                if let Some(t) = weak.upgrade() {
                    f(&t)
                }
            });
            actions.add_action(&a);
        };
        add("open", Box::new(|t| t.open_external()));
        add("folder", Box::new(|t| t.open_folder()));
        add(
            "copy-path",
            Box::new(|t| {
                if let Some(m) = t.current() {
                    t.page.clipboard().set_text(&m.path.to_string_lossy());
                    (t.toast)("Path copied");
                }
            }),
        );
        let weak = Rc::downgrade(&this);
        copy_action.connect_activate(move |_, _| {
            if let Some(t) = weak.upgrade()
                && let Some(tex) = t.picture.paintable().and_downcast::<gdk::Texture>()
            {
                t.page.clipboard().set_texture(&tex);
                (t.toast)("Image copied to clipboard");
            }
        });
        actions.add_action(&copy_action);
        this.page.insert_action_group("viewer", Some(&actions));

        this.prev.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.step(-1)
        ));
        this.next.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.step(1)
        ));
        delete.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.delete_current()
        ));
        open.connect_clicked(glib::clone!(
            #[weak]
            this,
            move |_| this.open_external()
        ));
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak]
            this,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| {
                match key {
                    gdk::Key::Left | gdk::Key::Page_Up => this.step(-1),
                    gdk::Key::Right | gdk::Key::Page_Down => this.step(1),
                    gdk::Key::Delete => this.delete_current(),
                    gdk::Key::space if this.current().is_some_and(|m| m.is_video) => {
                        this.player.toggle()
                    }
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            }
        ));
        this.page.add_controller(keys);
        this.page.connect_hidden(glib::clone!(
            #[weak]
            this,
            move |_| this.player.stop()
        ));
        this
    }

    pub fn connect_deleted(&self, f: impl Fn() + 'static) {
        *self.on_deleted.borrow_mut() = Some(Box::new(f));
    }

    pub fn show_items(self: &Rc<Self>, items: Vec<MediaFile>, index: usize) {
        let n = items.len();
        *self.items.borrow_mut() = items;
        self.index.set(index.min(n.saturating_sub(1)));
        self.load();
    }

    pub fn current(&self) -> Option<MediaFile> {
        self.items.borrow().get(self.index.get()).cloned()
    }

    pub fn step(self: &Rc<Self>, delta: i32) {
        let new = self.index.get() as i64 + delta as i64;
        if new >= 0 && (new as usize) < self.items.borrow().len() {
            self.index.set(new as usize);
            self.load();
        }
    }

    /// Start playing if the current item is a video (used when opening files).
    pub fn play_current(self: &Rc<Self>) {
        if self.current().is_some_and(|m| m.is_video) {
            self.player.play();
        }
    }

    pub fn select_first_video(self: &Rc<Self>) -> bool {
        let pos = self.items.borrow().iter().position(|m| m.is_video);
        if let Some(i) = pos {
            self.index.set(i);
            self.load();
        }
        pos.is_some()
    }

    fn load(self: &Rc<Self>) {
        self.player.stop();
        let n = self.items.borrow().len();
        let i = self.index.get();
        self.prev.set_visible(i > 0);
        self.next.set_visible(i + 1 < n);
        let Some(media) = self.current() else { return };
        self.title.set_title(&media.name());
        self.title.set_subtitle(&format!(
            "{} · {} · {}/{}",
            storage::format_time(media.mtime),
            storage::human_size(media.size),
            i + 1,
            n
        ));
        self.copy_action.set_enabled(!media.is_video);
        self.token.set(self.token.get() + 1);
        let token = self.token.get();
        if media.is_video {
            self.stack.set_visible_child_name("video");
            self.player.load(&media.path);
            return;
        }
        self.stack.set_visible_child_name("photo");
        self.picture.set_paintable(
            self.thumbs
                .peek(&media.path)
                .and_then(|t| t.texture)
                .as_ref(),
        );
        let this = self.clone();
        glib::spawn_future_local(async move {
            let path = media.path.clone();
            let tex = gio::spawn_blocking(move || gdk::Texture::from_filename(&path).ok())
                .await
                .ok()
                .flatten();
            if this.token.get() == token
                && let Some(t) = tex
            {
                this.picture.set_paintable(Some(&t));
            }
        });
    }

    fn open_external(&self) {
        if let Some(m) = self.current() {
            let root = self.page.root().and_downcast::<gtk::Window>();
            gtk::FileLauncher::new(Some(&gio::File::for_path(&m.path))).launch(
                root.as_ref(),
                gio::Cancellable::NONE,
                |_| {},
            );
        }
    }

    fn open_folder(&self) {
        if let Some(m) = self.current() {
            let root = self.page.root().and_downcast::<gtk::Window>();
            gtk::FileLauncher::new(Some(&gio::File::for_path(&m.path))).open_containing_folder(
                root.as_ref(),
                gio::Cancellable::NONE,
                |_| {},
            );
        }
    }

    pub fn delete_current(self: &Rc<Self>) {
        let Some(media) = self.current() else { return };
        let dialog = adw::AlertDialog::new(
            Some("Move to Trash?"),
            Some(&format!("“{}” will be moved to the trash.", media.name())),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("delete", "Move to Trash");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let this = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "delete" {
                return;
            }
            if media.is_video {
                this.player.stop();
            }
            let file = gio::File::for_path(&media.path);
            let this = this.clone();
            let media = media.clone();
            file.trash_async(
                glib::Priority::DEFAULT,
                gio::Cancellable::NONE,
                move |res| {
                    if res.is_err()
                        && let Err(e) =
                            gio::File::for_path(&media.path).delete(gio::Cancellable::NONE)
                    {
                        (this.toast)(&format!("Could not delete: {e}"));
                        return;
                    }
                    this.thumbs.forget(&media.path);
                    this.items.borrow_mut().retain(|m| m.path != media.path);
                    (this.toast)("Moved to trash");
                    if let Some(cb) = this.on_deleted.borrow().as_ref() {
                        cb();
                    }
                    if this.items.borrow().is_empty() {
                        if let Some(nav) = this
                            .page
                            .ancestor(adw::NavigationView::static_type())
                            .and_downcast::<adw::NavigationView>()
                        {
                            nav.pop();
                        }
                        return;
                    }
                    let n = this.items.borrow().len();
                    this.index.set(this.index.get().min(n - 1));
                    this.load();
                },
            );
        });
        dialog.present(Some(&self.page));
    }
}
