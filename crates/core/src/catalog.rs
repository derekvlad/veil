//! The wallpaper catalogue ("workshop"): a JSON manifest plus downloads.
//!
//! The catalogue itself is a plain file served over HTTPS (a repository, a
//! static host, anything). Each entry points at one media file and, ideally, at
//! its SHA-256. Downloads are size-capped, written to a `.part` file and moved
//! into place only after the hash matches — a broken or tampered download never
//! ends up in the library.
//!
//! No archives are unpacked here on purpose: the first version only accepts
//! media files, so there is no zip-slip surface at all.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::is_media_file;

/// Hard ceiling for one wallpaper, regardless of what the manifest says.
pub const MAX_MEDIA_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Ceiling for the manifest itself.
pub const MAX_CATALOG_BYTES: u64 = 4 * 1024 * 1024;
/// Ceiling for a preview image.
pub const MAX_PREVIEW_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Catalog {
    /// Format version, currently 1.
    pub version: u32,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Entry {
    /// Stable identifier, also used as the file stem.
    pub id: String,
    pub title: String,
    pub author: String,
    pub description: String,
    /// `video`, `image` or empty when unknown.
    pub kind: String,
    pub tags: Vec<String>,
    /// Optional preview image URL.
    pub preview: String,
    /// Direct link to the media file.
    pub url: String,
    /// Lower-case hex SHA-256 of the file.
    pub sha256: String,
    /// Expected size in bytes, used for progress and sanity checks.
    pub size: u64,
    pub license: String,
}

impl Catalog {
    pub fn parse(text: &str) -> Result<Self, String> {
        let catalog: Catalog =
            serde_json::from_str(text).map_err(|error| format!("catalog: {error}"))?;
        if catalog.entries.is_empty() {
            return Err("catalog: no entries".into());
        }
        for entry in &catalog.entries {
            entry.validate()?;
        }
        Ok(catalog)
    }

    pub fn find(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// Case-insensitive match on title, author and tags.
    pub fn search(&self, query: &str) -> Vec<&Entry> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return self.entries.iter().collect();
        }
        self.entries
            .iter()
            .filter(|entry| {
                entry.title.to_lowercase().contains(&query)
                    || entry.author.to_lowercase().contains(&query)
                    || entry
                        .tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&query))
            })
            .collect()
    }

    /// Reject entries the downloader cannot handle safely.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 0 && self.version != 1 {
            return Err(format!("catalog: unsupported version {}", self.version));
        }
        for entry in &self.entries {
            entry.validate()?;
        }
        Ok(())
    }
}

impl Entry {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err(format!(
                "catalog: bad id `{}` (use a-z, 0-9, `-` and `_`)",
                self.id
            ));
        }
        if !(self.url.starts_with("https://") || self.url.starts_with("http://")) {
            return Err(format!("catalog: {}: url must be http(s)", self.id));
        }
        if !self.preview.is_empty()
            && !(self.preview.starts_with("https://") || self.preview.starts_with("http://"))
        {
            return Err(format!("catalog: {}: preview must be http(s)", self.id));
        }
        if !self.sha256.is_empty() && !valid_sha256(&self.sha256) {
            return Err(format!("catalog: {}: sha256 is not 64 hex chars", self.id));
        }
        if self.sha256.is_empty() {
            return Err(format!(
                "catalog: {}: sha256 is required for catalogue entries",
                self.id
            ));
        }
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

fn valid_sha256(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Last path segment of a URL, without query or fragment.
pub fn file_name_from_url(url: &str) -> Result<String, String> {
    let path = url
        .split('#')
        .next()
        .unwrap_or(url)
        .split('?')
        .next()
        .unwrap_or(url);
    let segment = path.rsplit('/').next().unwrap_or("");
    let segment = percent_decode(segment);
    if segment.is_empty() {
        return Err("url has no file name".into());
    }
    // Never let a URL pick a path outside the library.
    let segment = segment
        .replace('/', "_")
        .replace('\\', "_")
        .trim_start_matches('.')
        .to_string();
    if segment.is_empty() {
        return Err("url has no usable file name".into());
    }
    Ok(segment)
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &value[index + 1..index + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .user_agent(concat!("veil/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Fetch a small text document (the catalogue).
pub fn fetch_text(url: &str) -> Result<String, String> {
    let response = agent()
        .get(url)
        .call()
        .map_err(|error| format!("cannot fetch {url}: {error}"))?;
    let mut reader = response.into_reader().take(MAX_CATALOG_BYTES + 1);
    let mut buffer = String::new();
    reader
        .read_to_string(&mut buffer)
        .map_err(|error| format!("cannot read {url}: {error}"))?;
    if buffer.len() as u64 > MAX_CATALOG_BYTES {
        return Err(format!("{url}: catalogue is too large"));
    }
    Ok(buffer)
}

/// Fetch a small binary document (a preview image).
pub fn fetch_bytes(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let response = agent()
        .get(url)
        .call()
        .map_err(|error| format!("cannot fetch {url}: {error}"))?;
    let mut reader = response.into_reader().take(limit + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {url}: {error}"))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{url}: response is too large"));
    }
    Ok(bytes)
}

/// Hex SHA-256 of a file, streamed.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Download a media file into `dir`, optionally checking its SHA-256.
///
/// Returns the path of the finished file. If a file with the same name and the
/// same hash already exists, it is reused instead of downloaded again.
pub fn install(url: &str, sha256: Option<&str>, dir: &Path) -> Result<PathBuf, String> {
    let name = file_name_from_url(url)?;
    if !is_media_file(Path::new(&name)) {
        return Err(format!(
            "{name}: only video and image files are supported (no archives yet)"
        ));
    }
    fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;

    let mut target = dir.join(&name);
    if target.exists() {
        let same = match sha256 {
            Some(expected) => sha256_file(&target)?.eq_ignore_ascii_case(expected),
            None => false,
        };
        if same {
            return Ok(target);
        }
        target = unique_path(dir, &name);
    }

    let part = dir.join(format!(
        ".{}.{}.part",
        std::process::id(),
        name.trim_start_matches('.')
    ));
    let result = download_to(url, &part, sha256);
    match result {
        Ok(()) => {
            fs::rename(&part, &target).map_err(|error| format!("{}: {error}", target.display()))?;
            Ok(target)
        }
        Err(error) => {
            let _ = fs::remove_file(&part);
            Err(error)
        }
    }
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "wallpaper".into());
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned())
        .unwrap_or_default();
    for counter in 1..1000 {
        let candidate = if extension.is_empty() {
            dir.join(format!("{stem}-{counter}"))
        } else {
            dir.join(format!("{stem}-{counter}.{extension}"))
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(name)
}

fn download_to(url: &str, part: &Path, expected: Option<&str>) -> Result<(), String> {
    let response = agent()
        .get(url)
        .call()
        .map_err(|error| format!("cannot download {url}: {error}"))?;
    let total = response
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok());
    if let Some(total) = total {
        if total > MAX_MEDIA_BYTES {
            return Err(format!("{url}: file is larger than 8 GiB"));
        }
    }

    let mut reader = response.into_reader().take(MAX_MEDIA_BYTES + 1);
    let mut writer = File::create(part).map_err(|error| format!("{}: {error}", part.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut written = 0u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("cannot read {url}: {error}"))?;
        if read == 0 {
            break;
        }
        written += read as u64;
        if written > MAX_MEDIA_BYTES {
            return Err(format!("{url}: file is larger than 8 GiB"));
        }
        hasher.update(&buffer[..read]);
        writer
            .write_all(&buffer[..read])
            .map_err(|error| format!("{}: {error}", part.display()))?;
    }
    writer
        .flush()
        .map_err(|error| format!("{}: {error}", part.display()))?;
    drop(writer);

    if written == 0 {
        return Err(format!("{url}: empty response"));
    }
    if let Some(total) = total {
        if total != written {
            return Err(format!("{url}: expected {total} bytes, got {written}"));
        }
    }
    if let Some(expected) = expected {
        let actual = format!("{:x}", hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(format!(
                "{url}: sha256 mismatch (expected {expected}, got {actual})"
            ));
        }
    }
    Ok(())
}

/// Resolve `id-or-url` against a catalogue and install it.
pub fn install_reference(
    reference: &str,
    catalog_url: &str,
    dir: &Path,
) -> Result<PathBuf, String> {
    if reference.starts_with("https://") || reference.starts_with("http://") {
        return install(reference, None, dir);
    }
    if catalog_url.is_empty() {
        return Err("no catalogue URL configured; run `veilctl catalog --url <url>`".into());
    }
    let text = fetch_text(catalog_url)?;
    let catalog = Catalog::parse(&text)?;
    let entry = catalog
        .find(reference)
        .ok_or_else(|| format!("no `{reference}` in {catalog_url}"))?;
    install(&entry.url, Some(&entry.sha256), dir)
}

/// Copy a downloaded preview into the cache and return its path.
pub fn cached_preview(url: &str, cache: &Path, id: &str) -> Result<PathBuf, String> {
    let path = cache.join(format!("catalog-{id}.img"));
    if path.exists() {
        return Ok(path);
    }
    let bytes = fetch_bytes(url, MAX_PREVIEW_BYTES)?;
    fs::create_dir_all(cache).map_err(|error| format!("{}: {error}", cache.display()))?;
    let part = cache.join(format!(".{}-{}.part", std::process::id(), id));
    fs::write(&part, &bytes).map_err(|error| format!("{}: {error}", part.display()))?;
    fs::rename(&part, &path).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "version": 1,
      "entries": [
        {
          "id": "neon-city",
          "title": "Neon City",
          "author": "derek",
          "kind": "video",
          "tags": ["cyberpunk", "4k"],
          "url": "https://example.com/neon%20city.mp4",
          "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
          "size": 1234,
          "license": "CC0"
        }
      ]
    }"#;

    #[test]
    fn the_shipped_example_is_valid() {
        let text = include_str!("../../../docs/catalog.example.json");
        let catalog = Catalog::parse(text).unwrap();
        assert_eq!(catalog.entries.len(), 2);
        assert!(catalog.find("neon-city").is_some());
        assert_eq!(catalog.search("NATURE").len(), 1);
    }

    #[test]
    fn parses_and_finds_entries() {
        let catalog = Catalog::parse(SAMPLE).unwrap();
        assert_eq!(catalog.entries.len(), 1);
        assert!(catalog.find("neon-city").is_some());
        assert!(catalog.find("missing").is_none());
        assert_eq!(catalog.search("CYBER").len(), 1);
        assert_eq!(catalog.search("derek").len(), 1);
        assert!(catalog.search("nothing").is_empty());
    }

    #[test]
    fn rejects_unsafe_entries() {
        let mut catalog = Catalog::parse(SAMPLE).unwrap();
        catalog.entries[0].url = "file:///etc/passwd".into();
        assert!(catalog.validate().is_err());

        let mut catalog = Catalog::parse(SAMPLE).unwrap();
        catalog.entries[0].sha256.clear();
        assert!(Catalog::parse(&serde_json::to_string(&catalog).unwrap()).is_err());

        let mut catalog = Catalog::parse(SAMPLE).unwrap();
        catalog.entries[0].id = "../escape".into();
        assert!(catalog.validate().is_err());
    }

    #[test]
    fn file_names_never_escape_the_directory() {
        assert_eq!(
            file_name_from_url("https://example.com/a/neon%20city.mp4?x=1").unwrap(),
            "neon city.mp4"
        );
        assert_eq!(
            file_name_from_url("https://example.com/../../etc/passwd").unwrap(),
            "passwd"
        );
        assert!(file_name_from_url("https://example.com/").is_err());
    }

    #[test]
    fn sha256_matches_the_reference_vectors() {
        let dir = std::env::temp_dir().join(format!("veil-hash-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abc.bin");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_rejects_archives_and_unknown_extensions() {
        let dir = std::env::temp_dir().join(format!("veil-install-{}", std::process::id()));
        let error = install("https://example.com/a.zip", None, &dir).unwrap_err();
        assert!(error.contains("only video and image files"));
        let error = install("https://example.com/a.txt", None, &dir).unwrap_err();
        assert!(error.contains("only video and image files"));
    }
}
