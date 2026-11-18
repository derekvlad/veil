//! The workshop window: browse a JSON catalogue and install wallpapers.
//!
//! Everything network-related happens on worker threads; the UI only listens on
//! a channel. The catalogue format lives in `veil::catalog`, so the CLI and the
//! window install files through exactly the same checked code path.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use veil::catalog::{self, Catalog, Entry};

use crate::library;
use crate::{t, AppState};

enum Msg {
    Catalog(Result<Catalog, String>),
    Preview(String, PathBuf),
    Installed {
        title: String,
        path: PathBuf,
        error: Option<String>,
    },
}

struct Ui {
    stack: gtk::Stack,
    flow: gtk::FlowBox,
    title: gtk::Label,
    url_entry: gtk::Entry,
    error: gtk::Label,
    state: Rc<AppState>,
    sender: Sender<Msg>,
    receiver: RefCell<Receiver<Msg>>,
    cards: RefCell<Vec<(String, gtk::FlowBoxChild)>>,
}

impl Ui {
    fn load(self: &Rc<Self>) {
        let url = self.state.catalog_url();
        if url.is_empty() {
            self.show_setup();
            return;
        }
        self.error.set_text("");
        self.stack.set_visible_child_name("loading");

        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result = catalog::fetch_text(&url).and_then(|text| Catalog::parse(&text));
            let _ = sender.send(Msg::Catalog(result));
        });
    }

    fn show_setup(self: &Rc<Self>) {
        self.url_entry.set_text(&self.state.catalog_url());
        self.error.set_text("");
        self.stack.set_visible_child_name("setup");
        self.url_entry.grab_focus();
    }

    fn save_url(self: &Rc<Self>) {
        let url = self.url_entry.text().trim().to_string();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            self.error.set_text(t(
                "The link must start with https://",
                "Ссылка должна начинаться с https://",
            ));
            return;
        }
        self.state.save_catalog_url(&url);
        self.load();
    }

    fn rebuild(self: &Rc<Self>, catalog: &Catalog) {
        while let Some(child) = self.flow.first_child() {
            self.flow.remove(&child);
        }
        self.cards.borrow_mut().clear();

        let library = self.state.library_dir();
        let cache = self.state.preview_cache_dir();
        let previews: Vec<(String, String)> = catalog
            .entries
            .iter()
            .filter(|entry| !entry.preview.is_empty())
            .map(|entry| (entry.id.clone(), entry.preview.clone()))
            .collect();

        for entry in &catalog.entries {
            let card = self.card(entry, &library, &cache);
            self.flow.append(&card);
            self.cards.borrow_mut().push((entry.id.clone(), card));
        }
        self.title.set_text(&format!(
            "{} · {}",
            t("Workshop", "Мастерская"),
            catalog.entries.len()
        ));
        self.stack.set_visible_child_name("list");

        if !previews.is_empty() {
            let sender = self.sender.clone();
            let cache = cache.clone();
            std::thread::spawn(move || {
                for (id, url) in previews {
                    if let Ok(path) = catalog::cached_preview(&url, &cache, &id) {
                        if sender.send(Msg::Preview(id, path)).is_err() {
                            break;
                        }
                    }
                }
            });
        }
    }

    fn card(self: &Rc<Self>, entry: &Entry, library: &Path, cache: &Path) -> gtk::FlowBoxChild {
        let id = entry.id.clone();
        let boxed = gtk::Box::new(gtk::Orientation::Vertical, 6);
        boxed.add_css_class("workshop-card");
        boxed.set_size_request(236, 262);

        // Previews go through the same 240x135 poster cache as the library, so
        // a 4K PNG never dictates the card size. `FlowBox` lays cards out by
        // their natural width, which is exactly the poster size.
        let preview = gtk::Stack::new();
        preview.set_size_request(236, 133);
        preview.add_css_class("card-preview");
        let icon = gtk::Image::from_icon_name("video-x-generic-symbolic");
        icon.set_pixel_size(48);
        icon.add_css_class("dim-label");
        preview.add_named(&icon, Some("icon"));
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Cover);
        preview.add_named(&picture, Some("picture"));
        preview.set_visible_child_name("icon");

        let cached = cache.join(format!("catalog-{id}.img"));
        if cached.is_file() {
            show_preview(&preview, &picture, &cached);
        }

        let title = gtk::Label::new(Some(&entry.title));
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.set_lines(2);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.add_css_class("heading");

        let mut meta = Vec::new();
        if !entry.author.is_empty() {
            meta.push(entry.author.clone());
        }
        if !entry.kind.is_empty() {
            meta.push(entry.kind.clone());
        }
        if entry.size > 0 {
            meta.push(size_label(entry.size));
        }
        let meta_label = gtk::Label::new(Some(&meta.join(" · ")));
        meta_label.set_xalign(0.0);
        meta_label.add_css_class("caption");
        meta_label.add_css_class("dim-label");

        let tags = entry.tags.join(" · ");
        let tags_label = gtk::Label::new(Some(&tags));
        tags_label.set_xalign(0.0);
        tags_label.add_css_class("caption");
        tags_label.add_css_class("dim-label");
        tags_label.set_visible(!tags.is_empty());

        let installed = catalog::file_name_from_url(&entry.url)
            .map(|name| library.join(name).exists())
            .unwrap_or(false);

        let action = gtk::Button::with_label(if installed {
            t("Installed", "Установлено")
        } else {
            t("Install", "Установить")
        });
        if installed {
            action.add_css_class("flat");
        } else {
            action.add_css_class("suggested-action");
        }
        action.set_sensitive(!installed);
        action.set_tooltip_text(Some(&entry.url));

        action.connect_clicked({
            let weak = Rc::downgrade(self);
            let entry = entry.clone();
            move |button| {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                button.set_sensitive(false);
                button.set_label(t("Downloading…", "Скачиваю…"));
                let sender = ui.sender.clone();
                let library = ui.state.library_dir();
                let url = entry.url.clone();
                let sha256 = entry.sha256.clone();
                let title = entry.title.clone();
                std::thread::spawn(move || {
                    let result = catalog::install(&url, Some(&sha256), &library);
                    let message = match result {
                        Ok(path) => Msg::Installed {
                            title,
                            path,
                            error: None,
                        },
                        Err(error) => Msg::Installed {
                            title,
                            path: PathBuf::new(),
                            error: Some(error),
                        },
                    };
                    let _ = sender.send(message);
                });
            }
        });

        boxed.append(&preview);
        boxed.append(&title);
        boxed.append(&meta_label);
        boxed.append(&tags_label);
        boxed.append(&action);

        let child = gtk::FlowBoxChild::new();
        child.set_child(Some(&boxed));
        if !entry.description.is_empty() {
            child.set_tooltip_text(Some(&entry.description));
        }
        child
    }

    fn pump(self: &Rc<Self>) {
        loop {
            let message = match self.receiver.borrow_mut().try_recv() {
                Ok(message) => message,
                Err(_) => break,
            };
            self.handle(message);
        }
    }

    fn handle(self: &Rc<Self>, message: Msg) {
        match message {
            Msg::Catalog(Ok(catalog)) => self.rebuild(&catalog),
            Msg::Catalog(Err(error)) => {
                self.error.set_text(&error);
                self.stack.set_visible_child_name("setup");
            }
            Msg::Preview(id, path) => {
                for (card_id, child) in self.cards.borrow().iter() {
                    if card_id != &id {
                        continue;
                    }
                    let Some(boxed) = child.child().and_downcast::<gtk::Box>() else {
                        break;
                    };
                    let Some(preview) = boxed.first_child().and_downcast::<gtk::Stack>() else {
                        break;
                    };
                    let Some(picture) = preview
                        .child_by_name("picture")
                        .and_downcast::<gtk::Picture>()
                    else {
                        break;
                    };
                    show_preview(&preview, &picture, &path);
                    break;
                }
            }
            Msg::Installed { title, path, error } => {
                match error {
                    None => {
                        self.state
                            .notify(&format!("{}: {}", t("Installed", "Установлено"), title));
                        self.state.refresh_library();
                        log_line(&format!("workshop: {}", path.display()));
                    }
                    Some(error) => {
                        self.state.notify(&error);
                        log_line(&format!("workshop: {error}"));
                    }
                }
                self.load();
            }
        }
    }
}

/// Show a cached preview, re-using the library's 240x135 poster cache.
fn show_preview(stack: &gtk::Stack, picture: &gtk::Picture, source: &Path) {
    let Some(poster) = library::poster(source) else {
        log_line(&format!("{}: no preview", source.display()));
        return;
    };
    picture.set_filename(Some(&poster));
    stack.set_visible_child_name("picture");
}

fn log_line(message: &str) {
    eprintln!("veil-gui: {message}");
}

fn size_label(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.1} GiB", mb / 1024.0)
    } else if mb >= 1.0 {
        format!("{mb:.1} MiB")
    } else {
        format!("{:.0} KiB", bytes as f64 / 1024.0)
    }
}

fn boxed_text(child: &gtk::FlowBoxChild) -> Option<String> {
    let boxed = child.child().and_downcast::<gtk::Box>()?;
    let mut labels = Vec::new();
    let mut current = boxed.first_child();
    while let Some(widget) = current {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            labels.push(label.text().to_string());
        }
        current = widget.next_sibling();
    }
    Some(labels.join(" ").to_lowercase())
}

pub fn open(state: &Rc<AppState>) {
    let window = adw::Window::builder()
        .title(t("Workshop", "Мастерская"))
        .transient_for(&state.window)
        .default_width(900)
        .default_height(680)
        .build();

    let header = adw::HeaderBar::new();
    let title = gtk::Label::new(Some(t("Workshop", "Мастерская")));
    title.add_css_class("title");
    header.set_title_widget(Some(&title));

    let url_button = gtk::Button::from_icon_name("link-symbolic");
    url_button.add_css_class("flat");
    url_button.set_tooltip_text(Some(t(
        "Change the catalogue link",
        "Изменить ссылку на каталог",
    )));
    header.pack_end(&url_button);

    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.add_css_class("flat");
    refresh.set_tooltip_text(Some(t("Reload the catalogue", "Обновить каталог")));
    header.pack_end(&refresh);

    let stack = gtk::Stack::new();
    stack.set_vexpand(true);

    // ---------------------------------------------------------------- loading
    let loading = gtk::Box::new(gtk::Orientation::Vertical, 12);
    loading.set_valign(gtk::Align::Center);
    let spinner = gtk::Spinner::new();
    spinner.set_size_request(32, 32);
    spinner.start();
    let loading_label = gtk::Label::new(Some(t("Loading the catalogue…", "Загружаю каталог…")));
    loading.append(&spinner);
    loading.append(&loading_label);
    stack.add_named(&loading, Some("loading"));

    // ------------------------------------------------------------------ setup
    let setup = gtk::Box::new(gtk::Orientation::Vertical, 12);
    setup.set_valign(gtk::Align::Center);
    setup.set_halign(gtk::Align::Center);
    setup.set_margin_start(32);
    setup.set_margin_end(32);
    let setup_title = gtk::Label::new(Some(t("Where is the catalogue?", "Где находится каталог?")));
    setup_title.add_css_class("title-2");
    let setup_hint = gtk::Label::new(Some(t(
        "Paste a link to catalog.json. It is a plain JSON file, so any static host will do.",
        "Вставьте ссылку на catalog.json. Это обычный JSON-файл, подойдёт любой статический хостинг.",
    )));
    setup_hint.set_wrap(true);
    setup_hint.set_justify(gtk::Justification::Center);
    setup_hint.add_css_class("dim-label");
    let url_entry = gtk::Entry::new();
    url_entry.set_placeholder_text(Some("https://…/catalog.json"));
    url_entry.set_width_chars(52);
    let save = gtk::Button::with_label(t("Load the catalogue", "Загрузить каталог"));
    save.add_css_class("suggested-action");
    save.add_css_class("pill");
    let error = gtk::Label::new(None);
    error.set_wrap(true);
    error.add_css_class("error");
    setup.append(&setup_title);
    setup.append(&setup_hint);
    setup.append(&url_entry);
    setup.append(&save);
    setup.append(&error);
    stack.add_named(&setup, Some("setup"));

    // ------------------------------------------------------------------- list
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(t("Search the workshop", "Поиск по мастерской")));
    search.set_margin_top(12);
    search.set_margin_bottom(8);
    search.set_margin_start(16);
    search.set_margin_end(16);
    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_homogeneous(false);
    flow.set_column_spacing(16);
    flow.set_row_spacing(16);
    flow.set_max_children_per_line(5);
    flow.set_min_children_per_line(1);
    flow.set_margin_start(16);
    flow.set_margin_end(16);
    flow.set_margin_bottom(16);
    flow.set_valign(gtk::Align::Start);
    // `ScrolledWindow` hands its child the whole viewport height and `FlowBox`
    // then spreads the extra space between rows. The holder pins the grid to
    // the top so rows stay as tight as `row_spacing` asks for.
    let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    holder.set_valign(gtk::Align::Start);
    holder.append(&flow);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&holder)
        .build();
    list.append(&search);
    list.append(&scroller);
    stack.add_named(&list, Some("list"));

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&stack));
    window.set_content(Some(&toolbar_view));

    let (sender, receiver) = mpsc::channel();
    let ui = Rc::new(Ui {
        stack,
        flow,
        title,
        url_entry,
        error,
        state: state.clone(),
        sender,
        receiver: RefCell::new(receiver),
        cards: RefCell::new(Vec::new()),
    });

    // The window owns the state: without this the `Rc<Ui>` would be dropped as
    // soon as `open()` returns and every signal handler would silently stop.
    unsafe { window.set_data("veil-workshop-ui", ui.clone()) };

    save.connect_clicked({
        let weak = Rc::downgrade(&ui);
        move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.save_url();
            }
        }
    });
    ui.url_entry.connect_activate({
        let weak = Rc::downgrade(&ui);
        move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.save_url();
            }
        }
    });
    refresh.connect_clicked({
        let weak = Rc::downgrade(&ui);
        move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.load();
            }
        }
    });
    url_button.connect_clicked({
        let weak = Rc::downgrade(&ui);
        move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.show_setup();
            }
        }
    });
    search.connect_search_changed({
        let weak = Rc::downgrade(&ui);
        move |entry| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let query = entry.text().trim().to_lowercase();
            for (_, child) in ui.cards.borrow().iter() {
                let text = boxed_text(child).unwrap_or_default();
                child.set_visible(query.is_empty() || text.contains(&query));
            }
        }
    });

    let weak = Rc::downgrade(&ui);
    glib::timeout_add_local(Duration::from_millis(120), move || {
        let Some(ui) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        ui.pump();
        glib::ControlFlow::Continue
    });

    ui.load();
    window.present();
}
