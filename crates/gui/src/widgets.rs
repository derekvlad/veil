//! The widgets window: place applets on a preview of the wallpaper.
//!
//! `veil-widgets` draws them for real; this window is only an editor. Both sides
//! use `veil::widgets::Widget`, so a drag here means the same percentages the
//! renderer reads, and the renderer picks the change up from the config file.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;

use veil::widgets::{Kind, Parts, Widget};

use crate::{library, russian, t, AppState};

/// The preview keeps the shape of a monitor, whatever the window size is.
const PREVIEW_RATIO: f64 = 16.0 / 9.0;

/// One label on the preview canvas.
struct CanvasItem {
    index: usize,
    label: gtk::Label,
}

struct Ui {
    state: Rc<AppState>,
    toast: adw::ToastOverlay,
    list: gtk::ListBox,
    fixed: gtk::Fixed,
    /// One style sheet for every label on the canvas.
    provider: gtk::CssProvider,
    /// Size the canvas had at the last layout, to notice a resize.
    canvas: Cell<(i32, i32)>,
    items: RefCell<Vec<CanvasItem>>,
    widgets: RefCell<Vec<Widget>>,
    selected: Cell<Option<usize>>,
    /// Set while the property rows are filled in, so handlers stay quiet.
    updating: Cell<bool>,
    monitors: Vec<String>,
    kind_label: gtk::Label,
    output: gtk::DropDown,
    x: gtk::SpinButton,
    y: gtk::SpinButton,
    size: gtk::SpinButton,
    color: gtk::ColorDialogButton,
    opacity: gtk::Scale,
    shade: gtk::Switch,
    font: gtk::Entry,
    format: gtk::Entry,
    enabled: gtk::Switch,
    remove: gtk::Button,
}

impl Ui {
    fn selected_widget(&self) -> Option<Widget> {
        self.selected
            .get()
            .and_then(|index| self.widgets.borrow().get(index).cloned())
    }

    fn select(self: &Rc<Self>, index: Option<usize>) {
        self.selected.set(index);
        self.show_props();
        self.paint_selection();
        let row = self
            .selected
            .get()
            .and_then(|index| self.list.row_at_index(index as i32));
        self.list.select_row(row.as_ref());
    }

    fn add(self: &Rc<Self>, kind: Kind) {
        let mut widget = Widget::new(kind);
        widget.sanitize();
        let index = {
            let mut widgets = self.widgets.borrow_mut();
            widgets.push(widget);
            widgets.len() - 1
        };
        self.rebuild();
        self.select(Some(index));
        self.save();
        self.start_renderer();
    }

    fn remove_selected(self: &Rc<Self>) {
        let Some(index) = self.selected.get() else {
            return;
        };
        if index >= self.widgets.borrow().len() {
            return;
        }
        self.widgets.borrow_mut().remove(index);
        self.select(None);
        self.rebuild();
        self.save();
    }

    /// Apply an edit to the selected widget and write the config.
    fn mutate(self: &Rc<Self>, change: impl FnOnce(&mut Widget)) {
        if self.updating.get() {
            return;
        }
        let Some(index) = self.selected.get() else {
            return;
        };
        let edited = {
            let mut widgets = self.widgets.borrow_mut();
            let Some(widget) = widgets.get_mut(index) else {
                return;
            };
            change(widget);
            widget.sanitize();
            widget.clone()
        };
        self.refresh_row(index);
        self.restyle(index, &edited);
        self.save();
    }

    /// Write the config; the running renderer watches the file and reloads.
    fn save(self: &Rc<Self>) {
        {
            let mut cfg = self.state.cfg.borrow_mut();
            cfg.widgets = self.widgets.borrow().clone();
            for widget in cfg.widgets.iter_mut() {
                widget.sanitize();
            }
        }
        self.state.save_config();
    }

    /// Start the renderer if it is not up yet; it watches the config file.
    fn start_renderer(&self) {
        let spawned = std::process::Command::new("veil-widgets")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok();
        if !spawned {
            self.toast.add_toast(adw::Toast::new(t(
                "veil-widgets is not installed; run ./install.sh or cargo install --path crates/widgets",
                "veil-widgets не установлен; запустите ./install.sh",
            )));
        }
    }

    /// Rebuild the list of widgets and the canvas.
    fn rebuild(self: &Rc<Self>) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        let widgets = self.widgets.borrow().clone();
        for (index, widget) in widgets.iter().enumerate() {
            let row = gtk::ListBoxRow::new();
            let body = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            body.set_margin_top(6);
            body.set_margin_bottom(6);
            body.set_margin_start(10);
            body.set_margin_end(10);
            let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
            text.set_hexpand(true);
            let title = gtk::Label::new(Some(widget.kind.label(russian())));
            title.set_xalign(0.0);
            let subtitle = gtk::Label::new(Some(&subtitle_for(widget)));
            subtitle.add_css_class("caption");
            subtitle.add_css_class("dim-label");
            subtitle.set_xalign(0.0);
            text.append(&title);
            text.append(&subtitle);
            body.append(&text);
            let switch = gtk::Switch::new();
            switch.set_active(widget.enabled);
            switch.set_valign(gtk::Align::Center);
            switch.connect_state_set({
                let weak = Rc::downgrade(self);
                move |_, active| {
                    if let Some(ui) = weak.upgrade() {
                        let mut widgets = ui.widgets.borrow_mut();
                        if let Some(widget) = widgets.get_mut(index) {
                            widget.enabled = active;
                        }
                        drop(widgets);
                        ui.refresh_row(index);
                        ui.save();
                    }
                    glib::Propagation::Proceed
                }
            });
            body.append(&switch);
            row.set_child(Some(&body));
            self.list.append(&row);
        }

        self.paint();
    }

    /// Redraw every label on the canvas, dropping the old ones.
    fn paint(self: &Rc<Self>) {
        while let Some(child) = self.fixed.first_child() {
            self.fixed.remove(&child);
        }
        let widgets = self.widgets.borrow().clone();
        let mut items = Vec::new();
        for (index, widget) in widgets.iter().enumerate() {
            // Multi-line content is centred line by line.
            let label = gtk::Label::new(Some(&preview_text(widget)));
            label.set_justify(gtk::Justification::Center);
            label.add_css_class("canvas-widget");
            label.add_css_class(&format!("veil-canvas-{index}"));
            self.fixed.put(&label, 0.0, 0.0);

            let drag = gtk::GestureDrag::new();
            // Percentages where the gesture began; `offset_*` is cumulative.
            let origin: Rc<Cell<Option<(f64, f64)>>> = Rc::new(Cell::new(None));
            drag.connect_drag_begin({
                let weak = Rc::downgrade(self);
                let origin = origin.clone();
                move |_, _, _| {
                    let Some(ui) = weak.upgrade() else {
                        return;
                    };
                    ui.select(Some(index));
                    origin.set(
                        ui.widgets
                            .borrow()
                            .get(index)
                            .map(|widget| (widget.x, widget.y)),
                    );
                }
            });
            drag.connect_drag_update({
                let weak = Rc::downgrade(self);
                let origin = origin.clone();
                move |_, offset_x, offset_y| {
                    let Some(ui) = weak.upgrade() else {
                        return;
                    };
                    let Some((start_x, start_y)) = origin.get() else {
                        return;
                    };
                    let (canvas_w, canvas_h) = (ui.fixed.width(), ui.fixed.height());
                    if canvas_w <= 0 || canvas_h <= 0 {
                        return;
                    }
                    if let Some(slot) = ui.widgets.borrow_mut().get_mut(index) {
                        slot.x = start_x + offset_x / f64::from(canvas_w) * 100.0;
                        slot.y = start_y + offset_y / f64::from(canvas_h) * 100.0;
                        slot.sanitize();
                    }
                    ui.layout();
                }
            });
            drag.connect_drag_end({
                let weak = Rc::downgrade(self);
                move |_, _, _| {
                    if let Some(ui) = weak.upgrade() {
                        ui.refresh_row(index);
                        ui.show_props();
                        ui.save();
                    }
                }
            });
            label.add_controller(drag);

            let click = gtk::GestureClick::new();
            click.connect_released({
                let weak = Rc::downgrade(self);
                move |_, _, _, _| {
                    if let Some(ui) = weak.upgrade() {
                        ui.select(Some(index));
                    }
                }
            });
            label.add_controller(click);

            items.push(CanvasItem { index, label });
        }
        *self.items.borrow_mut() = items;
        self.apply_css();
        self.layout();
    }

    /// Reload the canvas style sheet and the preview text of one widget.
    fn restyle(&self, index: usize, widget: &Widget) {
        for item in self.items.borrow().iter() {
            if item.index == index {
                item.label.set_text(&preview_text(widget));
            }
        }
        self.apply_css();
        self.layout();
    }

    /// One sheet for the whole canvas: CSS classes carry the per-widget values.
    fn apply_css(&self) {
        // Font size scales with the preview, so a 52pt clock looks like one.
        let width = match self.fixed.width() {
            0 => 1920.0,
            width => f64::from(width),
        };
        let scale = width / 1920.0;
        let mut css = String::new();
        for (index, widget) in self.widgets.borrow().iter().enumerate() {
            css.push_str(&canvas_css(&format!("veil-canvas-{index}"), widget, scale));
        }
        self.provider.load_from_string(&css);
    }

    /// Styles and positions follow the canvas size, which only exists after a
    /// layout pass; a short timer notices the first one and every resize.
    fn refresh_canvas(&self) {
        let size = (self.fixed.width(), self.fixed.height());
        if size.0 > 0 && size.1 > 0 && size != self.canvas.get() {
            self.canvas.set(size);
            self.apply_css();
        }
        // The style sheet only reaches the widgets on the next layout cycle, so
        // their size is read again on every tick instead of once.
        self.layout();
    }

    /// Put the labels on their percentages, scaled to the preview size.
    fn layout(&self) {
        let width = self.fixed.width();
        let height = self.fixed.height();
        if width <= 0 || height <= 0 {
            return;
        }
        let widgets = self.widgets.borrow().clone();
        for item in self.items.borrow().iter() {
            let Some(widget) = widgets.get(item.index) else {
                continue;
            };
            // The allocation is what the eye sees; `measure` may still describe
            // the font from before the last style sheet reload.
            let (label_width, label_height) = size_of(item.label.upcast_ref());
            let (x, y) = widget.position((width, height), (label_width, label_height));
            self.fixed.move_(&item.label, f64::from(x), f64::from(y));
        }
    }

    fn refresh_row(&self, index: usize) {
        let Some(row) = self.list.row_at_index(index as i32) else {
            return;
        };
        let Some(widget) = self.widgets.borrow().get(index).cloned() else {
            return;
        };
        let Some(body) = row
            .child()
            .and_then(|child| child.downcast::<gtk::Box>().ok())
        else {
            return;
        };
        let Some(column) = body
            .first_child()
            .and_then(|child| child.downcast::<gtk::Box>().ok())
        else {
            return;
        };
        if let Some(subtitle) = column
            .last_child()
            .and_then(|node| node.downcast::<gtk::Label>().ok())
        {
            subtitle.set_text(&subtitle_for(&widget));
        }
    }

    fn paint_selection(&self) {
        for item in self.items.borrow().iter() {
            if Some(item.index) == self.selected.get() {
                item.label.add_css_class("selected-widget");
            } else {
                item.label.remove_css_class("selected-widget");
            }
        }
    }

    /// Fill the property rows from the selected widget (or disable them).
    fn show_props(&self) {
        self.updating.set(true);
        let widget = self.selected_widget();
        let filled = widget.is_some();
        for row in [
            self.output.upcast_ref::<gtk::Widget>(),
            self.x.upcast_ref(),
            self.y.upcast_ref(),
            self.size.upcast_ref(),
            self.color.upcast_ref(),
            self.opacity.upcast_ref(),
            self.shade.upcast_ref(),
            self.font.upcast_ref(),
            self.format.upcast_ref(),
            self.enabled.upcast_ref(),
        ] {
            row.set_sensitive(filled);
        }
        self.remove.set_sensitive(filled);

        let Some(widget) = widget else {
            self.kind_label
                .set_text(t("Nothing selected", "Ничего не выбрано"));
            self.updating.set(false);
            return;
        };

        self.kind_label.set_text(widget.kind.label(russian()));

        let mut names = vec![t("All monitors", "Все экраны").to_string()];
        names.extend(self.monitors.iter().cloned());
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let model = gtk::StringList::new(&refs);
        self.output.set_model(Some(&model));
        let index = self
            .monitors
            .iter()
            .position(|name| name == &widget.output)
            .map(|index| index as u32 + 1)
            .unwrap_or(0);
        self.output.set_selected(index);

        self.x.set_value(widget.x);
        self.y.set_value(widget.y);
        self.size.set_value(widget.size);
        if let Ok(rgba) = gtk::gdk::RGBA::parse(&widget.color) {
            self.color.set_rgba(&rgba);
        }
        self.opacity.set_value(widget.opacity);
        self.shade.set_active(widget.shade);
        self.font.set_text(&widget.font);
        self.format.set_text(&widget.format);
        self.format.set_placeholder_text(Some(match widget.kind {
            Kind::Clock => "%H:%M  ·  %d %B",
            Kind::Stats => "CPU {cpu}% · RAM {used}/{total} GB",
            Kind::Battery => "{percent}% {state}",
            Kind::Mpris => t(
                "song title comes from the player",
                "название берётся у плеера",
            ),
            Kind::Text => t("any text", "любой текст"),
        }));
        self.enabled.set_active(widget.enabled);
        self.updating.set(false);
    }
}

/// A widget's current allocation, or its natural size before the first pass.
fn size_of(widget: &gtk::Widget) -> (i32, i32) {
    let (width, height) = (widget.width(), widget.height());
    if width > 0 && height > 0 {
        return (width, height);
    }
    let width = widget.measure(gtk::Orientation::Horizontal, -1).0;
    let height = widget.measure(gtk::Orientation::Vertical, width).0;
    (width, height)
}

fn subtitle_for(widget: &Widget) -> String {
    format!(
        "{} · {:.0}% / {:.0}%",
        if widget.output.is_empty() {
            t("all monitors", "все экраны")
        } else {
            widget.output.as_str()
        },
        widget.x,
        widget.y
    )
}

/// What the preview shows for a widget, independent of the real data sources.
fn preview_text(widget: &Widget) -> String {
    match widget.kind {
        Kind::Clock => veil::widgets::render_time(&widget.format, &preview_parts(), russian()),
        Kind::Stats => veil::widgets::render_stats(&widget.format, 12.0, 8.2, 15.5),
        Kind::Battery => veil::widgets::render_battery(&widget.format, 74, true, false, russian()),
        Kind::Mpris => t("Artist — track title", "Исполнитель — название").to_string(),
        Kind::Text => widget.format.clone(),
    }
}

/// A fixed moment, so the clock in the preview does not tick while you edit.
fn preview_parts() -> Parts {
    Parts {
        hour: 21,
        minute: 42,
        second: 0,
        day: 18,
        month: 11,
        year: 2026,
        weekday: 2,
    }
}

/// CSS for a label on the preview canvas, scaled for a ~820 px wide preview.
fn canvas_css(class: &str, widget: &Widget, scale: f64) -> String {
    let family = if widget.font.trim().is_empty() {
        String::new()
    } else {
        format!("font-family: \"{}\";", widget.font.replace('"', ""))
    };
    let plate = if widget.shade {
        "background-color: rgba(0,0,0,0.45); border-radius: 10px; padding: 4px 9px;"
    } else {
        ""
    };
    // 1pt is 4/3 px at 96 dpi, shrunk to the preview's share of the monitor.
    let pixels = (widget.size * 1.3333 * scale).max(5.0);
    format!(
        ".{class} {{ {family} font-size: {pixels:.1}px; color: {}; opacity: {}; \
         text-shadow: 0 1px 3px rgba(0,0,0,0.95); {plate} }}\n",
        widget.color, widget.opacity
    )
}

/// Open the editor, or focus the one that is already open.
pub fn open(state: &Rc<AppState>) {
    // The editor keeps its state on its own window; the same key finds it.
    if let Some(app) = state.window.application() {
        for window in app.windows() {
            let open = unsafe { window.data::<Rc<Ui>>("veil-widgets-editor").is_some() };
            if open {
                window.present();
                return;
            }
        }
    }

    let window = adw::Window::builder()
        .title(t("Widgets", "Виджеты"))
        .transient_for(&state.window)
        .default_width(1220)
        .default_height(720)
        .build();

    let header = adw::HeaderBar::new();
    let title = gtk::Label::new(Some(t("Widgets", "Виджеты")));
    title.add_css_class("title");
    header.set_title_widget(Some(&title));

    let add = gtk::MenuButton::new();
    add.set_icon_name("list-add-symbolic");
    add.add_css_class("flat");
    add.set_tooltip_text(Some(t("Add a widget", "Добавить виджет")));
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
    menu.set_margin_top(6);
    menu.set_margin_bottom(6);
    menu.set_margin_start(6);
    menu.set_margin_end(6);
    for kind in Kind::ALL {
        let button = gtk::Button::with_label(kind.label(russian()));
        button.add_css_class("flat");
        menu.append(&button);
    }
    let popover = gtk::Popover::new();
    popover.set_child(Some(&menu));
    add.set_popover(Some(&popover));
    header.pack_start(&add);

    // ------------------------------------------------------------------- list
    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    list.set_selection_mode(gtk::SelectionMode::Single);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_width(280)
        .child(&list)
        .build();
    scroller.set_size_request(280, -1);

    // ----------------------------------------------------------------- canvas
    let picture = gtk::Picture::new();
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Cover);
    picture.add_css_class("canvas-preview");
    if let Some(poster) = preview_poster(state) {
        // `set_filename` silently ignores files GdkTexture will not take, so
        // the error is worth seeing; wallpaper previews are often GIFs.
        match gtk::gdk::Texture::from_filename(&poster) {
            Ok(texture) => picture.set_paintable(Some(&texture)),
            Err(error) => eprintln!("veil-gui: {}: {error}", poster.display()),
        }
    }

    let fixed = gtk::Fixed::new();
    fixed.set_hexpand(true);
    fixed.set_vexpand(true);
    fixed.set_halign(gtk::Align::Fill);
    fixed.set_valign(gtk::Align::Fill);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&picture));
    overlay.add_overlay(&fixed);
    overlay.set_vexpand(true);

    let aspect = gtk::AspectFrame::new(0.5, 0.5, PREVIEW_RATIO as f32, false);
    aspect.set_child(Some(&overlay));
    aspect.set_vexpand(true);
    aspect.set_margin_top(10);
    aspect.set_margin_bottom(10);
    aspect.set_margin_start(10);
    aspect.set_margin_end(10);

    // ------------------------------------------------------------- properties
    let props = gtk::Box::new(gtk::Orientation::Vertical, 8);
    props.set_margin_top(10);
    props.set_margin_bottom(10);
    props.set_margin_start(12);
    props.set_margin_end(12);

    let kind_label = gtk::Label::new(Some(t("Nothing selected", "Ничего не выбрано")));
    kind_label.set_xalign(0.0);
    kind_label.add_css_class("heading");
    props.append(&kind_label);

    let grid = gtk::Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(10);
    grid.set_hexpand(true);

    let output = gtk::DropDown::from_strings(&[t("All monitors", "Все экраны")]);
    let x = gtk::SpinButton::with_range(0.0, 100.0, 0.5);
    x.set_digits(1);
    let y = gtk::SpinButton::with_range(0.0, 100.0, 0.5);
    y.set_digits(1);
    let size = gtk::SpinButton::with_range(6.0, 400.0, 1.0);
    size.set_value(24.0);
    let color = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    let opacity = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.05, 1.0, 0.05);
    opacity.set_draw_value(false);
    let shade = gtk::Switch::new();
    let font = gtk::Entry::new();
    font.set_placeholder_text(Some(t("Interface font", "Шрифт интерфейса")));
    let format = gtk::Entry::new();
    let enabled = gtk::Switch::new();

    let rows: [(&str, &gtk::Widget); 10] = [
        (t("Monitor", "Монитор"), output.upcast_ref()),
        (t("X, %", "X, %"), x.upcast_ref()),
        (t("Y, %", "Y, %"), y.upcast_ref()),
        (t("Font size, pt", "Кегль, pt"), size.upcast_ref()),
        (t("Colour", "Цвет"), color.upcast_ref()),
        (t("Opacity", "Прозрачность"), opacity.upcast_ref()),
        (t("Plate", "Плашка"), shade.upcast_ref()),
        (t("Font", "Шрифт"), font.upcast_ref()),
        (t("Format", "Формат"), format.upcast_ref()),
        (t("Enabled", "Включён"), enabled.upcast_ref()),
    ];
    for (row, (label, widget)) in rows.into_iter().enumerate() {
        let caption = gtk::Label::new(Some(label));
        caption.set_xalign(0.0);
        caption.add_css_class("caption");
        grid.attach(&caption, 0, row as i32, 1, 1);
        grid.attach(widget, 1, row as i32, 1, 1);
    }
    props.append(&grid);

    let remove = gtk::Button::with_label(t("Remove the widget", "Удалить виджет"));
    remove.add_css_class("destructive-action");
    remove.set_halign(gtk::Align::Start);
    props.append(&remove);

    // The properties need their own column: below the preview there is no room
    // for ten rows in a 680 px tall window.
    let props_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_width(300)
        .vexpand(true)
        .child(&props)
        .build();
    props_scroller.set_size_request(300, -1);

    let hint = gtk::Label::new(Some(t(
        "Drag widgets on the preview; positions are percentages of the monitor.",
        "Перетаскивайте виджеты по превью; позиции хранятся в процентах от монитора.",
    )));
    hint.add_css_class("caption");
    hint.add_css_class("dim-label");
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    hint.set_margin_top(4);
    hint.set_margin_bottom(8);
    hint.set_margin_start(10);
    hint.set_margin_end(10);

    let canvas_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    canvas_column.set_hexpand(true);
    canvas_column.append(&aspect);
    canvas_column.append(&hint);

    let columns = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    columns.append(&scroller);
    columns.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    columns.append(&canvas_column);
    columns.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    columns.append(&props_scroller);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&columns);

    let toast = adw::ToastOverlay::new();
    toast.set_child(Some(&content));

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&toast));
    window.set_content(Some(&toolbar_view));

    let provider = gtk::CssProvider::new();
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let ui = Rc::new(Ui {
        state: state.clone(),
        toast: toast.clone(),
        list: list.clone(),
        fixed: fixed.clone(),
        provider,
        canvas: Cell::new((0, 0)),
        items: RefCell::new(Vec::new()),
        widgets: RefCell::new(state.cfg.borrow().widgets.clone()),
        selected: Cell::new(None),
        updating: Cell::new(false),
        monitors: state.monitors.borrow().clone(),
        kind_label,
        output: output.clone(),
        x: x.clone(),
        y: y.clone(),
        size: size.clone(),
        color: color.clone(),
        opacity: opacity.clone(),
        shade: shade.clone(),
        font: font.clone(),
        format: format.clone(),
        enabled: enabled.clone(),
        remove: remove.clone(),
    });

    // The window keeps the state alive; every handler only holds a weak reference.
    unsafe {
        window.set_data("veil-widgets-editor", ui.clone());
    }

    for (index, button) in menu
        .observe_children()
        .into_iter()
        .filter_map(|child| {
            child
                .ok()
                .and_then(|child| child.downcast::<gtk::Button>().ok())
        })
        .enumerate()
    {
        let weak = Rc::downgrade(&ui);
        let popover = popover.clone();
        let kind = Kind::ALL[index.min(Kind::ALL.len() - 1)];
        button.connect_clicked(move |_| {
            popover.popdown();
            if let Some(ui) = weak.upgrade() {
                ui.add(kind);
            }
        });
    }

    remove.connect_clicked({
        let weak = Rc::downgrade(&ui);
        move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.remove_selected();
            }
        }
    });

    list.connect_row_selected({
        let weak = Rc::downgrade(&ui);
        move |_, row| {
            let (Some(ui), Some(row)) = (weak.upgrade(), row) else {
                return;
            };
            let index = row.index() as usize;
            if ui.selected.get() != Some(index) {
                ui.select(Some(index));
            }
        }
    });

    output.connect_selected_notify({
        let weak = Rc::downgrade(&ui);
        move |dropdown| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let index = dropdown.selected();
            let name = if index == 0 {
                String::new()
            } else {
                ui.monitors
                    .get(index as usize - 1)
                    .cloned()
                    .unwrap_or_default()
            };
            ui.mutate(|widget| widget.output = name);
        }
    });
    x.connect_value_changed({
        let weak = Rc::downgrade(&ui);
        move |button| {
            if let Some(ui) = weak.upgrade() {
                let value = button.value();
                ui.mutate(|widget| widget.x = value);
            }
        }
    });
    y.connect_value_changed({
        let weak = Rc::downgrade(&ui);
        move |button| {
            if let Some(ui) = weak.upgrade() {
                let value = button.value();
                ui.mutate(|widget| widget.y = value);
            }
        }
    });
    size.connect_value_changed({
        let weak = Rc::downgrade(&ui);
        move |button| {
            if let Some(ui) = weak.upgrade() {
                let value = button.value();
                ui.mutate(|widget| widget.size = value);
            }
        }
    });
    color.connect_rgba_notify({
        let weak = Rc::downgrade(&ui);
        move |button| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let rgba = button.rgba();
            let hex = format!(
                "#{:02x}{:02x}{:02x}",
                (rgba.red() * 255.0).round() as u8,
                (rgba.green() * 255.0).round() as u8,
                (rgba.blue() * 255.0).round() as u8
            );
            ui.mutate(|widget| widget.color = hex);
        }
    });
    opacity.connect_value_changed({
        let weak = Rc::downgrade(&ui);
        move |scale| {
            if let Some(ui) = weak.upgrade() {
                let value = scale.value();
                ui.mutate(|widget| widget.opacity = value);
            }
        }
    });
    shade.connect_active_notify({
        let weak = Rc::downgrade(&ui);
        move |switch| {
            if let Some(ui) = weak.upgrade() {
                let active = switch.is_active();
                ui.mutate(|widget| widget.shade = active);
            }
        }
    });
    font.connect_changed({
        let weak = Rc::downgrade(&ui);
        move |entry| {
            if let Some(ui) = weak.upgrade() {
                let text = entry.text().to_string();
                ui.mutate(|widget| widget.font = text);
            }
        }
    });
    format.connect_changed({
        let weak = Rc::downgrade(&ui);
        move |entry| {
            if let Some(ui) = weak.upgrade() {
                let text = entry.text().to_string();
                ui.mutate(|widget| widget.format = text);
            }
        }
    });
    enabled.connect_active_notify({
        let weak = Rc::downgrade(&ui);
        move |switch| {
            if let Some(ui) = weak.upgrade() {
                let active = switch.is_active();
                ui.mutate(|widget| widget.enabled = active);
            }
        }
    });

    // Repaint the labels when the preview changes size.
    // GTK4 has no `width`/`height` notification on widgets: poll the allocation.
    glib::timeout_add_local(std::time::Duration::from_millis(120), {
        let weak = Rc::downgrade(&ui);
        move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            ui.refresh_canvas();
            glib::ControlFlow::Continue
        }
    });

    ui.rebuild();
    ui.select(None);
    window.present();
    ui.start_renderer();
}

/// Poster of the current wallpaper, if it is a local file.
fn preview_poster(state: &AppState) -> Option<std::path::PathBuf> {
    let path = {
        let cfg = state.cfg.borrow();
        let first = state.monitors.borrow().first().cloned().unwrap_or_default();
        cfg.path_for(&first)
    };
    if path.is_empty() {
        return None;
    }
    if let Some(id) = veil::engine::marker_id(&path) {
        return veil::engine::find(id).and_then(|item| item.preview);
    }
    library::poster(std::path::Path::new(&path))
}
