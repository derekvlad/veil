//! Library scanning, ffprobe metadata and cached thumbnails.
//!
//! Everything here is blocking; the GUI calls it from worker threads so that
//! a slow disk or a 4K file never freezes the window.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

const VIDEO_EXT: &[&str] = &["mp4", "webm", "mkv", "mov", "m4v", "avi", "ogv"];
const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "webp", "avif", "gif", "bmp"];

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub video: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    pub thumb: Option<PathBuf>,
    pub video: bool,
}

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let ext = ext.to_ascii_lowercase();
            VIDEO_EXT.contains(&ext.as_str()) || IMAGE_EXT.contains(&ext.as_str())
        })
        .unwrap_or(false)
}

pub fn scan(dir: &Path) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = read
        .flatten()
        .map(|item| item.path())
        .filter(|path| path.is_file() && is_supported(path))
        .map(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let video = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| VIDEO_EXT.contains(&ext.to_ascii_lowercase().as_str()))
                .unwrap_or(false);
            Entry { path, name, video }
        })
        .collect();
    entries.sort_by_key(|entry| entry.name.to_lowercase());
    entries
}

/// Cheap cache key: the file's identity, size and modification time.
fn cache_key(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    if let Ok(meta) = std::fs::metadata(path) {
        meta.len().hash(&mut hasher);
        if let Ok(modified) = meta.modified() {
            if let Ok(duration) = modified.duration_since(UNIX_EPOCH) {
                duration.as_secs().hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

pub fn cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from) {
        return dir.join("veil").join("thumbs");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("veil").join("thumbs")
}

/// Probe the file and, for videos, produce a cached JPEG poster frame.
pub fn probe(path: &Path, video: bool) -> Probe {
    let mut probe = Probe::default();

    if let Ok(output) = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height:format=duration",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
    {
        if output.status.success() {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                if let Some(stream) = value["streams"].get(0) {
                    probe.width = stream["width"].as_u64().unwrap_or(0) as u32;
                    probe.height = stream["height"].as_u64().unwrap_or(0) as u32;
                }
                if let Some(duration) = value["format"]["duration"].as_str() {
                    probe.duration = duration.parse().unwrap_or(0.0);
                }
            }
        }
    }

    probe.video = video;
    // A poster for everything, not only videos: it keeps the grid fast with
    // 8K stills and hides the fact that GtkPicture cannot animate a GIF.
    probe.thumb = poster(path);
    probe
}

fn poster(path: &Path) -> Option<PathBuf> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let target = dir.join(format!("{:016x}.jpg", cache_key(path)));
    if target.is_file() {
        return Some(target);
    }

    // Take a frame a bit into the clip so that intros are skipped, and fall
    // back to the very first frame for very short clips.
    for offset in ["1.5", "0"] {
        let status = Command::new("ffmpeg")
            .args(["-nostdin", "-y", "-v", "error", "-ss", offset, "-i"])
            .arg(path)
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale=240:135:force_original_aspect_ratio=increase,crop=240:135",
                "-q:v",
                "4",
            ])
            .arg(&target)
            .status();
        if matches!(status, Ok(status) if status.success()) && target.is_file() {
            return Some(target);
        }
    }
    let _ = std::fs::remove_file(&target);
    None
}

/// Copy a file into the library folder without overwriting anything.
pub fn import(source: &Path, library: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(library).map_err(|error| format!("{}: {error}", library.display()))?;
    let name = source
        .file_name()
        .ok_or_else(|| format!("{}: not a file", source.display()))?;
    let mut target = library.join(name);
    if target.exists() {
        let stem = source
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "wallpaper".into());
        let ext = source
            .extension()
            .map(|ext| format!(".{}", ext.to_string_lossy()))
            .unwrap_or_default();
        let mut index = 1;
        loop {
            target = library.join(format!("{stem}-{index}{ext}"));
            if !target.exists() {
                break;
            }
            index += 1;
        }
    }
    std::fs::copy(source, &target).map_err(|error| format!("{}: {error}", source.display()))?;
    Ok(target)
}

pub fn human_size(width: u32, height: u32) -> String {
    if width == 0 || height == 0 {
        return String::new();
    }
    // 3840x2160 and friends are much easier to read as "4K".
    let label = match width.max(height) {
        w if w >= 3800 => "4K",
        w if w >= 2500 => "1440p",
        w if w >= 1900 => "1080p",
        w if w >= 1200 => "720p",
        _ => "",
    };
    if label.is_empty() {
        format!("{width}×{height}")
    } else {
        format!("{label} · {width}×{height}")
    }
}
