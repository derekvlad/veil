//! `veil-widgets` — draws the `[[widgets]]` of the veil config over the wallpaper.
//!
//! One transparent layer-shell window per monitor, on the `bottom` layer: above
//! the wallpaper, below normal windows. Every window has an empty input region,
//! so clicks go to the wallpaper and nothing steals focus.
//!
//! Widgets are plain text: a clock, CPU/RAM, the battery, the current track and
//! free-form text. One timer refreshes whatever is dynamic.

mod clock;
mod system;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer as ShellLayer, LayerShell};

use veil::config::{config_path, Config};
use veil::widgets::{self, Kind, Widget};

/// Refresh cadences, in seconds: stats and the track do not need a clock tick.
const STATS_EVERY: u32 = 2;
const BATTERY_EVERY: u32 = 30;
const MPRIS_EVERY: u32 = 3;

/// One drawn widget and the label that shows it.
struct Item {
    widget: Widget,
    label: gtk::Label,
}

/// One layer-shell window and the widgets that live on it.
struct Screen {
    size: (i32, i32),
    window: gtk::Window,
    fixed: gtk::Fixed,
    items: Vec<Item>,
}

/// Everything that survives between ticks.
#[derive(Default)]
struct Runtime {
    screens: Vec<Screen>,
    cpu: RefCell<system::Cpu>,
    battery: Option<system::Battery>,
    russian: bool,
    tick: u32,
    /// Set once the first window is up; later activations are just reloads.
    started: bool,
    /// The single style sheet for every label; rebuilt with the windows.
    provider: Option<gtk::CssProvider>,
}

fn main() -> glib::ExitCode {
    let mut check = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--check" => check = true,
            "-h" | "--help" => {
                println!("veil-widgets — draw the [[widgets]] of the veil config");
                println!("  veil-widgets           run, one window per monitor");
                println!("  veil-widgets --check   validate the config and exit");
                return glib::ExitCode::SUCCESS;
            }
            other => {
                eprintln!("veil-widgets: unknown option `{other}`");
                return glib::ExitCode::FAILURE;
            }
        }
    }

    let path = config_path();
    let cfg = match load(&path) {
        Ok(cfg) => cfg,
        Err(error) => {
            eprintln!("veil-widgets: {error}");
            return glib::ExitCode::FAILURE;
        }
    };

    if check {
        if cfg.widgets.is_empty() {
            println!("no widgets configured");
        }
        for widget in &cfg.widgets {
            let state = if widget.enabled { "" } else { " (off)" };
            println!("{}{state}", widget.summary());
        }
        return glib::ExitCode::SUCCESS;
    }

    // Nothing to draw: do not keep a process alive for an empty config.
    if !cfg.widgets.iter().any(|widget| widget.enabled) {
        eprintln!(
            "veil-widgets: no widgets in {}, nothing to do",
            path.display()
        );
        return glib::ExitCode::SUCCESS;
    }

    // A unique application: a second launch (from the GUI, say) becomes one
    // more `activate` on the running process instead of a duplicate window.
    let app = gtk::Application::builder()
        .application_id("org.veil.Widgets")
        .build();

    let config = Rc::new(RefCell::new(cfg));
    let runtime = Rc::new(RefCell::new(Runtime {
        russian: russian(),
        battery: system::Battery::discover(),
        ..Runtime::default()
    }));

    app.connect_activate({
        let config = config.clone();
        let runtime = runtime.clone();
        let path = path.clone();
        move |app| {
            // Already running: someone asked us to pick up a new config.
            if runtime.borrow().started {
                match load(&path) {
                    Ok(fresh) => *config.borrow_mut() = fresh,
                    Err(error) => eprintln!("veil-widgets: {error}"),
                }
                build(app, &config.borrow(), &mut runtime.borrow_mut());
                update(&runtime);
                return;
            }

            runtime.borrow_mut().started = true;
            build(app, &config.borrow(), &mut runtime.borrow_mut());
            update(&runtime);

            glib::timeout_add_seconds_local(1, {
                let runtime = runtime.clone();
                move || {
                    {
                        let mut state = runtime.borrow_mut();
                        state.tick = state.tick.wrapping_add(1);
                    }
                    update(&runtime);
                    glib::ControlFlow::Continue
                }
            });

            watch(app, &path, config.clone(), runtime.clone());
        }
    });

    app.run()
}

fn load(path: &Path) -> Result<Config, String> {
    let (mut cfg, _) = Config::load_from(path)?;
    cfg.widgets.iter_mut().for_each(Widget::sanitize);
    Ok(cfg)
}

/// Replace every window with ones built from the current config.
fn build(app: &gtk::Application, cfg: &Config, runtime: &mut Runtime) {
    for screen in runtime.screens.drain(..) {
        screen.window.destroy();
    }

    let Some(display) = gtk::gdk::Display::default() else {
        eprintln!("veil-widgets: no display");
        return;
    };
    // One style sheet for every label of this build; the old one would linger.
    if let Some(provider) = runtime.provider.take() {
        gtk::style_context_remove_provider_for_display(&display, &provider);
    }
    // Layer surfaces have no decorations to paint, but GTK still fills a window
    // with the theme colour unless it is told not to.
    let mut css = String::from("window.veil-widgets { background: none; box-shadow: none; }\n");
    let monitors = display.monitors();
    for index in 0..monitors.n_items() {
        let Some(monitor) = monitors
            .item(index)
            .and_then(|item| item.downcast::<gtk::gdk::Monitor>().ok())
        else {
            continue;
        };
        let name = monitor
            .connector()
            .map(|name| name.to_string())
            .unwrap_or_else(|| format!("monitor-{index}"));
        let widgets: Vec<Widget> = cfg
            .widgets
            .iter()
            .filter(|widget| widget.shows_on(&name))
            .cloned()
            .collect();
        if widgets.is_empty() {
            continue;
        }

        let window = gtk::Window::builder()
            .application(app)
            .decorated(false)
            .resizable(false)
            .build();
        window.init_layer_shell();
        window.add_css_class("veil-widgets");
        window.set_monitor(Some(&monitor));
        window.set_layer(ShellLayer::Bottom);
        window.set_namespace(Some("veil-widgets"));
        window.set_keyboard_mode(KeyboardMode::None);
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        // Cover the monitor, but never reserve space in the layout.
        window.set_exclusive_zone(-1);
        let geometry = monitor.geometry();
        // Without a size the window shrinks to its content and clips widgets.
        window.set_default_size(geometry.width(), geometry.height());

        let fixed = gtk::Fixed::new();
        window.set_child(Some(&fixed));

        let mut items = Vec::new();
        for (number, widget) in widgets.into_iter().enumerate() {
            let class = format!("veil-w{index}-{number}");
            // Multi-line content (a clock over a date) is centred line by line.
            let label = gtk::Label::new(None);
            label.set_justify(gtk::Justification::Center);
            label.add_css_class("veil-widget");
            label.add_css_class(&class);
            css.push_str(&css_for(&class, &widget));
            fixed.put(&label, 0.0, 0.0);
            items.push(Item { widget, label });
        }

        // Clicks belong to the wallpaper and to the windows above it.
        window.connect_realize(|window| {
            if let Some(surface) = window.surface() {
                surface.set_input_region(Some(&gtk::cairo::Region::create()));
            }
        });

        window.present();
        runtime.screens.push(Screen {
            size: (geometry.width(), geometry.height()),
            window,
            fixed,
            items,
        });
    }

    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    runtime.provider = Some(provider);

    if runtime.screens.is_empty() {
        eprintln!("veil-widgets: widgets are configured, but no monitor matched them");
    }
}

/// CSS for one widget: font, size, colour, readability shadow and plate.
fn css_for(class: &str, widget: &Widget) -> String {
    let family = if widget.font.trim().is_empty() {
        String::new()
    } else {
        format!("font-family: \"{}\";", widget.font.replace('"', ""))
    };
    let plate = if widget.shade {
        "background-color: rgba(0,0,0,0.35); border-radius: 12px; padding: 6px 12px;"
    } else {
        ""
    };
    format!(
        ".{class} {{ {family} font-size: {}pt; color: {}; opacity: {}; \
         text-shadow: 0 1px 3px rgba(0,0,0,0.9); {plate} }}\n",
        widget.size, widget.color, widget.opacity
    )
}

/// Refresh text on every label, then put the labels in place.
fn update(runtime: &Rc<RefCell<Runtime>>) {
    let runtime = runtime.borrow();
    let parts = clock::now();
    let stats = if runtime.tick % STATS_EVERY == 0 {
        let (used, total) = system::memory();
        Some((runtime.cpu.borrow_mut().percent(), used, total))
    } else {
        None
    };
    let battery = if runtime.tick % BATTERY_EVERY == 0 {
        runtime.battery.as_ref().and_then(|battery| battery.read())
    } else {
        None
    };
    let track = if runtime.tick % MPRIS_EVERY == 0 {
        Some(system::now_playing().unwrap_or_default())
    } else {
        None
    };

    for screen in &runtime.screens {
        for item in &screen.items {
            let text = match item.widget.kind {
                Kind::Clock => widgets::render_time(&item.widget.format, &parts, runtime.russian),
                Kind::Stats => match stats {
                    Some((cpu, used, total)) => {
                        widgets::render_stats(&item.widget.format, cpu, used, total)
                    }
                    None => {
                        place(screen, item);
                        continue;
                    }
                },
                Kind::Battery => match battery {
                    Some((capacity, charging, full)) => widgets::render_battery(
                        &item.widget.format,
                        capacity,
                        charging,
                        full,
                        runtime.russian,
                    ),
                    None => {
                        place(screen, item);
                        continue;
                    }
                },
                Kind::Mpris => match &track {
                    Some(track) => track.clone(),
                    None => {
                        place(screen, item);
                        continue;
                    }
                },
                Kind::Text => item.widget.format.clone(),
            };
            item.label.set_text(&text);
            place(screen, item);
        }
    }
}

/// Put a label where its percentages say, once its size is known.
fn place(screen: &Screen, item: &Item) {
    // The allocation is the truth after the style sheet has been applied; the
    // earlier `measure` may describe the default font.
    let (width, height) = (item.label.width(), item.label.height());
    let (width, height) = if width > 0 && height > 0 {
        (width, height)
    } else {
        let width = item.label.measure(gtk::Orientation::Horizontal, -1).0;
        let height = item.label.measure(gtk::Orientation::Vertical, width).0;
        (width, height)
    };
    let (x, y) = item.widget.position(screen.size, (width, height));
    screen.fixed.move_(&item.label, f64::from(x), f64::from(y));
}

/// Rebuild when the config file changes; editors write it in several steps.
fn watch(
    app: &gtk::Application,
    path: &Path,
    config: Rc<RefCell<Config>>,
    runtime: Rc<RefCell<Runtime>>,
) {
    let file = gtk::gio::File::for_path(path);
    let Ok(monitor) = file.monitor_file(
        gtk::gio::FileMonitorFlags::NONE,
        gtk::gio::Cancellable::NONE,
    ) else {
        eprintln!("veil-widgets: cannot watch {}", path.display());
        return;
    };
    monitor.connect_changed({
        let app = app.clone();
        let path: PathBuf = path.to_path_buf();
        let config = config.clone();
        let runtime = runtime.clone();
        move |_, _, _, _| {
            let app = app.clone();
            let path = path.clone();
            let config = config.clone();
            let runtime = runtime.clone();
            glib::timeout_add_seconds_local_once(1, move || match load(&path) {
                Ok(fresh) => {
                    *config.borrow_mut() = fresh;
                    build(&app, &config.borrow(), &mut runtime.borrow_mut());
                    update(&runtime);
                }
                Err(error) => eprintln!("veil-widgets: {error}"),
            });
        }
    });
    // The monitor has to outlive this function.
    std::mem::forget(monitor);
}

fn russian() -> bool {
    for key in ["LC_ALL", "LC_TIME", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                return value.to_lowercase().starts_with("ru");
            }
        }
    }
    false
}
