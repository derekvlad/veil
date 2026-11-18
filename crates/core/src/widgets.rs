//! Desktop widgets: small pieces of text drawn on top of the wallpaper.
//!
//! Widgets are plain data here — what to draw and where. The `veil-widgets`
//! process turns them into layer-shell windows, and the GUI edits the same
//! structs, so both always agree on the geometry.
//!
//! Positions are percentages of the monitor, not pixels: one configuration
//! works on a 1080p screen and on a 4K one.

use serde::{Deserialize, Deserializer, Serialize};

/// What a widget shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Time and date, formatted with `format`.
    Clock,
    /// CPU load and memory usage.
    Stats,
    /// Battery charge, when the machine has one.
    Battery,
    /// The track that MPRIS players report.
    Mpris,
    /// Whatever `format` says.
    Text,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Clock,
        Kind::Stats,
        Kind::Battery,
        Kind::Mpris,
        Kind::Text,
    ];

    /// Name used in the configuration and on the command line.
    pub fn id(self) -> &'static str {
        match self {
            Kind::Clock => "clock",
            Kind::Stats => "stats",
            Kind::Battery => "battery",
            Kind::Mpris => "mpris",
            Kind::Text => "text",
        }
    }

    pub fn label(self, russian: bool) -> &'static str {
        match (self, russian) {
            (Kind::Clock, true) => "Часы",
            (Kind::Clock, false) => "Clock",
            (Kind::Stats, true) => "Нагрузка",
            (Kind::Stats, false) => "System load",
            (Kind::Battery, true) => "Батарея",
            (Kind::Battery, false) => "Battery",
            (Kind::Mpris, true) => "Сейчас играет",
            (Kind::Mpris, false) => "Now playing",
            (Kind::Text, true) => "Текст",
            (Kind::Text, false) => "Text",
        }
    }

    pub fn parse(text: &str) -> Option<Kind> {
        Kind::ALL
            .into_iter()
            .find(|kind| kind.id() == text.trim().to_ascii_lowercase())
    }
}

/// Which point of the widget box sits at its position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Start,
    Center,
    End,
}

impl Align {
    /// Horizontal shift of the box as a fraction of its width (0, ½ or 1).
    pub fn fraction(self) -> f64 {
        match self {
            Align::Start => 0.0,
            Align::Center => 0.5,
            Align::End => 1.0,
        }
    }

    pub fn from_fraction(value: f64) -> Align {
        if value <= 0.25 {
            Align::Start
        } else if value >= 0.75 {
            Align::End
        } else {
            Align::Center
        }
    }
}

/// One widget in `[[widgets]]`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Widget {
    pub kind: Kind,
    /// Monitor name, `all` or an empty string for every screen.
    pub output: String,
    /// Anchor point in percent of the monitor, 0…100.
    pub x: f64,
    pub y: f64,
    pub halign: Align,
    pub valign: Align,
    /// Font size in points.
    pub size: f64,
    /// `#rrggbb`.
    pub color: String,
    /// 0…1.
    pub opacity: f64,
    /// A soft plate behind the text, so it stays readable on bright frames.
    pub shade: bool,
    /// Font family; empty means the interface font.
    pub font: String,
    /// What to draw: strftime-like for clocks, a template for the rest.
    pub format: String,
    pub enabled: bool,
}

impl Widget {
    /// A sensible widget of the given kind, ready to be dropped on a screen.
    pub fn new(kind: Kind) -> Self {
        let mut widget = Widget {
            kind,
            output: String::new(),
            x: 50.0,
            y: 6.0,
            halign: Align::Center,
            valign: Align::Start,
            size: 24.0,
            color: "#ffffff".into(),
            opacity: 0.9,
            shade: true,
            font: String::new(),
            format: String::new(),
            enabled: true,
        };
        match kind {
            Kind::Clock => {
                widget.size = 52.0;
                widget.format = "%H:%M\n%d %B".into();
            }
            Kind::Stats => {
                widget.x = 2.0;
                widget.y = 3.0;
                widget.halign = Align::Start;
                widget.size = 16.0;
                widget.format = "CPU {cpu}% · RAM {used}/{total} GB".into();
            }
            Kind::Battery => {
                widget.x = 98.0;
                widget.y = 3.0;
                widget.halign = Align::End;
                widget.size = 16.0;
            }
            Kind::Mpris => {
                widget.x = 2.0;
                widget.y = 97.0;
                widget.valign = Align::End;
                widget.size = 16.0;
            }
            Kind::Text => {
                widget.size = 24.0;
                widget.format = "veil".into();
            }
        }
        widget
    }

    /// Keep the values usable: anything a config typo could break is clamped.
    pub fn sanitize(&mut self) {
        self.x = self.x.clamp(0.0, 100.0);
        self.y = self.y.clamp(0.0, 100.0);
        self.size = if self.size.is_finite() {
            self.size.clamp(6.0, 400.0)
        } else {
            24.0
        };
        self.opacity = if self.opacity.is_finite() {
            self.opacity.clamp(0.05, 1.0)
        } else {
            1.0
        };
        if let Some(color) = normalize_color(&self.color) {
            self.color = color;
        } else {
            self.color = "#ffffff".into();
        }
        if self.output.eq_ignore_ascii_case("all") {
            self.output.clear();
        }
        self.output = self.output.trim().to_string();
    }

    /// Whether the widget belongs on this monitor.
    pub fn shows_on(&self, output: &str) -> bool {
        self.enabled && (self.output.is_empty() || self.output == output)
    }

    /// Top-left corner of the widget box, in pixels, for a box of `width`×`height`.
    pub fn position(&self, screen: (i32, i32), box_size: (i32, i32)) -> (i32, i32) {
        let x = (screen.0 as f64 * self.x / 100.0).round() as i32;
        let y = (screen.1 as f64 * self.y / 100.0).round() as i32;
        let left = x - (box_size.0 as f64 * self.halign.fraction()).round() as i32;
        let top = y - (box_size.1 as f64 * self.valign.fraction()).round() as i32;
        // Never park a widget completely off-screen.
        (
            left.clamp(0, (screen.0 - box_size.0).max(0)),
            top.clamp(0, (screen.1 - box_size.1).max(0)),
        )
    }

    /// One-line summary for `veilctl widgets list` and the GUI list.
    pub fn summary(&self) -> String {
        let output = if self.output.is_empty() {
            "all"
        } else {
            self.output.as_str()
        };
        format!(
            "{} · {output} · {:.0}%/{:.0}% · {}pt",
            self.kind.id(),
            self.x,
            self.y,
            self.size
        )
    }
}

impl<'de> Deserialize<'de> for Widget {
    /// Defaults depend on the kind, so a bare `kind = "clock"` already looks
    /// like a clock instead of a generic label in the top-left corner.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            kind: Kind,
            output: Option<String>,
            x: Option<f64>,
            y: Option<f64>,
            halign: Option<Align>,
            valign: Option<Align>,
            size: Option<f64>,
            color: Option<String>,
            opacity: Option<f64>,
            shade: Option<bool>,
            font: Option<String>,
            format: Option<String>,
            enabled: Option<bool>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let mut widget = Widget::new(raw.kind);
        if let Some(value) = raw.output {
            widget.output = value;
        }
        if let Some(value) = raw.x {
            widget.x = value;
        }
        if let Some(value) = raw.y {
            widget.y = value;
        }
        if let Some(value) = raw.halign {
            widget.halign = value;
        }
        if let Some(value) = raw.valign {
            widget.valign = value;
        }
        if let Some(value) = raw.size {
            widget.size = value;
        }
        if let Some(value) = raw.color {
            widget.color = value;
        }
        if let Some(value) = raw.opacity {
            widget.opacity = value;
        }
        if let Some(value) = raw.shade {
            widget.shade = value;
        }
        if let Some(value) = raw.font {
            widget.font = value;
        }
        if let Some(value) = raw.format {
            widget.format = value;
        }
        if let Some(value) = raw.enabled {
            widget.enabled = value;
        }
        Ok(widget)
    }
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa` to `#rrggbb` (alpha is not supported yet).
pub fn normalize_color(text: &str) -> Option<String> {
    let text = text.trim().trim_start_matches('#');
    let digits: Vec<char> = text.chars().collect();
    if !digits.iter().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let short: String = match digits.len() {
        3 => digits.iter().flat_map(|c| [*c, *c]).collect(),
        6 | 8 => digits[..6].iter().collect(),
        _ => return None,
    };
    Some(format!("#{}", short.to_ascii_lowercase()))
}

/// A wall clock, split into fields so the formatting is testable without libc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parts {
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub day: u32,
    pub month: u32,
    pub year: u32,
    /// 0 = Sunday, as `struct tm` reports it.
    pub weekday: u32,
}

/// Render a strftime-like `format`.
///
/// Supported: `%H` `%M` `%S` `%d` `%m` `%Y` `%A` `%B` `%%`, plus a literal
/// `\n`. Anything else is left as it is, so a typo is visible, not fatal.
pub fn render_time(format: &str, parts: &Parts, russian: bool) -> String {
    let months_ru = [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
    ];
    let months_en = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let days_ru = [
        "воскресенье",
        "понедельник",
        "вторник",
        "среда",
        "четверг",
        "пятница",
        "суббота",
    ];
    let days_en = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    let months = if russian { months_ru } else { months_en };
    let days = if russian { days_ru } else { days_en };
    let month = || {
        months
            .get(parts.month.saturating_sub(1) as usize)
            .copied()
            .unwrap_or("")
    };
    let weekday = || days.get(parts.weekday as usize % 7).copied().unwrap_or("");

    let mut out = String::with_capacity(format.len() + 8);
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        match c {
            '%' => match chars.next() {
                Some('H') => out.push_str(&format!("{:02}", parts.hour)),
                Some('M') => out.push_str(&format!("{:02}", parts.minute)),
                Some('S') => out.push_str(&format!("{:02}", parts.second)),
                Some('d') => out.push_str(&format!("{:02}", parts.day)),
                Some('m') => out.push_str(&format!("{:02}", parts.month)),
                Some('Y') => out.push_str(&parts.year.to_string()),
                Some('A') => out.push_str(weekday()),
                Some('B') => out.push_str(month()),
                Some('%') => out.push('%'),
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
                None => out.push('%'),
            },
            // A literal backslash-n is friendlier in TOML than an escape.
            '\\' if chars.clone().next() == Some('n') => {
                chars.next();
                out.push('\n');
            }
            other => out.push(other),
        }
    }
    out
}

/// Fill the `{cpu}`, `{used}`, `{total}`, `{ram}` placeholders of a stats line.
pub fn render_stats(format: &str, cpu: f64, used_gb: f64, total_gb: f64) -> String {
    let percent = if total_gb > 0.0 {
        used_gb / total_gb * 100.0
    } else {
        0.0
    };
    format
        .replace("{cpu}", &format!("{:.0}", cpu))
        .replace("{ram}", &format!("{:.0}", percent))
        .replace("{used}", &format!("{:.1}", used_gb))
        .replace("{total}", &format!("{:.1}", total_gb))
}

/// `Battery: 74% (charging)` without pulling a formatter into the renderer.
pub fn render_battery(
    format: &str,
    capacity: u8,
    charging: bool,
    full: bool,
    russian: bool,
) -> String {
    let state = match (charging, full, russian) {
        (false, _, true) => "заряжено",
        (true, _, false) => "заряжается",
        (_, true, true) => "заряжено",
        (_, _, true) => "заряжается",
        (_, _, false) => "от батареи",
    };
    let state_en = match (charging, full) {
        (true, _) => "charging",
        (_, true) => "full",
        _ => "on battery",
    };
    format
        .replace("{percent}", &capacity.to_string())
        .replace("{state}", if russian { state } else { state_en })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_kind_gets_that_kinds_geometry() {
        let clock: Widget = toml::from_str(r#"kind = "clock""#).unwrap();
        assert_eq!(clock.size, 52.0);
        assert_eq!(clock.halign, Align::Center);
        assert_eq!(clock.format, "%H:%M\n%d %B");
        assert!(clock.enabled);

        let stats: Widget = toml::from_str("kind = \"stats\"\nsize = 20").unwrap();
        assert_eq!(stats.halign, Align::Start);
        assert_eq!(stats.size, 20.0);
        assert_eq!(stats.x, 2.0);
        assert!(stats.format.starts_with("CPU {cpu}%"));
    }

    #[test]
    fn round_trips_through_toml() {
        let mut widget = Widget::new(Kind::Mpris);
        widget.output = "DP-1".into();
        widget.x = 12.5;
        widget.color = "#aabbcc".into();
        widget.shade = false;
        let text = toml::to_string(&widget).unwrap();
        let back: Widget = toml::from_str(&text).unwrap();
        assert_eq!(widget, back);
    }

    #[test]
    fn sanitize_survives_a_broken_config() {
        let mut widget = Widget::new(Kind::Text);
        widget.x = -20.0;
        widget.y = 400.0;
        widget.size = f64::NAN;
        widget.opacity = 9.0;
        widget.color = "red".into();
        widget.output = "ALL".into();
        widget.sanitize();
        assert_eq!(widget.x, 0.0);
        assert_eq!(widget.y, 100.0);
        assert_eq!(widget.size, 24.0);
        assert_eq!(widget.opacity, 1.0);
        assert_eq!(widget.color, "#ffffff");
        assert!(widget.output.is_empty());
    }

    #[test]
    fn anchors_land_inside_the_screen() {
        let mut widget = Widget::new(Kind::Text);
        widget.halign = Align::End;
        widget.valign = Align::End;
        widget.x = 100.0;
        widget.y = 100.0;
        // A 200×40 box on a 1920×1080 screen ends flush with the corner.
        assert_eq!(widget.position((1920, 1080), (200, 40)), (1720, 1040));
        // Off-screen-ish positions are clamped, never negative.
        widget.x = 0.0;
        assert_eq!(widget.position((1920, 1080), (200, 40)), (0, 1040));
        let mut huge = Widget::new(Kind::Text);
        huge.halign = Align::Center;
        assert_eq!(huge.position((800, 600), (2000, 40)), (0, 36));
    }

    #[test]
    fn colors_are_normalized_or_rejected() {
        assert_eq!(normalize_color("#abc").unwrap(), "#aabbcc");
        assert_eq!(normalize_color("AABBCC").unwrap(), "#aabbcc");
        // Alpha is dropped, not rejected.
        assert_eq!(normalize_color("#11223344").unwrap(), "#112233");
        assert!(normalize_color("red").is_none());
        assert!(normalize_color("#12345").is_none());
    }

    #[test]
    fn clock_formatting_is_bilingual() {
        let parts = Parts {
            hour: 9,
            minute: 5,
            second: 3,
            day: 18,
            month: 11,
            year: 2026,
            weekday: 3,
        };
        assert_eq!(render_time("%H:%M", &parts, true), "09:05");
        assert_eq!(render_time("%d %B %Y", &parts, true), "18 ноября 2026");
        assert_eq!(render_time("%A", &parts, false), "Wednesday");
        assert_eq!(render_time("%A", &parts, true), "среда");
        assert_eq!(render_time("100%%", &parts, true), "100%");
        // Unknown directives stay visible instead of disappearing.
        assert_eq!(render_time("%Q", &parts, true), "%Q");
        assert_eq!(render_time("a\\nb", &parts, true), "a\nb");
    }

    #[test]
    fn stats_and_battery_templates() {
        assert_eq!(
            render_stats("CPU {cpu}% · RAM {used}/{total} GB", 12.4, 8.25, 15.5),
            "CPU 12% · RAM 8.2/15.5 GB"
        );
        assert_eq!(render_stats("RAM {ram}%", 0.0, 0.0, 0.0), "RAM 0%");
        assert_eq!(
            render_battery("{percent}% {state}", 74, true, false, true),
            "74% заряжается"
        );
        assert_eq!(
            render_battery("{percent}% {state}", 74, false, false, false),
            "74% on battery"
        );
    }

    #[test]
    fn widgets_pick_their_monitors() {
        let mut everywhere = Widget::new(Kind::Clock);
        assert!(everywhere.shows_on("DP-1"));
        let only_dp = Widget {
            output: "DP-1".into(),
            ..Widget::new(Kind::Clock)
        };
        assert!(only_dp.shows_on("DP-1"));
        assert!(!only_dp.shows_on("HDMI-A-1"));
        let off = Widget {
            enabled: false,
            ..Widget::new(Kind::Clock)
        };
        assert!(!off.shows_on("DP-1"));
    }

    #[test]
    fn kinds_and_summaries_read_well() {
        assert_eq!(Kind::parse("MPRIS"), Some(Kind::Mpris));
        assert_eq!(Kind::parse("clock "), Some(Kind::Clock));
        assert_eq!(Kind::parse("bogus"), None);
        let widget = Widget::new(Kind::Battery);
        assert_eq!(widget.summary(), "battery · all · 98%/3% · 16pt");
    }
}
