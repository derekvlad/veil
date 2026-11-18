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
    /// Treat the monitors as one canvas and show a single picture across all
    /// of them. Per-monitor overrides are ignored while this is on.
    pub span: bool,
}

impl Default for Wallpaper {
    fn default() -> Self {
        Self {
            path: String::new(),
            mode: Mode::Fill,
            color: "#000000".to_string(),
            span: false,
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
    /// Flip the picture vertically. The GL render path writes bottom-up
    /// frames, so this is on by default; turn it off only if your driver
    /// already delivers top-down pixels.
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
            flip_y: true,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    /// Folder the GUI scans for wallpapers. `~` is expanded.
    pub path: String,
}

impl Default for Library {
    fn default() -> Self {
        Self {
            path: "~/.local/share/veil/wallpapers".to_string(),
        }
    }
}

/// Automatic wallpaper rotation.
///
/// An empty `items` list means "every picture in the library". Entries under
/// `outputs` are per-monitor playlists; monitors without an entry fall back to
/// `items` (or the library scan).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Playlist {
    pub enabled: bool,
    /// Seconds between two changes. Values below 5 are clamped by the daemon.
    pub interval_secs: u64,
    pub shuffle: bool,
    /// Fade the new picture in from black. 0 disables the effect.
    pub fade_secs: f64,
    pub items: Vec<String>,
    pub outputs: BTreeMap<String, Vec<String>>,
}

impl Default for Playlist {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: 300,
            shuffle: true,
            fade_secs: 1.2,
            items: Vec::new(),
            outputs: BTreeMap::new(),
        }
    }
}

impl Playlist {
    /// Seconds between changes, never less than five.
    pub fn interval(&self) -> u64 {
        self.interval_secs.max(5)
    }

    /// Files to rotate through on `output`, expanded and de-duplicated.
    pub fn files_for(&self, output: &str, library: &Path) -> Vec<String> {
        let explicit = self
            .outputs
            .get(output)
            .filter(|list| !list.is_empty())
            .or_else(|| (!self.items.is_empty()).then_some(&self.items));

        let files: Vec<String> = match explicit {
            Some(list) => list.iter().map(|path| expand(path)).collect(),
            None => library_files(library),
        };

        let mut seen = std::collections::BTreeSet::new();
        files
            .into_iter()
            .filter(|path| !path.is_empty() && seen.insert(path.clone()))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub wallpaper: Wallpaper,
    pub playback: Playback,
    pub rules: Rules,
    pub library: Library,
    pub playlist: Playlist,
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
# One picture spread over every monitor instead of one per monitor.
span = false

[playback]
# auto-safe picks VAAPI/VDPAU/NVDEC when the driver supports it.
hwdec = "auto-safe"
scale_filter = "lanczos"
# 0 = native frame rate. Lower values cut upload/compositing cost.
fps_limit = 0
vsync = false
# GL-рендер отдаёт кадр снизу вверх, поэтому по умолчанию переворачиваем.
flip_y = true
mute = true

[rules]
pause_on_fullscreen = true
pause_on_maximized = false
pause_on_idle = true
idle_timeout_secs = 120
pause_on_battery = false

[library]
# Folder with the wallpapers shown in the GUI.
path = "~/.local/share/veil/wallpapers"

[playlist]
# Rotate the wallpaper automatically. An empty `items` list means every
# picture in the library; `[playlist.outputs]` holds per-monitor lists.
enabled = false
interval_secs = 300
shuffle = true
# Fade the new picture in from black. 0 disables it.
fade_secs = 1.2
items = []

[playlist.outputs]
# "DP-1" = ["/home/user/Videos/left.mp4", "/home/user/Videos/left2.mp4"]

# [outputs]
# "DP-1" = "/home/user/Videos/other.mp4"
"##;

const VIDEO_EXT: &[&str] = &["mp4", "webm", "mkv", "mov", "m4v", "avi", "ogv"];
const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "webp", "avif", "gif", "bmp"];

/// True for extensions the daemon can hand to mpv.
pub fn is_media_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let ext = ext.to_ascii_lowercase();
            VIDEO_EXT.contains(&ext.as_str()) || IMAGE_EXT.contains(&ext.as_str())
        })
        .unwrap_or(false)
}

/// Sorted list of playable files directly inside `dir` (no recursion).
pub fn library_files(dir: &Path) -> Vec<String> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = read
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_media_file(path))
        .collect();
    paths.sort_by_key(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });
    paths
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

pub fn expand(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(rest)
                .to_string_lossy()
                .into_owned();
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
    ///
    /// In span mode there is exactly one picture for the whole canvas, so
    /// overrides are ignored until span is turned off again.
    pub fn path_for(&self, output: &str) -> String {
        if self.wallpaper.span {
            return expand(&self.wallpaper.path);
        }
        self.outputs
            .get(output)
            .map(|p| expand(p))
            .unwrap_or_else(|| expand(&self.wallpaper.path))
    }

    pub fn has_wallpaper(&self) -> bool {
        !self.wallpaper.path.is_empty() || !self.outputs.is_empty()
    }

    /// True when `output` has its own picture instead of the global one.
    pub fn has_override(&self, output: &str) -> bool {
        !self.wallpaper.span && self.outputs.contains_key(output)
    }

    /// Playlist files for one monitor (expanded, de-duplicated).
    pub fn playlist_for(&self, output: &str) -> Vec<String> {
        self.playlist.files_for(output, &self.library_path())
    }

    /// Persist the configuration, keeping a human-readable header.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let body = toml::to_string_pretty(self)
            .map_err(|e| format!("cannot serialise the configuration: {e}"))?;
        let text = format!(
            "# veil — video wallpapers for Wayland\n\
             # Written by veil-gui; the daemon re-reads this file on SIGHUP/reload.\n\n{body}"
        );
        fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Wallpaper library folder, expanded and with a sane fallback.
    pub fn library_path(&self) -> PathBuf {
        let raw = if self.library.path.is_empty() {
            Library::default().path
        } else {
            self.library.path.clone()
        };
        PathBuf::from(expand(&raw))
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
        assert!(cfg.library.path.ends_with("wallpapers"));
        assert!(!cfg.has_wallpaper());
    }

    #[test]
    fn output_override_wins() {
        let mut cfg = Config::default();
        cfg.wallpaper.path = "/a.mp4".into();
        cfg.outputs.insert("DP-1".into(), "/b.mp4".into());
        assert_eq!(cfg.path_for("DP-1"), "/b.mp4");
        assert_eq!(cfg.path_for("HDMI-A-1"), "/a.mp4");
        assert!(cfg.has_override("DP-1"));
    }

    #[test]
    fn span_ignores_overrides() {
        let mut cfg = Config::default();
        cfg.wallpaper.path = "/a.mp4".into();
        cfg.wallpaper.span = true;
        cfg.outputs.insert("DP-1".into(), "/b.mp4".into());
        assert_eq!(cfg.path_for("DP-1"), "/a.mp4");
        assert!(!cfg.has_override("DP-1"));
    }

    #[test]
    fn playlist_falls_back_to_library() {
        let dir = std::env::temp_dir().join(format!("veil-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("b.mp4"), b"x").unwrap();
        fs::write(dir.join("a.png"), b"x").unwrap();
        fs::write(dir.join("notes.txt"), b"x").unwrap();

        let playlist = Playlist::default();
        let files = playlist.files_for("DP-1", &dir);
        assert_eq!(files.len(), 2);
        assert!(files[0].ends_with("a.png"));
        assert!(files[1].ends_with("b.mp4"));

        let mut playlist = playlist;
        playlist.items = vec!["~/one.mp4".into(), "~/one.mp4".into()];
        let files = playlist.files_for("DP-1", &dir);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("one.mp4"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_round_trip() {
        let mut cfg = Config::default();
        cfg.wallpaper.path = "/tmp/a.mp4".into();
        cfg.wallpaper.span = true;
        cfg.playlist.enabled = true;
        cfg.playlist.interval_secs = 42;
        cfg.playlist.items = vec!["/tmp/b.mp4".into()];
        cfg.playlist
            .outputs
            .insert("DP-1".into(), vec!["/tmp/c.mp4".into()]);

        let dir = std::env::temp_dir().join(format!("veil-cfg-{}", std::process::id()));
        let path = dir.join("config.toml");
        cfg.save_to(&path).unwrap();
        let (loaded, _) = Config::load_from(&path).unwrap();
        assert!(loaded.wallpaper.span);
        assert_eq!(loaded.playlist.interval_secs, 42);
        assert_eq!(loaded.playlist.outputs["DP-1"], ["/tmp/c.mp4"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
