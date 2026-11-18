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
    // Web and application wallpapers keep their code inside a `.pkg` (or rely on
    // a dependency's), so there is often no `file` at all — and no engine that
    // could run them either. Only the kinds we hand over need a main asset.
    let supported = matches!(kind.as_str(), "video" | "scene");
    if supported && project.file.is_empty() {
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
pub fn arguments(targets: &[Target], assets: Option<&Path>, scaling: &str) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(assets) = assets {
        args.push("--assets-dir".into());
        args.push(assets.display().to_string());
    }
    args.push("--scaling".into());
    args.push(scaling.to_string());
    for target in targets {
        args.push("--screen-root".into());
        args.push(target.screen.clone());
        args.push("--bg".into());
        args.push(target.id.clone());
    }
    args
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
        let web = parse_project(
            &dir,
            r#"{"title":"w","type":"web","file":"index.html","workshopid":7}"#,
        )
        .unwrap();
        assert_eq!(web.kind, "web");
        assert!(!web.supported);
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
    fn keeps_unsupported_kinds_without_a_file() {
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
        let args = arguments(&targets, None, "fill");
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
        let with_assets = arguments(&targets[..1], Some(Path::new("/opt/assets")), "fit");
        assert_eq!(with_assets[0], "--assets-dir");
        assert_eq!(with_assets[1], "/opt/assets");
        assert_eq!(with_assets[2], "--scaling");
        assert_eq!(with_assets[3], "fit");
    }
}
