//! Settings of a Wallpaper Engine scene.
//!
//! The engine exposes the knobs an author declared in `project.json` and reads
//! its overrides from `--set-property key=value`. This window asks the engine
//! for the list, draws a fitting control for every entry, and writes the answers
//! into `[engine.properties]` of the config; the daemon restarts the engine and
//! the scene picks them up.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use veil::engine::{self, Property, PropertyKind};
use veil::ipc::{self, Request};

use crate::{t, AppState};

/// A slider drag fires many changes; the engine is restarted once, after this.
const APPLY_DELAY: Duration = Duration::from_millis(400);

enum Msg {
    Properties(Result<Vec<Property>, String>),
}

struct Ui {
    state: Rc<AppState>,
    rows: gtk::Box,
    status: gtk::Label,
    properties: RefCell<Vec<Property>>,
    /// Set while the window puts values into its controls.
    quiet: Cell<bool>,
    /// A reload is already waiting for the debounce to run out.
    pending: Cell<bool>,
}

impl Ui {
    /// Write one value into the config, or drop the key when it is the default.
    fn store(self: &Rc<Self>, key: &str, default: &str, value: String) {
        if self.quiet.get() {
            return;
        }
        {
            let mut cfg = self.state.cfg.borrow_mut();
            if value == default {
                cfg.engine.properties.remove(key);
            } else {
                cfg.engine.properties.insert(key.to_string(), value);
            }
        }
        self.state.save_config();
        self.schedule();
    }

    /// Ask the daemon to re-read the config, but not on every slider pixel.
    fn schedule(self: &Rc<Self>) {
        if self.pending.replace(true) {
            return;
        }
        glib::timeout_add_local_once(APPLY_DELAY, {
            let weak = Rc::downgrade(self);
            move || {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                ui.pending.set(false);
                if let Err(error) = ipc::call(&Request::Reload) {
                    ui.state.toast(&error);
                }
            }
        });
    }

    /// Put every setting back to what the scene itself declares.
    fn reset_all(self: &Rc<Self>) {
        {
            let mut cfg = self.state.cfg.borrow_mut();
            for property in self.properties.borrow().iter() {
                cfg.engine.properties.remove(&property.key);
            }
        }
        self.state.save_config();
        self.schedule();
        let properties = self.properties.borrow().clone();
        self.fill(properties);
        self.state
            .toast(t("Scene settings reset", "Настройки сцены сброшены"));
    }

    fn fill(self: &Rc<Self>, properties: Vec<Property>) {
        while let Some(child) = self.rows.first_child() {
            self.rows.remove(&child);
        }
        if properties.is_empty() {
            self.status.set_text(t(
                "This scene has no settings of its own.",
                "У этой сцены нет собственных настроек.",
            ));
            *self.properties.borrow_mut() = properties;
            return;
        }
        self.status.set_text(&format!(
            "{}: {}",
            t("Settings in this scene", "Настроек в этой сцене"),
            properties.len()
        ));
        // The scene reports its own defaults; what a user already changed lives
        // in the config and must win, or the window would show stale values.
        let overrides = self.state.cfg.borrow().engine.properties.clone();
        self.quiet.set(true);
        for property in &properties {
            let mut shown = property.clone();
            if let Some(value) = overrides.get(&shown.key) {
                shown.value = value.clone();
            }
            self.rows.append(&self.row(&shown));
        }
        self.quiet.set(false);
        *self.properties.borrow_mut() = properties;
    }

    fn row(self: &Rc<Self>, property: &Property) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
        row.add_css_class("scene-row");
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        row.set_margin_start(14);
        row.set_margin_end(14);

        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let title = gtk::Label::new(Some(&property.title()));
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_wrap(true);
        title.set_tooltip_text(Some(&property.key));
        line.append(&title);
        row.append(&line);

        let key = property.key.clone();
        let default = property.default_value();

        match property.kind {
            PropertyKind::Boolean => {
                let switch = gtk::Switch::new();
                switch.set_active(property.value != "0");
                switch.set_valign(gtk::Align::Center);
                line.append(&switch);
                switch.connect_active_notify({
                    let weak = Rc::downgrade(self);
                    let key = key.clone();
                    let default = default.clone();
                    move |switch| {
                        let Some(ui) = weak.upgrade() else {
                            return;
                        };
                        ui.store(&key, &default, engine::bool_value(switch.is_active()));
                    }
                });
            }
            PropertyKind::Combo if !property.options.is_empty() => {
                let labels: Vec<&str> = property
                    .options
                    .iter()
                    .map(|(_, label)| label.as_str())
                    .collect();
                let dropdown = gtk::DropDown::from_strings(&labels);
                let selected = property
                    .options
                    .iter()
                    .position(|(option, _)| option == &property.value)
                    .unwrap_or(0);
                dropdown.set_selected(selected as u32);
                dropdown.set_hexpand(true);
                row.append(&dropdown);
                dropdown.connect_selected_notify({
                    let weak = Rc::downgrade(self);
                    let key = key.clone();
                    let default = default.clone();
                    let options = property.options.clone();
                    move |dropdown| {
                        let Some(ui) = weak.upgrade() else {
                            return;
                        };
                        let Some((value, _)) = options.get(dropdown.selected() as usize) else {
                            return;
                        };
                        ui.store(&key, &default, value.clone());
                    }
                });
            }
            PropertyKind::Slider => {
                let value_label = gtk::Label::new(Some(&property.value));
                value_label.add_css_class("caption");
                value_label.add_css_class("dim-label");
                line.append(&value_label);

                let value: f64 = property.value.parse().unwrap_or(property.min);
                let step = if property.step > 0.0 {
                    property.step
                } else {
                    (property.max - property.min) / 100.0
                };
                let scale = gtk::Scale::with_range(
                    gtk::Orientation::Horizontal,
                    property.min,
                    property.max,
                    step,
                );
                scale.set_value(value.clamp(property.min, property.max));
                scale.set_draw_value(false);
                scale.set_hexpand(true);
                row.append(&scale);
                scale.connect_value_changed({
                    let weak = Rc::downgrade(self);
                    let key = key.clone();
                    let label = value_label.clone();
                    let step = property.step;
                    let default = property.default_value();
                    move |scale| {
                        let Some(ui) = weak.upgrade() else {
                            return;
                        };
                        let value = engine::number_value(scale.value(), step);
                        label.set_text(&value);
                        ui.store(&key, &default, value);
                    }
                });
            }
            PropertyKind::Color => {
                let components = engine::color_components(&property.value);
                let button = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
                button.set_rgba(&gtk::gdk::RGBA::new(
                    f32::from(components[0]) / 255.0,
                    f32::from(components[1]) / 255.0,
                    f32::from(components[2]) / 255.0,
                    f32::from(components[3]) / 255.0,
                ));
                button.set_halign(gtk::Align::Start);
                row.append(&button);
                button.connect_rgba_notify({
                    let weak = Rc::downgrade(self);
                    let key = key.clone();
                    let default = default.clone();
                    move |button| {
                        let Some(ui) = weak.upgrade() else {
                            return;
                        };
                        let rgba = button.rgba();
                        let value = engine::color_value([
                            (rgba.red() * 255.0).round() as u8,
                            (rgba.green() * 255.0).round() as u8,
                            (rgba.blue() * 255.0).round() as u8,
                            (rgba.alpha() * 255.0).round() as u8,
                        ]);
                        ui.store(&key, &default, value);
                    }
                });
            }
            kind if kind.is_textual() => {
                let entry = gtk::Entry::new();
                entry.set_text(&property.value);
                entry.set_hexpand(true);
                row.append(&entry);
                entry.connect_changed({
                    let weak = Rc::downgrade(self);
                    let key = key.clone();
                    let default = default.clone();
                    move |entry| {
                        let Some(ui) = weak.upgrade() else {
                            return;
                        };
                        ui.store(&key, &default, entry.text().to_string());
                    }
                });
            }
            _ => {
                let value = gtk::Label::new(Some(&property.value));
                value.set_xalign(0.0);
                value.add_css_class("dim-label");
                value.set_selectable(true);
                row.append(&value);
            }
        }
        row
    }
}

/// Ask the engine for the settings of one scene.
fn list_properties(
    item: &engine::Item,
    binary: &str,
    assets: &str,
) -> Result<Vec<Property>, String> {
    let Some(binary) = engine::binary(binary) else {
        return Err(t(
            "The engine is not installed: yay -S linux-wallpaperengine-git",
            "Движок не установлен: yay -S linux-wallpaperengine-git",
        )
        .to_string());
    };
    let assets = if assets.trim().is_empty() {
        engine::assets_dir()
    } else {
        Some(PathBuf::from(veil::config::expand(assets)))
    };

    let mut command = Command::new(binary);
    if let Some(assets) = assets {
        command.arg("--assets-dir").arg(assets);
    }
    // The scene folder is unambiguous even when the id would collide.
    command.arg("--list-properties").arg(&item.dir);
    let output = command
        .output()
        .map_err(|error| format!("{}: {error}", item.dir.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr.lines().last().unwrap_or_default();
        return Err(format!("{}: {last}", item.dir.display()));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(engine::parse_properties(&text))
}

/// Open the settings window for one scene.
pub fn open(state: &Rc<AppState>, item: &engine::Item) {
    // One window at a time: it belongs to one scene.
    if let Some(app) = state.window.application() {
        for window in app.windows() {
            let open = unsafe { window.data::<Rc<Ui>>("veil-scene-settings").is_some() };
            if open {
                window.close();
            }
        }
    }

    let window = gtk::Window::builder()
        .application(&state.window.application().expect("window has an app"))
        .transient_for(&state.window)
        .title(item.title.clone())
        .default_width(480)
        .default_height(620)
        .build();
    window.add_css_class("scene-settings");

    let header = gtk::HeaderBar::new();
    let title = adw::WindowTitle::new(
        &item.title,
        &format!("{} · {}", t("Scene settings", "Настройки сцены"), item.id),
    );
    header.set_title_widget(Some(&title));
    window.set_titlebar(Some(&header));

    let status = gtk::Label::new(Some(t("Asking the engine…", "Спрашиваю движок…")));
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.add_css_class("caption");
    status.add_css_class("dim-label");
    status.set_margin_top(10);
    status.set_margin_bottom(6);
    status.set_margin_start(14);
    status.set_margin_end(14);

    let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&rows)
        .build();

    let hint = gtk::Label::new(Some(t(
        "Changes are applied at once: the engine restarts the scene.",
        "Изменения применяются сразу: движок перезапускает сцену.",
    )));
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    hint.add_css_class("caption");
    hint.add_css_class("dim-label");

    let reset = gtk::Button::with_label(t("Reset to scene defaults", "Сбросить к значениям сцены"));
    reset.add_css_class("flat");
    let close = gtk::Button::with_label(t("Close", "Закрыть"));
    close.add_css_class("suggested-action");
    close.connect_clicked({
        let window = window.clone();
        move |_| window.close()
    });

    // Hint on its own line: with two long Russian buttons it would be squeezed.
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.append(&reset);
    reset.set_hexpand(true);
    reset.set_halign(gtk::Align::Start);
    buttons.append(&close);
    let footer = gtk::Box::new(gtk::Orientation::Vertical, 6);
    footer.set_margin_top(8);
    footer.set_margin_bottom(10);
    footer.set_margin_start(14);
    footer.set_margin_end(14);
    footer.append(&hint);
    footer.append(&buttons);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&status);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    content.append(&scroller);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    content.append(&footer);
    window.set_child(Some(&content));

    let (binary, assets) = {
        let cfg = state.cfg.borrow();
        (cfg.engine.binary.clone(), cfg.engine.assets.clone())
    };

    let ui = Rc::new(Ui {
        state: state.clone(),
        rows: rows.clone(),
        status: status.clone(),
        properties: RefCell::new(Vec::new()),
        quiet: Cell::new(false),
        pending: Cell::new(false),
    });

    reset.connect_clicked({
        let weak = Rc::downgrade(&ui);
        move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.reset_all();
            }
        }
    });

    let (sender, receiver) = mpsc::channel::<Msg>();
    {
        let item = item.clone();
        std::thread::spawn(move || {
            let result = list_properties(&item, &binary, &assets);
            let _ = sender.send(Msg::Properties(result));
        });
    }
    glib::timeout_add_local(Duration::from_millis(120), {
        let weak = Rc::downgrade(&ui);
        move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            match receiver.try_recv() {
                Ok(Msg::Properties(Ok(properties))) => {
                    ui.fill(properties);
                    glib::ControlFlow::Break
                }
                Ok(Msg::Properties(Err(error))) => {
                    ui.status.set_text(&error);
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
            }
        }
    });

    unsafe {
        window.set_data("veil-scene-settings", ui);
    }
    window.present();
}
