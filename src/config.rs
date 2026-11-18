//! Configuration: where the wallpaper lives and how it should be played.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// How the picture is fitted into a monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Cover the screen, cropping the overflow (default).
    Fill,
    /// Show the whole picture, letterboxing the rest.
    Fit,
    /// Stretch to the screen, ignoring the aspect ratio.
    Stretch,
    /// No scaling at all: native size, centred.
    Center,
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Fill
    }
}

impl std::str::FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "fill" | "cover" => Ok(Mode::Fill),
            "fit" | "contain" => Ok(Mode::Fit),
            "stretch" => Ok(Mode::Stretch),
            "center" => Ok(Mode::Center),
            other => Err(format!("unknown fill mode `{other}`")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Wallpaper {
    /// Path to a video or still image. `~` is expanded.
    pub path: String,
    pub mode: Mode,
    /// Colour behind the picture (letterboxing, or before the first frame).
    pub color: String,
}

impl Default for Wallpaper {
    fn default() -> Self {
        Self {
            path: String::new(),
            mode: Mode::Fill,
            color: "#000000".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Playback {
    /// mpv hwdec mode: auto-safe (default), vaapi, vaapi-copy, vulkan, no.
    pub hwdec: String,
    /// GPU scaler used when the source is larger than the screen.
    pub scale_filter: String,
    /// Soft cap on presented frames per second (0 = follow the video).
    /// This does not reduce decoding work, only upload/compositing.
    pub fps_limit: u32,
    /// Wait for vblank before presenting. Smoother on a single monitor,
    /// can add a frame of latency with several.
    pub vsync: bool,
    /// Set to true if the image appears upside down.
    pub flip_y: bool,
    /// Silence the player (video wallpapers are always silent for now).
    pub mute: bool,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            hwdec: "auto-safe".to_string(),
            scale_filter: "lanczos".to_string(),
            fps_limit: 0,
            vsync: false,
            flip_y: false,
            mute: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Rules {
    pub pause_on_fullscreen: bool,
    pub pause_on_maximized: bool,
    pub pause_on_idle: bool,
    pub idle_timeout_secs: u64,
    pub pause_on_battery: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            pause_on_fullscreen: true,
            pause_on_maximized: false,
            pause_on_idle: true,
            idle_timeout_secs: 120,
            pause_on_battery: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub wallpaper: Wallpaper,
    pub playback: Playback,
    pub rules: Rules,
    /// Per-monitor overrides: key is the wl_output name (e.g. "DP-1").
    pub outputs: BTreeMap<String, String>,
}

pub const DEFAULT_CONFIG: &str = r##"# veil — video wallpapers for Wayland
#
# The daemon reloads this file on SIGHUP or `veilctl reload`.

[wallpaper]
# Video or image. Leave empty to let the compositor draw its own background.
path = ""
# fill | fit | stretch | center
mode = "fill"
color = "#000000"

[playback]
# auto-safe picks VAAPI/VDPAU/NVDEC when the driver supports it.
hwdec = "auto-safe"
scale_filter = "lanczos"
# 0 = native frame rate. Lower values cut upload/compositing cost.
fps_limit = 0
vsync = false
flip_y = false
mute = true

[rules]
pause_on_fullscreen = true
pause_on_maximized = false
pause_on_idle = true
idle_timeout_secs = 120
pause_on_battery = false

# [outputs]
# "DP-1" = "/home/user/Videos/other.mp4"
"##;

pub fn expand(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("veil").join("config.toml")
}

impl Config {
    pub fn load() -> Result<(Self, PathBuf), String> {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Result<(Self, PathBuf), String> {
        if !path.exists() {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            fs::write(path, DEFAULT_CONFIG).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let cfg: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok((cfg, path.to_path_buf()))
    }

    /// Wallpaper for a given monitor, honouring per-output overrides.
    pub fn path_for(&self, output: &str) -> String {
        self.outputs
            .get(output)
            .map(|p| expand(p))
            .unwrap_or_else(|| expand(&self.wallpaper.path))
    }

    pub fn has_wallpaper(&self) -> bool {
        !self.wallpaper.path.is_empty() || !self.outputs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_defaults() {
        let cfg: Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(cfg.wallpaper.mode, Mode::Fill);
        assert_eq!(cfg.playback.hwdec, "auto-safe");
        assert!(cfg.rules.pause_on_fullscreen);
        assert!(!cfg.has_wallpaper());
    }

    #[test]
    fn output_override_wins() {
        let mut cfg = Config::default();
        cfg.wallpaper.path = "/a.mp4".into();
        cfg.outputs.insert("DP-1".into(), "/b.mp4".into());
        assert_eq!(cfg.path_for("DP-1"), "/b.mp4");
        assert_eq!(cfg.path_for("HDMI-A-1"), "/a.mp4");
    }
}
