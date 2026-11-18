//! Wallpaper Engine content and the external `linux-wallpaperengine` engine.
//!
//! Scenes are not video: they are a `scene.json` plus shaders and textures that
//! only Wallpaper Engine's runtime understands. Instead of reimplementing that
//! runtime, veil hands those wallpapers to `linux-wallpaperengine` and keeps
//! drawing its own videos itself.
//!
//! Everything here is plain data work: locating Steam libraries, parsing
//! `project.json`, and building the command line for the engine. Nothing in
//! this module spawns processes — the daemon owns the child.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Steam's application id for Wallpaper Engine.
pub const APP_ID: u32 = 431960;
/// Prefix that marks a wallpaper as "play through the engine".
pub const MARKER: &str = "we:";

/// One wallpaper found in a Steam Workshop folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Workshop id, digits only.
    pub id: String,
    pub title: String,
    /// `scene`, `video`, `web` and so on, lower-cased.
    pub kind: String,
    /// Folder with `project.json`.
    pub dir: PathBuf,
    /// Main asset from `project.json` (`file`), relative to `dir`.
    pub file: String,
    /// Preview image, when the author shipped one.
    pub preview: Option<PathBuf>,
    /// Whether `linux-wallpaperengine` can render this kind.
    pub supported: bool,
}

impl Item {
    /// Absolute path of the main asset.
    pub fn asset(&self) -> PathBuf {
        self.dir.join(&self.file)
    }

    /// The engine marker stored in the config and the IPC protocol.
    pub fn marker(&self) -> String {
        format!("{MARKER}{}", self.id)
    }
}

/// Raw `project.json`, only the fields we need.
#[derive(Debug, Deserialize)]
struct Project {
    #[serde(default)]
    title: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    file: String,
    #[serde(default)]
    workshopid: Option<serde_json::Value>,
    #[serde(default)]
    preview: String,
}

/// `Some(id)` when the wallpaper string is an engine marker.
pub fn marker_id(path: &str) -> Option<&str> {
    path.strip_prefix(MARKER).map(str::trim)
}

/// Read one `project.json` into an [`Item`].
pub fn parse_project(dir: &Path, text: &str) -> Result<Item, String> {
    let project: Project =
        serde_json::from_str(text).map_err(|error| format!("{}: {error}", dir.display()))?;
    let kind = project.kind.to_ascii_lowercase();
    // Application and unknown wallpapers keep their code inside a `.pkg` (or
    // rely on a dependency's), so there is often no `file` at all — and no
    // engine that could run them either. Scenes, videos and web pages name a
    // main file the engine knows how to open; a web wallpaper that only points
    // at a dependency has none and stays listed as unusable.
    let has_file = !project.file.is_empty();
    let supported = has_file && matches!(kind.as_str(), "video" | "scene" | "web");
    if matches!(kind.as_str(), "video" | "scene") && !has_file {
        return Err(format!("{}: project has no `file`", dir.display()));
    }
    let id = project
        .workshopid
        .as_ref()
        .and_then(|value| match value {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .or_else(|| {
            dir.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let id = id.trim().to_string();
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("{}: bad workshop id `{id}`", dir.display()));
    }

    let preview_name = if project.preview.is_empty() {
        "preview.jpg".to_string()
    } else {
        project.preview
    };
    let preview = dir.join(preview_name);
    let preview = preview.is_file().then_some(preview);

    let title = if project.title.is_empty() {
        id.clone()
    } else {
        project.title
    };

    Ok(Item {
        id,
        title,
        kind,
        dir: dir.to_path_buf(),
        file: project.file,
        preview,
        supported,
    })
}

/// Every Steam library that has the client installed.
pub fn steam_libraries() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join(".local/share/Steam"));
        roots.push(home.join(".steam/steam"));
        roots.push(home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
        roots.push(home.join("snap/steam/common/.local/share/Steam"));
    }
    roots.extend(
        std::env::var_os("VEIL_STEAM_ROOT")
            .map(PathBuf::from)
            .into_iter(),
    );

    let mut libraries = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let vdf = root.join("steamapps/libraryfolders.vdf");
        if let Ok(text) = fs::read_to_string(&vdf) {
            for path in vdf_paths(&text) {
                push_library(&mut libraries, PathBuf::from(path));
            }
        }
        push_library(&mut libraries, root);
    }
    libraries
}

/// Add a library once, resolving `~/.steam/steam` to the real store folder.
fn push_library(libraries: &mut Vec<PathBuf>, path: PathBuf) {
    let path = fs::canonicalize(&path).unwrap_or(path);
    if path.is_dir() && !libraries.contains(&path) {
        libraries.push(path);
    }
}

/// Pull `"path" "…"` entries out of `libraryfolders.vdf`.
///
/// The file is a Valve KeyValues document; a full parser is overkill for the
/// one key we need, and this keeps the module dependency-free.
pub fn vdf_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("\"path\"") else {
            continue;
        };
        let rest = rest.trim();
        let Some(rest) = rest.strip_prefix('"') else {
            continue;
        };
        if let Some(end) = rest.rfind('"') {
            let path = &rest[..end];
            if !path.is_empty() {
                paths.push(path.to_string());
            }
        }
    }
    paths
}

/// Folder holding Wallpaper Engine's own assets (shaders, textures, models).
pub fn assets_dir() -> Option<PathBuf> {
    for library in steam_libraries() {
        let assets = library.join(format!("steamapps/common/wallpaper_engine/assets"));
        if assets.is_dir() {
            return Some(assets);
        }
    }
    None
}

/// Workshop items for Wallpaper Engine in every Steam library.
pub fn scan_workshop() -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    for library in steam_libraries() {
        let content = library.join(format!("steamapps/workshop/content/{APP_ID}"));
        let Ok(entries) = fs::read_dir(&content) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Ok(text) = fs::read_to_string(dir.join("project.json")) else {
                continue;
            };
            match parse_project(&dir, &text) {
                Ok(item) => {
                    if !items.iter().any(|known| known.id == item.id) {
                        items.push(item);
                    }
                }
                Err(error) => log_line(&format!("{error}")),
            }
        }
    }
    items.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    items
}

/// Find one workshop item by id.
pub fn find(id: &str) -> Option<Item> {
    scan_workshop().into_iter().find(|item| item.id == id)
}

/// The CEF library to preload, when the engine needs it.
///
/// Chromium resolves libc's `close()` from its interposer with
/// `dlsym(RTLD_NEXT, "close")`, which only works if libcef precedes libc in the
/// load order. Builds that link libcef only through the engine's own shared
/// library get libcef after libc and crash on every web wallpaper with
/// "close symbol missing" (upstream issue #628). Preloading libcef restores the
/// order the interposer expects.
///
/// `configured` is `[engine] preload`: empty means "look for it", `none` or
/// `off` disables the workaround, and any other value is a path.
pub fn libcef(binary: &Path, configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if matches!(configured, "none" | "off") {
        return None;
    }
    if !configured.is_empty() {
        return Some(PathBuf::from(veil_expand(configured)));
    }

    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("VEIL_LIBCEF") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(dir) = binary.parent() {
        candidates.push(dir.join("libcef.so"));
    }
    // The packaged launcher runs from /opt/<name>, where the library lives.
    candidates.push(PathBuf::from("/opt/linux-wallpaperengine/libcef.so"));
    candidates.into_iter().find(|path| path.is_file())
}

fn veil_expand(path: &str) -> String {
    crate::config::expand(path)
}

/// Which `linux-wallpaperengine` binary to run.
pub fn binary(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return Some(path);
        }
        // A bare name still has to resolve through PATH.
        if !configured.contains('/') {
            if let Some(found) = which(configured) {
                return Some(found);
            }
        }
        return None;
    }
    which("linux-wallpaperengine")
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// One monitor assignment for the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Wayland output name, e.g. `DP-1`.
    pub screen: String,
    /// Workshop id.
    pub id: String,
}

/// Build the command line that paints `targets` with the engine.
pub fn arguments(
    targets: &[Target],
    assets: Option<&Path>,
    scaling: &str,
    properties: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(assets) = assets {
        args.push("--assets-dir".into());
        args.push(assets.display().to_string());
    }
    args.push("--scaling".into());
    args.push(scaling.to_string());
    for (key, value) in properties {
        // The engine silently ignores unknown keys, so a value left over from
        // another scene is harmless.
        args.push("--set-property".into());
        args.push(format!("{key}={value}"));
    }
    for target in targets {
        args.push("--screen-root".into());
        args.push(target.screen.clone());
        args.push("--bg".into());
        args.push(target.id.clone());
    }
    args
}

// ----------------------------------------------------------------- properties

/// What kind of control a scene setting wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyKind {
    Boolean,
    /// One of the keys listed in [`Property::options`].
    Combo,
    Slider,
    /// `R,G,B,A` with every component from 0 to 255.
    Color,
    Text,
    File,
    SceneTexture,
    /// A kind this version of veil does not know; shown read-only.
    Unknown,
}

impl PropertyKind {
    fn parse(text: &str) -> Self {
        match text {
            "boolean" => Self::Boolean,
            "combo" => Self::Combo,
            "slider" => Self::Slider,
            "color" => Self::Color,
            "text" => Self::Text,
            "file" => Self::File,
            "scene texture" => Self::SceneTexture,
            "textinput" => Self::Text,
            _ => Self::Unknown,
        }
    }

    /// Whether the value is free-form enough to be worth an entry.
    pub fn is_textual(self) -> bool {
        matches!(self, Self::Text | Self::File | Self::SceneTexture)
    }
}

/// One entry of `--list-properties`.
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    /// The key passed to `--set-property`, e.g. `clock`.
    pub key: String,
    pub kind: PropertyKind,
    /// Human text from the wallpaper, possibly a translation key.
    pub label: String,
    /// Value in the form the engine accepts it back.
    pub value: String,
    /// What the scene itself declares, before any override.
    pub default: String,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    /// `(key, label)` pairs of a combo box.
    pub options: Vec<(String, String)>,
}

impl Property {
    /// A label fit for a Russian or English window.
    pub fn title(&self) -> String {
        let label = self.label.trim();
        // Wallpaper Engine uses translation keys for some built-in settings.
        if label.is_empty() || label.starts_with("ui_") {
            return self.key.clone();
        }
        label.to_string()
    }

    /// The value shown as the default, for "reset to the scene's own".
    pub fn default_value(&self) -> String {
        self.default.clone()
    }

    /// Turn what a user typed into the string `--set-property` expects.
    ///
    /// The engine is forgiving in odd ways (colours are four floats from its
    /// own dump, but `R,G,B,A` from the command line), so every value a person
    /// can enter is normalised here rather than trusted as typed.
    pub fn normalize(&self, raw: &str) -> Result<String, String> {
        let raw = raw.trim();
        match self.kind {
            PropertyKind::Boolean => match raw.to_ascii_lowercase().as_str() {
                "1" | "on" | "true" | "yes" => Ok(bool_value(true)),
                "0" | "off" | "false" | "no" => Ok(bool_value(false)),
                _ => Err(format!("`{raw}` is not on or off")),
            },
            PropertyKind::Color => normalize_color(raw),
            PropertyKind::Combo => {
                if self.options.iter().any(|(key, _)| key == raw) {
                    return Ok(raw.to_string());
                }
                if let Some((key, _)) = self
                    .options
                    .iter()
                    .find(|(_, label)| label.eq_ignore_ascii_case(raw))
                {
                    return Ok(key.clone());
                }
                let known: Vec<String> = self
                    .options
                    .iter()
                    .map(|(key, label)| format!("{key} = {label}"))
                    .collect();
                Err(format!("`{raw}` is not one of: {}", known.join(", ")))
            }
            PropertyKind::Slider => {
                let value: f64 = raw
                    .parse()
                    .map_err(|_| format!("`{raw}` is not a number"))?;
                let value = value.clamp(self.min, self.max);
                Ok(number_value(value, self.step))
            }
            _ => Ok(raw.to_string()),
        }
    }
}

/// `R,G,B,A`, `#rgb`, `#rrggbb` or `#rrggbbaa` as the engine wants it.
fn normalize_color(raw: &str) -> Result<String, String> {
    let Some(hex) = raw.strip_prefix('#') else {
        let parts: Vec<&str> = raw.split(',').collect();
        if parts.len() != 4 {
            return Err(format!("expected `R,G,B,A`, got `{raw}`"));
        }
        let mut components = [0u8; 4];
        for (slot, part) in components.iter_mut().zip(parts) {
            *slot = part
                .trim()
                .parse()
                .map_err(|_| format!("`{part}` is not a number from 0 to 255"))?;
        }
        return Ok(color_value(components));
    };

    let digits: Vec<u8> = hex
        .chars()
        .map(|c| c.to_digit(16).map(|digit| digit as u8))
        .collect::<Option<_>>()
        .ok_or_else(|| format!("`{raw}` is not a hex colour"))?;
    let pair = |high: u8, low: u8| high * 16 + low;
    let single = |value: u8| value * 17;
    let components = match digits[..] {
        [r, g, b] => [single(r), single(g), single(b), 255],
        [r, g, b, a] => [single(r), single(g), single(b), single(a)],
        [r1, r2, g1, g2, b1, b2] => [pair(r1, r2), pair(g1, g2), pair(b1, b2), 255],
        [r1, r2, g1, g2, b1, b2, a1, a2] => {
            [pair(r1, r2), pair(g1, g2), pair(b1, b2), pair(a1, a2)]
        }
        _ => return Err(format!("`{raw}` is not a hex colour")),
    };
    Ok(color_value(components))
}

/// Parse the output of `linux-wallpaperengine --list-properties`.
pub fn parse_properties(text: &str) -> Vec<Property> {
    let mut properties: Vec<Property> = Vec::new();
    let mut current: Option<Property> = None;
    let mut in_values = false;

    for line in text.lines() {
        let line = line.trim_end();
        // `Values:` sits at the left margin too, but starts the option list.
        if line.trim() == "Values:" && current.is_some() {
            in_values = true;
            continue;
        }
        // A header line is not indented: `clock - boolean`.
        if !line.starts_with(' ') && !line.starts_with('\t') && !line.trim().is_empty() {
            if let Some(done) = current.take() {
                properties.push(done);
            }
            in_values = false;
            let Some((key, kind)) = line.rsplit_once(" - ") else {
                continue;
            };
            current = Some(Property {
                key: key.trim().to_string(),
                kind: PropertyKind::parse(kind.trim()),
                label: String::new(),
                value: String::new(),
                default: String::new(),
                min: 0.0,
                max: 1.0,
                step: 0.01,
                options: Vec::new(),
            });
            continue;
        }

        let Some(property) = current.as_mut() else {
            continue;
        };
        let trimmed = line.trim();
        if trimmed == "Values:" {
            in_values = true;
            continue;
        }
        if in_values {
            if let Some((key, label)) = trimmed.split_once(" = ") {
                property
                    .options
                    .push((key.trim().to_string(), label.trim().to_string()));
            }
            continue;
        }
        if let Some(text) = trimmed.strip_prefix("Text: ") {
            property.label = text.trim().to_string();
        } else if let Some(text) = trimmed.strip_prefix("Min: ") {
            property.min = text.trim().parse().unwrap_or(0.0);
        } else if let Some(text) = trimmed.strip_prefix("Max: ") {
            property.max = text.trim().parse().unwrap_or(1.0);
        } else if let Some(text) = trimmed.strip_prefix("Step: ") {
            property.step = text.trim().parse().unwrap_or(0.01);
        } else if let Some(text) = trimmed.strip_prefix("Value: ") {
            let text = text.trim();
            property.value = match property.kind {
                // The engine prints colours as four floats from 0 to 1.
                PropertyKind::Color => color_from_floats(text),
                // `0.600000` is noise; the step says what matters.
                PropertyKind::Slider => {
                    let step = property.step;
                    text.parse::<f64>()
                        .map(|value| number_value(value, step))
                        .unwrap_or_else(|_| text.to_string())
                }
                _ => text.to_string(),
            };
        }
    }
    if let Some(done) = current.take() {
        properties.push(done);
    }
    for property in &mut properties {
        property.default = property.value.clone();
    }
    properties
}

/// `0.752941, 0.752941, 0.752941, 1.000000` → `192,192,192,255`.
fn color_from_floats(text: &str) -> String {
    let mut parts: Vec<u8> = text
        .split(',')
        .map(|part| {
            let value: f64 = part.trim().parse().unwrap_or(0.0);
            (value.clamp(0.0, 1.0) * 255.0).round() as u8
        })
        .collect();
    while parts.len() < 4 {
        parts.push(255);
    }
    parts.truncate(4);
    format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3])
}

/// `R,G,B,A` in the 0…255 range, as `--set-property` expects it.
pub fn color_value(components: [u8; 4]) -> String {
    format!(
        "{},{},{},{}",
        components[0], components[1], components[2], components[3]
    )
}

/// Split a `R,G,B,A` value from the config back into components.
pub fn color_components(value: &str) -> [u8; 4] {
    let mut parts = [255u8; 4];
    for (slot, part) in parts.iter_mut().zip(value.split(',')) {
        *slot = part.trim().parse().unwrap_or(255);
    }
    parts
}

/// `1` or `0`, the two strings the engine reads as booleans.
pub fn bool_value(enabled: bool) -> String {
    if enabled { "1" } else { "0" }.to_string()
}

/// A slider value without the noise of float formatting.
pub fn number_value(value: f64, step: f64) -> String {
    let digits = match step {
        step if step >= 1.0 => 0,
        step if step >= 0.1 => 1,
        step if step >= 0.01 => 2,
        step if step >= 0.001 => 3,
        _ => 4,
    };
    format!("{value:.digits$}")
}

fn log_line(message: &str) {
    eprintln!("veil: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECT: &str = r#"{
        "title": "Neon Rain",
        "type": "Scene",
        "file": "scene.pkg",
        "workshopid": 1845706469,
        "preview": "preview.gif"
    }"#;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("veil-engine-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_a_scene_project() {
        let dir = temp_dir("scene");
        fs::write(dir.join("preview.gif"), b"gif").unwrap();
        let item = parse_project(&dir, PROJECT).unwrap();
        assert_eq!(item.id, "1845706469");
        assert_eq!(item.title, "Neon Rain");
        assert_eq!(item.kind, "scene");
        assert!(item.supported);
        assert_eq!(item.marker(), "we:1845706469");
        assert_eq!(item.asset(), dir.join("scene.pkg"));
        assert_eq!(item.preview, Some(dir.join("preview.gif")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn falls_back_to_the_folder_name_for_the_id() {
        let dir = temp_dir("no-id");
        let folder = dir.join("123456");
        fs::create_dir_all(&folder).unwrap();
        let item = parse_project(&folder, r#"{"type":"video","file":"a.mp4"}"#).unwrap();
        assert_eq!(item.id, "123456");
        assert_eq!(item.title, "123456");
        assert!(item.supported);
        assert!(item.preview.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_projects_without_a_file_and_bad_ids() {
        let dir = temp_dir("bad");
        assert!(parse_project(&dir, r#"{"type":"video"}"#).is_err());
        assert!(parse_project(&dir, r#"{"type":"video","file":"a.mp4"}"#).is_err());
        let folder = dir.join("not-digits");
        fs::create_dir_all(&folder).unwrap();
        assert!(parse_project(&folder, r#"{"title":"x","type":"video","file":"a.mp4"}"#).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn marks_kinds_the_engine_cannot_render() {
        let dir = temp_dir("kinds");
        // The engine renders web pages through its own copy of Chromium.
        let web = parse_project(
            &dir,
            r#"{"title":"w","type":"web","file":"index.html","workshopid":7}"#,
        )
        .unwrap();
        assert_eq!(web.kind, "web");
        assert!(web.supported);
        let application = parse_project(
            &dir,
            r#"{"title":"a","type":"application","file":"a.pkg","workshopid":9}"#,
        )
        .unwrap();
        assert_eq!(application.kind, "application");
        assert!(!application.supported);
        let video = parse_project(
            &dir,
            r#"{"title":"v","type":"VIDEO","file":"a.mp4","workshopid":8}"#,
        )
        .unwrap();
        assert_eq!(video.kind, "video");
        assert!(video.supported);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn markers_round_trip() {
        assert_eq!(marker_id("we:1845706469"), Some("1845706469"));
        assert_eq!(marker_id("/home/user/a.mp4"), None);
        assert_eq!(marker_id("we:"), Some(""));
    }

    #[test]
    fn keeps_kinds_without_a_file() {
        // A web wallpaper that only points at a dependency has nothing the
        // engine could load, so it stays listed but unusable.
        let dir = temp_dir("web-no-file");
        let folder = dir.join("3121652946");
        fs::create_dir_all(&folder).unwrap();
        let item = parse_project(
            &folder,
            r#"{"title":"Clock","type":"web","dependency":"1"}"#,
        )
        .unwrap();
        assert_eq!(item.kind, "web");
        assert!(!item.supported);
        assert!(item.file.is_empty());
        assert_eq!(item.id, "3121652946");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_library_paths_from_vdf() {
        let text = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"/home/derek/.local/share/Steam"
	}
	"1"
	{
		"path"		"/run/media/derek/games/SteamLibrary"
	}
}"#;
        assert_eq!(
            vdf_paths(text),
            vec![
                "/home/derek/.local/share/Steam".to_string(),
                "/run/media/derek/games/SteamLibrary".to_string()
            ]
        );
        assert!(vdf_paths("").is_empty());
        assert!(vdf_paths("\"path\"\n\"novalue\"").is_empty());
    }

    #[test]
    fn builds_one_assignment_per_screen() {
        let targets = vec![
            Target {
                screen: "DP-1".into(),
                id: "111".into(),
            },
            Target {
                screen: "HDMI-A-1".into(),
                id: "222".into(),
            },
        ];
        let properties = BTreeMap::new();
        let args = arguments(&targets, None, "fill", &properties);
        assert_eq!(
            args,
            vec![
                "--scaling",
                "fill",
                "--screen-root",
                "DP-1",
                "--bg",
                "111",
                "--screen-root",
                "HDMI-A-1",
                "--bg",
                "222"
            ]
        );
        let with_assets = arguments(
            &targets[..1],
            Some(Path::new("/opt/assets")),
            "fit",
            &properties,
        );
        assert_eq!(with_assets[0], "--assets-dir");
        assert_eq!(with_assets[1], "/opt/assets");
        assert_eq!(with_assets[2], "--scaling");
        assert_eq!(with_assets[3], "fit");
    }

    #[test]
    fn passes_scene_settings_as_overrides() {
        let mut properties = BTreeMap::new();
        properties.insert("clock".to_string(), "0".to_string());
        properties.insert("audiobarscolor".to_string(), "255,0,128,255".to_string());
        let targets = vec![Target {
            screen: "DP-1".into(),
            id: "111".into(),
        }];
        let args = arguments(&targets, None, "fill", &properties);
        assert_eq!(
            args,
            vec![
                "--scaling",
                "fill",
                "--set-property",
                "audiobarscolor=255,0,128,255",
                "--set-property",
                "clock=0",
                "--screen-root",
                "DP-1",
                "--bg",
                "111"
            ]
        );
    }

    const PROPERTIES: &str = r#"Running with: ./linux-wallpaperengine --assets-dir /assets
Using wallpaper engine's assets at "/assets" based on --assets-dir parameter
audiobars - combo
	Text: Audio bars:
	Value: 3
Values: 
		0 = Off
		1 = Left
		3 = Bottom

audiobarscolor - color
	Text: Audio bars color:
	Value: 0.752941, 0.752941, 0.752941, 1.000000

clock - boolean
	Text: Clock
	Value: 1

hairanimationspeed - slider
	Text: Hair Animation Speed
	Min: 0.01
	Max: 4
	Step: 0.01
	Value: 2.500000

schemecolor - color
	Text: ui_browse_properties_scheme_color
	Value: 0.000000, 0.000000, 0.000000, 1.000000
"#;

    #[test]
    fn reads_every_kind_of_scene_setting() {
        let properties = parse_properties(PROPERTIES);
        let keys: Vec<&str> = properties.iter().map(|item| item.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "audiobars",
                "audiobarscolor",
                "clock",
                "hairanimationspeed",
                "schemecolor"
            ]
        );

        let combo = &properties[0];
        assert_eq!(combo.kind, PropertyKind::Combo);
        assert_eq!(combo.title(), "Audio bars:");
        assert_eq!(combo.value, "3");
        assert_eq!(combo.options.len(), 3);
        assert_eq!(combo.options[2].0, "3");
        assert_eq!(combo.options[2].1, "Bottom");

        let color = &properties[1];
        assert_eq!(color.kind, PropertyKind::Color);
        assert_eq!(color.value, "192,192,192,255");

        let boolean = &properties[2];
        assert_eq!(boolean.kind, PropertyKind::Boolean);
        assert_eq!(boolean.value, "1");

        let slider = &properties[3];
        assert_eq!(slider.kind, PropertyKind::Slider);
        assert_eq!(slider.value, "2.50");
        assert_eq!(slider.default, "2.50");
        assert_eq!((slider.min, slider.max, slider.step), (0.01, 4.0, 0.01));
        assert_eq!(number_value(2.5, slider.step), "2.50");

        // Translation keys are useless in a window; the property name is not.
        assert_eq!(properties[4].title(), "schemecolor");
    }

    #[test]
    fn tolerates_empty_and_junk_output() {
        assert!(parse_properties("").is_empty());
        assert!(parse_properties("Running with: something\nUsing assets\n").is_empty());
    }

    #[test]
    fn converts_colours_both_ways() {
        assert_eq!(color_components("255,0,128,255"), [255, 0, 128, 255]);
        assert_eq!(color_components(" 12, 34,56 , 78 "), [12, 34, 56, 78]);
        assert_eq!(color_components("bad"), [255, 255, 255, 255]);
        assert_eq!(color_value([1, 2, 3, 4]), "1,2,3,4");
        assert_eq!(bool_value(false), "0");
        assert_eq!(bool_value(true), "1");
        assert_eq!(number_value(0.5, 1.0), "0");
        assert_eq!(number_value(0.5294, 0.001), "0.529");
    }

    #[test]
    fn finds_libcef_next_to_the_engine() {
        let dir = temp_dir("libcef");
        let binary = dir.join("linux-wallpaperengine");
        fs::write(&binary, b"").unwrap();
        let cef = dir.join("libcef.so");
        fs::write(&cef, b"").unwrap();
        assert_eq!(libcef(&binary, ""), Some(cef));
        // An explicit "off" means the build does not need the workaround.
        assert_eq!(libcef(&binary, "none"), None);
        assert_eq!(libcef(&binary, "off"), None);
        let elsewhere = temp_dir("libcef-explicit");
        let path = elsewhere.join("my-libcef.so");
        fs::write(&path, b"").unwrap();
        assert_eq!(libcef(&binary, &path.display().to_string()), Some(path));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn normalises_what_a_person_types() {
        let properties = parse_properties(PROPERTIES);

        let clock = properties.iter().find(|item| item.key == "clock").unwrap();
        assert_eq!(clock.normalize("off").unwrap(), "0");
        assert_eq!(clock.normalize("TRUE").unwrap(), "1");
        assert!(clock.normalize("maybe").is_err());

        let bars = properties
            .iter()
            .find(|item| item.key == "audiobars")
            .unwrap();
        assert_eq!(bars.normalize("Bottom").unwrap(), "3");
        assert_eq!(bars.normalize("0").unwrap(), "0");
        assert!(bars.normalize("9").is_err());

        let colour = properties
            .iter()
            .find(|item| item.key == "audiobarscolor")
            .unwrap();
        assert_eq!(
            colour.normalize("255, 128, 0, 255").unwrap(),
            "255,128,0,255"
        );
        assert_eq!(colour.normalize("#f80").unwrap(), "255,136,0,255");
        assert_eq!(colour.normalize("#ff8800").unwrap(), "255,136,0,255");
        assert_eq!(colour.normalize("#ff880080").unwrap(), "255,136,0,128");
        assert!(colour.normalize("255,0,0").is_err());
        assert!(colour.normalize("#zzz").is_err());

        let speed = properties
            .iter()
            .find(|item| item.key == "hairanimationspeed")
            .unwrap();
        assert_eq!(speed.normalize("2.5").unwrap(), "2.50");
        // Out of range values are clamped instead of rejected.
        assert_eq!(speed.normalize("40").unwrap(), "4.00");
        assert!(speed.normalize("fast").is_err());
    }
}
