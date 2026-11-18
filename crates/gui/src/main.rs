//! `veil-gui` — the wallpaper picker.
//!
//! The window is deliberately plain GTK4 + libadwaita: it must look native on
//! any desktop that can run the daemon, from niri to KDE and GNOME.

mod library;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::{mpsc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use veil::config::{Config, Mode};
use veil::ipc::{self, Request};

const APP_ID: &str = "org.veil.Veil";

const MODES: [Mode; 4] = [Mode::Fill, Mode::Fit, Mode::Stretch, Mode::Center];

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| load_css());
    app.connect_activate(build_window);
    app.run()
}

// ------------------------------------------------------------------- language

fn russian() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var("LANG")
            .or_else(|_| std::env::var("LC_ALL"))
            .or_else(|_| std::env::var("LC_MESSAGES"))
            .map(|value| value.to_ascii_lowercase().starts_with("ru"))
            .unwrap_or(false)
    })
}

/// Pick one of two static strings, Russian when the session is Russian.
fn t(en: &'static str, ru: &'static str) -> &'static str {
    if russian() {
        ru
    } else {
        en
    }
}

fn files_text(count: usize) -> String {
    if russian() {
        let word = match count % 10 {
            1 if count % 100 != 11 => "файл",
            2..=4 if !(12..=14).contains(&(count % 100)) => "файла",
            _ => "файлов",
        };
        format!("{count} {word}")
    } else if count == 1 {
        "1 file".to_string()
    } else {
        format!("{count} files")
    }
}

fn monitors_text(count: usize) -> String {
    if russian() {
        let word = match count % 10 {
            1 if count % 100 != 11 => "экран",
            2..=4 if !(12..=14).contains(&(count % 100)) => "экрана",
            _ => "экранов",
        };
        format!("{count} {word}")
    } else if count == 1 {
        "1 monitor".to_string()
    } else {
        format!("{count} monitors")
    }
}

// ------------------------------------------------------------------------ css

fn load_css() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        r#"
        .wallpaper-card {
            padding: 8px;
            border-radius: 14px;
            transition: background 120ms ease-in-out;
        }
        .wallpaper-card:hover {
            background: alpha(currentColor, 0.07);
        }
        .wallpaper-card:active {
            background: alpha(currentColor, 0.12);
        }
        .thumb {
            border-radius: 10px;
            background: alpha(currentColor, 0.08);
        }
        .wallpaper-name {
            font-weight: 600;
        }
        .badge {
            background: @accent_bg_color;
            color: @accent_fg_color;
            border-radius: 999px;
            padding: 2px 10px;
            font-size: 0.78em;
            font-weight: 600;
        }
        .sidebar {
            background: alpha(currentColor, 0.03);
            border-right: 1px solid alpha(currentColor, 0.09);
        }
        .section-title {
            font-weight: 700;
            font-size: 0.8em;
            letter-spacing: 0.08em;
            text-transform: uppercase;
            opacity: 0.55;
        }
        .library-path {
            font-size: 0.85em;
            opacity: 0.65;
        }
        .empty-icon {
            opacity: 0.5;
        }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

// ---------------------------------------------------------------------- state

struct Card {
    path: String,
    name: String,
    button: gtk::Button,
    picture: gtk::Picture,
    stack: gtk::Stack,
    meta: gtk::Label,
    badge: gtk::Label,
}

struct AppState {
    window: adw::ApplicationWindow,
    toast: adw::ToastOverlay,
    cfg: RefCell<Config>,
    config_path: PathBuf,
    flow: gtk::FlowBox,
    content_stack: gtk::Stack,
    count_label: gtk::Label,
    monitor_dd: gtk::DropDown,
    monitors: RefCell<Vec<String>>,
    mode_dd: gtk::DropDown,
    cards: RefCell<Vec<Card>>,
    status_label: gtk::Label,
    daemon_button: gtk::Button,
    pause_button: gtk::Button,
    search: gtk::SearchEntry,
    library_label: gtk::Label,
    thumb_tx: mpsc::Sender<(String, library::Probe)>,
    ready: Cell<bool>,
}

impl AppState {
    fn toast(&self, message: &str) {
        let toast = adw::Toast::new(message);
        toast.set_timeout(4);
        self.toast.add_toast(toast);
    }

    fn save_config(&self) {
        let cfg = self.cfg.borrow();
        if let Err(error) = cfg.save_to(&self.config_path) {
            drop(cfg);
            self.toast(&error);
        }
    }

    /// Persist settings and let the daemon re-read the file.
    fn push_settings(&self) {
        self.save_config();
        if ipc::call(&Request::Reload).is_err() {
            self.toast(t(
                "Saved. Start the daemon to apply this.",
                "Сохранено. Запустите демон, чтобы применить.",
            ));
        }
    }
}

// --------------------------------------------------------------------- window

fn build_window(app: &adw::Application) {
    let (config, config_path) = match Config::load() {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("veil-gui: {error}");
            (Config::default(), veil::config::config_path())
        }
    };

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(t("veil — wallpapers", "veil — обои"))
        .default_width(1160)
        .default_height(760)
        .build();
    let _ = window.set_icon_name(Some(APP_ID));

    let toast = adw::ToastOverlay::new();

    // ---------------------------------------------------------------- header
    let header = adw::HeaderBar::new();
    let title = gtk::Label::new(Some("veil"));
    title.add_css_class("title");
    header.set_title_widget(Some(&title));

    let pause_button = gtk::Button::from_icon_name("media-playback-pause-symbolic");
    pause_button.add_css_class("flat");
    pause_button.set_tooltip_text(Some(t("Pause / resume", "Пауза / продолжить")));
    header.pack_end(&pause_button);

    let menu = gtk::gio::Menu::new();
    menu.append(
        Some(t("Refresh library", "Обновить библиотеку")),
        Some("win.refresh"),
    );
    menu.append(
        Some(t("Choose library folder…", "Выбрать папку библиотеки…")),
        Some("win.folder"),
    );
    menu.append(
        Some(t("Open configuration", "Открыть конфигурацию")),
        Some("win.config"),
    );
    menu.append(Some(t("About veil", "О программе")), Some("win.about"));
    menu.append(Some(t("Quit", "Выход")), Some("app.quit"));

    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text(t("Menu", "Меню"))
        .build();
    menu_button.add_css_class("flat");
    header.pack_end(&menu_button);

    // ---------------------------------------------------------------- sidebar
    let sidebar_inner = gtk::Box::new(gtk::Orientation::Vertical, 16);
    sidebar_inner.set_margin_top(18);
    sidebar_inner.set_margin_bottom(18);
    sidebar_inner.set_margin_start(18);
    sidebar_inner.set_margin_end(18);

    sidebar_inner.append(&section_title(t("Screen", "Экран")));
    let monitor_dd = gtk::DropDown::from_strings(&[t("All monitors", "Все мониторы")]);
    monitor_dd.set_tooltip_text(Some(t(
        "The next wallpaper goes to this monitor",
        "Следующие обои применятся к этому экрану",
    )));
    sidebar_inner.append(&monitor_dd);

    sidebar_inner.append(&section_title(t("Fill mode", "Заполнение")));
    let mode_labels = [
        t("Fill (crop)", "Заполнить (обрезать)"),
        t("Fit (letterbox)", "Вписать (поля)"),
        t("Stretch", "Растянуть"),
        t("Center", "По центру"),
    ];
    let mode_dd = gtk::DropDown::from_strings(&mode_labels);
    mode_dd.set_selected(mode_index(config.wallpaper.mode));
    sidebar_inner.append(&mode_dd);

    sidebar_inner.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    sidebar_inner.append(&section_title(t("Playback", "Воспроизведение")));

    let (mute_row, mute_switch) = switch_row(
        t("Mute", "Без звука"),
        t(
            "Video wallpapers are always silent by default",
            "Обои по умолчанию без звука",
        ),
        config.playback.mute,
    );
    let (fullscreen_row, fullscreen_switch) = switch_row(
        t("Pause in fullscreen", "Пауза в полноэкранном режиме"),
        t(
            "Save power while a game or video is open",
            "Экономить ресурсы при игре или видео",
        ),
        config.rules.pause_on_fullscreen,
    );
    let (idle_row, idle_switch) = switch_row(
        t("Pause when idle", "Пауза при простое"),
        t(
            "Stop decoding after a while without input",
            "Останавливать декодирование без ввода",
        ),
        config.rules.pause_on_idle,
    );
    let (battery_row, battery_switch) = switch_row(
        t("Pause on battery", "Пауза от батареи"),
        t(
            "Keep the battery for real work",
            "Беречь батарею для работы",
        ),
        config.rules.pause_on_battery,
    );

    sidebar_inner.append(&mute_row);
    sidebar_inner.append(&fullscreen_row);

    let idle_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    idle_box.append(&idle_row);
    let idle_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let idle_caption = gtk::Label::new(Some(t("Timeout, seconds", "Таймаут, секунд")));
    idle_caption.add_css_class("caption");
    idle_caption.add_css_class("dim-label");
    idle_caption.set_hexpand(true);
    idle_caption.set_halign(gtk::Align::Start);
    let idle_spin = gtk::SpinButton::with_range(10.0, 3600.0, 30.0);
    idle_spin.set_value(config.rules.idle_timeout_secs.max(10) as f64);
    idle_spin.set_numeric(true);
    idle_line.append(&idle_caption);
    idle_line.append(&idle_spin);
    idle_box.append(&idle_line);
    sidebar_inner.append(&idle_box);
    sidebar_inner.append(&battery_row);

    sidebar_inner.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    sidebar_inner.append(&section_title(t("Library", "Библиотека")));
    let library_label = gtk::Label::new(Some(&config.library_path().to_string_lossy()));
    library_label.add_css_class("library-path");
    library_label.set_wrap(true);
    library_label.set_xalign(0.0);
    sidebar_inner.append(&library_label);

    let library_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let folder_button = gtk::Button::with_label(t("Change folder…", "Сменить папку…"));
    folder_button.add_css_class("flat");
    library_buttons.append(&folder_button);
    let random_button = gtk::Button::with_label(t("Random", "Случайные"));
    random_button.add_css_class("flat");
    library_buttons.append(&random_button);
    sidebar_inner.append(&library_buttons);

    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    sidebar_inner.append(&spacer);

    let status_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    status_box.add_css_class("sidebar-status");
    let status_label = gtk::Label::new(Some(t("Checking the daemon…", "Проверяю демон…")));
    status_label.set_wrap(true);
    status_label.set_xalign(0.0);
    status_label.add_css_class("caption");
    status_label.add_css_class("dim-label");
    let daemon_button = gtk::Button::with_label(t("Start the daemon", "Запустить демон"));
    daemon_button.add_css_class("suggested-action");
    daemon_button.set_visible(false);
    status_box.append(&status_label);
    status_box.append(&daemon_button);
    sidebar_inner.append(&status_box);

    let sidebar_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&sidebar_inner)
        .build();
    sidebar_scroll.add_css_class("sidebar");
    sidebar_scroll.set_propagate_natural_width(false);
    sidebar_scroll.set_size_request(330, -1);
    sidebar_scroll.set_hexpand(false);

    // ---------------------------------------------------------------- library
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    toolbar.set_margin_top(16);
    toolbar.set_margin_bottom(12);
    toolbar.set_margin_start(20);
    toolbar.set_margin_end(20);

    let heading = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let library_title = gtk::Label::new(Some(t("Wallpapers", "Обои")));
    library_title.add_css_class("title-2");
    library_title.set_xalign(0.0);
    let count_label = gtk::Label::new(None);
    count_label.add_css_class("caption");
    count_label.add_css_class("dim-label");
    count_label.set_xalign(0.0);
    heading.append(&library_title);
    heading.append(&count_label);
    toolbar.append(&heading);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(t("Search", "Поиск")));
    search.set_width_chars(18);
    search.set_hexpand(true);
    search.set_halign(gtk::Align::End);
    search.set_max_width_chars(28);
    toolbar.append(&search);

    let add_button = gtk::Button::builder()
        .label(t("Add", "Добавить"))
        .icon_name("list-add-symbolic")
        .build();
    add_button.add_css_class("suggested-action");
    add_button.set_tooltip_text(Some(t(
        "Copy a video or an image into the library",
        "Скопировать видео или картинку в библиотеку",
    )));
    toolbar.append(&add_button);
    content.append(&toolbar);

    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_homogeneous(false);
    flow.set_column_spacing(10);
    flow.set_row_spacing(10);
    flow.set_max_children_per_line(8);
    flow.set_min_children_per_line(1);
    flow.set_margin_top(4);
    flow.set_margin_bottom(20);
    flow.set_margin_start(16);
    flow.set_margin_end(16);
    flow.set_valign(gtk::Align::Start);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&flow)
        .build();

    let empty_page = adw::StatusPage::builder()
        .icon_name("video-x-generic-symbolic")
        .title(t("No wallpapers yet", "Обоев пока нет"))
        .description(t(
            "Add a video or an image — it will be copied into the library folder.",
            "Добавьте видео или картинку — файл скопируется в папку библиотеки.",
        ))
        .build();
    let empty_add = gtk::Button::with_label(t("Add wallpapers", "Добавить обои"));
    empty_add.add_css_class("suggested-action");
    empty_add.add_css_class("pill");
    empty_add.set_halign(gtk::Align::Center);
    empty_page.set_child(Some(&empty_add));

    let content_stack = gtk::Stack::new();
    content_stack.set_vexpand(true);
    content_stack.add_named(&scroller, Some("library"));
    content_stack.add_named(&empty_page, Some("empty"));
    content_stack.set_visible_child_name("empty");
    content.append(&content_stack);

    let main_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    main_box.append(&sidebar_scroll);
    main_box.append(&content);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&main_box));
    toast.set_child(Some(&toolbar_view));
    window.set_content(Some(&toast));

    // ------------------------------------------------------------ shared state
    let (thumb_tx, thumb_rx) = mpsc::channel();
    let state = Rc::new(AppState {
        window: window.clone(),
        toast,
        cfg: RefCell::new(config),
        config_path,
        flow,
        content_stack,
        count_label,
        monitor_dd,
        monitors: RefCell::new(Vec::new()),
        mode_dd,
        cards: RefCell::new(Vec::new()),
        status_label,
        daemon_button,
        pause_button,
        search,
        library_label,
        thumb_tx,
        ready: Cell::new(false),
    });

    // ---------------------------------------------------------------- actions
    let refresh = gio::SimpleAction::new("refresh", None);
    refresh.connect_activate({
        let state = state.clone();
        move |_, _| rebuild_library(&state)
    });
    window.add_action(&refresh);

    let folder = gio::SimpleAction::new("folder", None);
    folder.connect_activate({
        let state = state.clone();
        move |_, _| choose_folder(&state)
    });
    window.add_action(&folder);

    let config_action = gio::SimpleAction::new("config", None);
    config_action.connect_activate({
        let state = state.clone();
        move |_, _| open_config(&state)
    });
    window.add_action(&config_action);

    let about = gio::SimpleAction::new("about", None);
    about.connect_activate({
        let state = state.clone();
        move |_, _| show_about(&state)
    });
    window.add_action(&about);

    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate({
        let app = app.clone();
        move |_, _| app.quit()
    });
    app.add_action(&quit);

    app.set_accels_for_action("win.refresh", &["F5"]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);

    // --------------------------------------------------------------- behaviour
    add_button.connect_clicked({
        let state = state.clone();
        move |_| add_files(&state)
    });
    empty_add.connect_clicked({
        let state = state.clone();
        move |_| add_files(&state)
    });
    folder_button.connect_clicked({
        let state = state.clone();
        move |_| choose_folder(&state)
    });
    random_button.connect_clicked({
        let state = state.clone();
        move |_| apply_random(&state)
    });
    state.daemon_button.connect_clicked({
        let state = state.clone();
        move |_| match start_daemon() {
            Ok(()) => {
                state.toast(t("Starting the daemon…", "Запускаю демон…"));
                let weak = Rc::downgrade(&state);
                glib::timeout_add_local_once(Duration::from_millis(600), move || {
                    if let Some(state) = weak.upgrade() {
                        refresh_status(&state);
                    }
                });
            }
            Err(error) => state.toast(&error),
        }
    });
    state.pause_button.connect_clicked({
        let state = state.clone();
        move |_| {
            let _ = ipc::call(&Request::Toggle);
            refresh_status(&state);
        }
    });
    state.search.connect_search_changed({
        let state = state.clone();
        move |entry| filter_cards(&state, entry.text().as_str())
    });
    state.mode_dd.connect_selected_notify({
        let state = state.clone();
        move |dropdown| {
            if !state.ready.get() {
                return;
            }
            let mode = mode_at(dropdown.selected());
            state.cfg.borrow_mut().wallpaper.mode = mode;
            state.save_config();
            let _ = ipc::call(&Request::Set {
                path: None,
                mode: Some(mode),
                output: None,
                clear_outputs: false,
            });
        }
    });
    mute_switch.connect_active_notify({
        let state = state.clone();
        move |switch| {
            if !state.ready.get() {
                return;
            }
            state.cfg.borrow_mut().playback.mute = switch.is_active();
            state.push_settings();
        }
    });
    fullscreen_switch.connect_active_notify({
        let state = state.clone();
        move |switch| {
            if !state.ready.get() {
                return;
            }
            state.cfg.borrow_mut().rules.pause_on_fullscreen = switch.is_active();
            state.push_settings();
        }
    });
    idle_switch.connect_active_notify({
        let state = state.clone();
        move |switch| {
            if !state.ready.get() {
                return;
            }
            state.cfg.borrow_mut().rules.pause_on_idle = switch.is_active();
            state.push_settings();
        }
    });
    battery_switch.connect_active_notify({
        let state = state.clone();
        move |switch| {
            if !state.ready.get() {
                return;
            }
            state.cfg.borrow_mut().rules.pause_on_battery = switch.is_active();
            state.push_settings();
        }
    });
    idle_spin.connect_value_changed({
        let state = state.clone();
        move |spin| {
            if !state.ready.get() {
                return;
            }
            state.cfg.borrow_mut().rules.idle_timeout_secs = spin.value() as u64;
            state.push_settings();
        }
    });

    // Thumbnails are produced on worker threads; collect their results here.
    {
        let state = Rc::downgrade(&state);
        glib::timeout_add_local(Duration::from_millis(120), move || {
            let Some(state) = state.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let mut received = 0;
            while let Ok((path, probe)) = thumb_rx.try_recv() {
                received += 1;
                apply_probe(&state, &path, &probe);
            }
            let _ = received;
            glib::ControlFlow::Continue
        });
    }

    // Status polling: cheap, one small JSON line every two seconds.
    {
        let weak = Rc::downgrade(&state);
        glib::timeout_add_local(Duration::from_secs(2), move || {
            let Some(state) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            refresh_status(&state);
            glib::ControlFlow::Continue
        });
    }

    state.ready.set(true);
    rebuild_library(&state);
    refresh_status(&state);
    window.present();
}

// ------------------------------------------------------------------ utilities

fn section_title(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("section-title");
    label.set_xalign(0.0);
    label
}

fn switch_row(title: &str, subtitle: &str, active: bool) -> (gtk::Box, gtk::Switch) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    let title_label = gtk::Label::new(Some(title));
    title_label.set_xalign(0.0);
    title_label.set_halign(gtk::Align::Start);
    let subtitle_label = gtk::Label::new(Some(subtitle));
    subtitle_label.add_css_class("caption");
    subtitle_label.add_css_class("dim-label");
    subtitle_label.set_xalign(0.0);
    subtitle_label.set_halign(gtk::Align::Start);
    subtitle_label.set_wrap(true);
    subtitle_label.set_max_width_chars(38);
    text.append(&title_label);
    text.append(&subtitle_label);
    let switch = gtk::Switch::new();
    switch.set_active(active);
    switch.set_valign(gtk::Align::Center);
    row.append(&text);
    row.append(&switch);
    (row, switch)
}

fn mode_index(mode: Mode) -> u32 {
    MODES.iter().position(|item| *item == mode).unwrap_or(0) as u32
}

fn mode_at(index: u32) -> Mode {
    MODES.get(index as usize).copied().unwrap_or(Mode::Fill)
}

fn selected_mode(state: &AppState) -> Mode {
    mode_at(state.mode_dd.selected())
}

fn selected_monitor(state: &AppState) -> Option<String> {
    let index = state.monitor_dd.selected() as usize;
    if index == 0 {
        return None;
    }
    state.monitors.borrow().get(index - 1).cloned()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

// -------------------------------------------------------------------- library

fn rebuild_library(state: &Rc<AppState>) {
    while let Some(child) = state.flow.first_child() {
        state.flow.remove(&child);
    }
    state.cards.borrow_mut().clear();

    let dir = state.cfg.borrow().library_path();
    if let Err(error) = std::fs::create_dir_all(&dir) {
        state.toast(&format!("{}: {error}", dir.display()));
    }
    state.library_label.set_text(&dir.to_string_lossy());

    let entries = library::scan(&dir);
    for entry in &entries {
        let card = build_card(state, entry);
        state.flow.insert(&card.button, -1);
        state.cards.borrow_mut().push(card);
    }

    if entries.is_empty() {
        state.content_stack.set_visible_child_name("empty");
        state
            .count_label
            .set_text(t("Nothing here yet", "Здесь пока пусто"));
    } else {
        state.content_stack.set_visible_child_name("library");
        state.count_label.set_text(&format!(
            "{} · {}",
            files_text(entries.len()),
            t("click one to apply", "нажмите на обои, чтобы применить")
        ));
    }

    let query = state.search.text().to_string();
    filter_cards(state, &query);

    // Posters and metadata come later, off the main thread.
    for entry in entries {
        let tx = state.thumb_tx.clone();
        let path = entry.path.clone();
        let video = entry.video;
        std::thread::spawn(move || {
            let probe = library::probe(&path, video);
            let _ = tx.send((path.to_string_lossy().into_owned(), probe));
        });
    }
}

fn build_card(state: &Rc<AppState>, entry: &library::Entry) -> Card {
    let picture = gtk::Picture::new();
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Cover);
    picture.add_css_class("thumb");

    let icon = gtk::Image::from_icon_name(if entry.video {
        "video-x-generic-symbolic"
    } else {
        "image-x-generic-symbolic"
    });
    icon.set_pixel_size(44);
    icon.add_css_class("empty-icon");

    let stack = gtk::Stack::new();
    stack.set_size_request(240, 135);
    stack.add_named(&icon, Some("icon"));
    stack.add_named(&picture, Some("thumb"));
    stack.set_visible_child_name("icon");

    let badge = gtk::Label::new(Some(t("On screen", "На экране")));
    badge.add_css_class("badge");
    badge.set_halign(gtk::Align::Start);
    badge.set_valign(gtk::Align::Start);
    badge.set_margin_start(8);
    badge.set_margin_top(8);
    badge.set_visible(false);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&stack));
    overlay.add_overlay(&badge);

    let name_label = gtk::Label::new(Some(&entry.name));
    name_label.add_css_class("wallpaper-name");
    name_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    name_label.set_max_width_chars(26);
    name_label.set_xalign(0.0);
    name_label.set_width_chars(1);

    let meta = gtk::Label::new(Some(if entry.video {
        t("Video", "Видео")
    } else {
        t("Image", "Фото")
    }));
    meta.add_css_class("caption");
    meta.add_css_class("dim-label");
    meta.set_xalign(0.0);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 7);
    body.set_width_request(240);
    body.append(&overlay);
    body.append(&name_label);
    body.append(&meta);

    let button = gtk::Button::builder().child(&body).has_frame(false).build();
    button.add_css_class("wallpaper-card");
    button.set_tooltip_text(Some(t(
        "Click to set as wallpaper · right click to remove",
        "Нажмите, чтобы поставить · правый клик — удалить",
    )));

    let path = entry.path.to_string_lossy().into_owned();
    button.connect_clicked({
        let weak = Rc::downgrade(state);
        let path = path.clone();
        move |_| {
            if let Some(state) = weak.upgrade() {
                apply_wallpaper(&state, Path::new(&path));
            }
        }
    });

    // Right click: a tiny popover with "remove from library".
    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    let remove = gtk::Button::builder()
        .label(t("Remove from library", "Удалить из библиотеки"))
        .icon_name("user-trash-symbolic")
        .has_frame(false)
        .build();
    popover.set_child(Some(&remove));
    popover.set_parent(&button);
    remove.connect_clicked({
        let weak = Rc::downgrade(state);
        let popover = popover.clone();
        let path = path.clone();
        move |_| {
            popover.popdown();
            let Some(state) = weak.upgrade() else {
                return;
            };
            let file = gtk::gio::File::for_path(&path);
            match file.trash(gtk::gio::Cancellable::NONE) {
                Ok(()) => {
                    state.toast(&format!(
                        "{}: {}",
                        t("Removed", "Удалено"),
                        file_name(Path::new(&path))
                    ));
                    rebuild_library(&state);
                }
                Err(error) => state.toast(&format!("{error}")),
            }
        }
    });
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    gesture.connect_pressed({
        let popover = popover.clone();
        move |_, _, x, y| {
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.popup();
        }
    });
    button.add_controller(gesture);

    Card {
        path,
        name: entry.name.clone(),
        button,
        picture,
        stack,
        meta,
        badge,
    }
}

fn apply_probe(state: &AppState, path: &str, probe: &library::Probe) {
    let cards = state.cards.borrow();
    let Some(card) = cards.iter().find(|card| card.path == path) else {
        return;
    };

    if let Some(thumb) = &probe.thumb {
        card.picture.set_filename(Some(thumb));
        card.stack.set_visible_child_name("thumb");
    } else if !probe.video {
        // Poster generation failed; stills can be decoded by GTK directly.
        card.picture.set_filename(Some(path));
        card.stack.set_visible_child_name("thumb");
    }

    let kind = if probe.video {
        t("Video", "Видео")
    } else {
        t("Image", "Фото")
    };
    let size = library::human_size(probe.width, probe.height);
    let duration = format_duration(probe.duration);
    let mut pieces: Vec<String> = vec![kind.to_string()];
    if !size.is_empty() {
        pieces.push(size);
    }
    if probe.video && !duration.is_empty() {
        pieces.push(duration);
    }
    card.meta.set_text(&pieces.join(" · "));
}

fn format_duration(seconds: f64) -> String {
    if seconds <= 0.5 {
        return String::new();
    }
    let total = seconds.round() as u64;
    let (hours, minutes, secs) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

fn filter_cards(state: &AppState, query: &str) {
    let query = query.trim().to_lowercase();
    for card in state.cards.borrow().iter() {
        card.button
            .set_visible(query.is_empty() || card.name.to_lowercase().contains(&query));
    }
}

// ---------------------------------------------------------------------- apply

fn apply_wallpaper(state: &Rc<AppState>, path: &Path) {
    let target = selected_monitor(state);
    let mode = selected_mode(state);
    let path_str = path.to_string_lossy().into_owned();

    {
        let mut cfg = state.cfg.borrow_mut();
        cfg.wallpaper.mode = mode;
        match &target {
            Some(output) => {
                cfg.outputs.insert(output.clone(), path_str.clone());
            }
            None => {
                cfg.wallpaper.path = path_str.clone();
                cfg.outputs.clear();
            }
        }
    }
    state.save_config();

    let request = Request::Set {
        path: Some(path_str.clone()),
        mode: Some(mode),
        output: target.clone(),
        clear_outputs: target.is_none(),
    };
    let name = file_name(path);
    match ipc::call(&request) {
        Ok(_) => {
            let where_to = target.unwrap_or_else(|| t("all monitors", "все экраны").to_string());
            state.toast(&format!("{name} → {where_to}"));
        }
        Err(_) => state.toast(t(
            "Saved to the configuration. Start the daemon to apply.",
            "Сохранено в конфигурации. Запустите демон, чтобы применить.",
        )),
    }
    refresh_status(state);
}

fn apply_random(state: &Rc<AppState>) {
    let entries = library::scan(&state.cfg.borrow().library_path());
    if entries.is_empty() {
        state.toast(t("The library is empty", "Библиотека пуста"));
        return;
    }
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos() as usize)
        .unwrap_or(0);
    let entry = &entries[seed % entries.len()];
    apply_wallpaper(state, &entry.path);
}

// --------------------------------------------------------------------- status

fn refresh_status(state: &Rc<AppState>) {
    match ipc::call(&Request::Status) {
        Ok(value) => {
            state.daemon_button.set_visible(false);
            state.pause_button.set_sensitive(true);

            let paused = value["paused"].as_bool().unwrap_or(false);
            state.pause_button.set_icon_name(if paused {
                "media-playback-start-symbolic"
            } else {
                "media-playback-pause-symbolic"
            });
            state.pause_button.set_tooltip_text(Some(if paused {
                t("Resume the wallpaper", "Продолжить обои")
            } else {
                t("Pause the wallpaper", "Поставить обои на паузу")
            }));

            let mut names = Vec::new();
            let mut active = Vec::new();
            let mut resolution = String::new();
            let mut decoder = String::new();

            if let Some(outputs) = value["outputs"].as_array() {
                for output in outputs {
                    if let Some(name) = output["name"].as_str() {
                        names.push(name.to_string());
                    }
                    if let Some(path) = output["path"].as_str() {
                        if !path.is_empty() {
                            active.push(path.to_string());
                        }
                    }
                    if resolution.is_empty() {
                        let width = output["pixels"]["width"].as_u64().unwrap_or(0);
                        let height = output["pixels"]["height"].as_u64().unwrap_or(0);
                        if width > 0 && height > 0 {
                            resolution = format!("{width}×{height}");
                        }
                    }
                    if decoder.is_empty() {
                        if let Some(current) = output["decoder"].as_str() {
                            if !current.is_empty() && current != "no" {
                                decoder = current.to_ascii_uppercase();
                            }
                        }
                    }
                }
            }

            let mut parts = vec![monitors_text(names.len())];
            if !decoder.is_empty() {
                parts.push(decoder);
            }
            if !resolution.is_empty() {
                parts.push(resolution);
            }
            if paused {
                parts.push(t("paused", "пауза").to_string());
            }
            state.status_label.set_text(&parts.join(" · "));

            sync_monitors(state, &names);
            update_badges(state, &active);
        }
        Err(_) => {
            state
                .status_label
                .set_text(t("Daemon is not running", "Демон не запущен"));
            state.daemon_button.set_visible(true);
            state.pause_button.set_sensitive(false);
            update_badges(state, &[]);
        }
    }
}

fn sync_monitors(state: &AppState, names: &[String]) {
    if state.monitors.borrow().as_slice() == names {
        return;
    }
    let previous = selected_monitor(state);

    let mut labels: Vec<String> = vec![t("All monitors", "Все мониторы").to_string()];
    labels.extend(names.iter().cloned());
    let refs: Vec<&str> = labels.iter().map(|label| label.as_str()).collect();
    state
        .monitor_dd
        .set_model(Some(&gtk::StringList::new(&refs)));

    let selected = previous
        .and_then(|name| names.iter().position(|item| *item == name))
        .map(|index| index as u32 + 1)
        .unwrap_or(0);
    state.monitor_dd.set_selected(selected);
    *state.monitors.borrow_mut() = names.to_vec();
}

fn update_badges(state: &AppState, active: &[String]) {
    for card in state.cards.borrow().iter() {
        card.badge
            .set_visible(active.iter().any(|path| *path == card.path));
    }
}

// --------------------------------------------------------------------- daemon

fn start_daemon() -> Result<(), String> {
    let mut candidates = vec![PathBuf::from("veild")];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".local/bin/veild"));
    }
    candidates.push(PathBuf::from("/usr/bin/veild"));
    candidates.push(PathBuf::from("/usr/local/bin/veild"));

    for candidate in candidates {
        if candidate.is_absolute() && !candidate.is_file() {
            continue;
        }
        match Command::new(&candidate).spawn() {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("{}: {error}", candidate.display())),
        }
    }
    Err(t("veild was not found in PATH", "veild не найден в PATH").to_string())
}

// -------------------------------------------------------------------- dialogs

fn add_files(state: &Rc<AppState>) {
    let dialog = gtk::FileDialog::builder()
        .title(t("Add wallpapers", "Добавить обои"))
        .build();

    let filter = gtk::FileFilter::new();
    filter.set_name(Some(t("Video and images", "Видео и изображения")));
    for pattern in [
        "*.mp4", "*.webm", "*.mkv", "*.mov", "*.m4v", "*.avi", "*.ogv", "*.png", "*.jpg", "*.jpeg",
        "*.webp", "*.avif", "*.gif", "*.bmp",
    ] {
        filter.add_pattern(pattern);
    }
    let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    dialog.set_filters(Some(&filters));

    let weak = Rc::downgrade(state);
    dialog.open_multiple(
        Some(&state.window),
        gtk::gio::Cancellable::NONE,
        move |result| {
            let Ok(files) = result else {
                return;
            };
            let Some(state) = weak.upgrade() else {
                return;
            };
            let library = state.cfg.borrow().library_path();
            let mut added = 0usize;
            let mut errors = Vec::new();
            for index in 0..files.n_items() {
                let Some(file) = files
                    .item(index)
                    .and_then(|item| item.downcast::<gtk::gio::File>().ok())
                else {
                    continue;
                };
                let Some(path) = file.path() else {
                    continue;
                };
                match library::import(&path, &library) {
                    Ok(_) => added += 1,
                    Err(error) => errors.push(error),
                }
            }
            for error in errors.iter().take(3) {
                eprintln!("veil-gui: {error}");
            }
            if added > 0 {
                rebuild_library(&state);
                state.toast(&format!(
                    "{}: {added}",
                    t("Added to the library", "Добавлено в библиотеку")
                ));
            } else if let Some(error) = errors.first() {
                state.toast(error);
            }
        },
    );
}

fn choose_folder(state: &Rc<AppState>) {
    let dialog = gtk::FileDialog::builder()
        .title(t("Wallpaper library", "Библиотека обоев"))
        .build();
    let weak = Rc::downgrade(state);
    dialog.select_folder(
        Some(&state.window),
        gtk::gio::Cancellable::NONE,
        move |result| {
            let Ok(folder) = result else {
                return;
            };
            let Some(state) = weak.upgrade() else {
                return;
            };
            let Some(path) = folder.path() else {
                return;
            };
            state.cfg.borrow_mut().library.path = path.to_string_lossy().into_owned();
            state.save_config();
            rebuild_library(&state);
        },
    );
}

fn open_config(state: &AppState) {
    let path = &state.config_path;
    if !path.exists() {
        state.save_config();
    }
    let uri = format!("file://{}", path.to_string_lossy());
    gtk::UriLauncher::new(&uri).launch(Some(&state.window), gtk::gio::Cancellable::NONE, |_| {});
}

fn show_about(state: &AppState) {
    let about = adw::AboutDialog::builder()
        .application_name("veil")
        .application_icon(APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("veil contributors")
        .comments(t(
            "Hardware-decoded video wallpapers for Wayland: niri, Hyprland, sway and other wlr-layer-shell compositors.",
            "Видеообои с аппаратным декодированием для Wayland: niri, Hyprland, sway и другие композиторы с wlr-layer-shell.",
        ))
        .build();
    about.present(Some(&state.window));
}
