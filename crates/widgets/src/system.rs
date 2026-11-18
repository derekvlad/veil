//! Small probes for the system widgets: CPU load, memory, battery, MPRIS.
//!
//! Everything here reads `/proc`, `/sys` or asks `playerctl`, so the widget
//! process needs no extra libraries and no D-Bus bindings of its own.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// CPU load, sampled between consecutive reads of `/proc/stat`.
#[derive(Default)]
pub struct Cpu {
    last: Option<(u64, u64)>,
}

impl Cpu {
    /// Busy percentage since the previous call; 0 on the first sample.
    pub fn percent(&mut self) -> f64 {
        let Some((total, idle)) = read_cpu() else {
            return 0.0;
        };
        let percent = match self.last {
            Some((last_total, last_idle)) if total > last_total => {
                let busy = (total - last_total).saturating_sub(idle - last_idle);
                busy as f64 * 100.0 / (total - last_total) as f64
            }
            _ => 0.0,
        };
        self.last = Some((total, idle));
        percent
    }
}

/// Total and idle jiffies from the aggregate `cpu` line.
fn read_cpu() -> Option<(u64, u64)> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    let line = stat.lines().next()?;
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let values: Vec<u64> = fields.filter_map(|field| field.parse().ok()).collect();
    if values.len() < 4 {
        return None;
    }
    // user, nice, system, idle, iowait, irq, softirq, steal
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    let total: u64 = values.iter().take(8).sum();
    Some((total, idle))
}

/// Used and total memory in gibibytes.
pub fn memory() -> (f64, f64) {
    let Ok(text) = fs::read_to_string("/proc/meminfo") else {
        return (0.0, 0.0);
    };
    let mut total = 0.0;
    let mut available = 0.0;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("MemTotal:") => total = kib(parts.next()),
            Some("MemAvailable:") => available = kib(parts.next()),
            _ => {}
        }
    }
    let used = (total - available).max(0.0);
    (used, total)
}

fn kib(value: Option<&str>) -> f64 {
    value
        .and_then(|text| text.parse::<f64>().ok())
        .map(|kib| kib / 1_048_576.0)
        .unwrap_or(0.0)
}

/// The first battery the kernel exposes.
pub struct Battery {
    dir: PathBuf,
}

impl Battery {
    pub fn discover() -> Option<Battery> {
        let entries = fs::read_dir("/sys/class/power_supply").ok()?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("BAT") || name.starts_with("bat") {
                return Some(Battery { dir: entry.path() });
            }
        }
        None
    }

    /// Charge percentage, charging, and `full`.
    pub fn read(&self) -> Option<(u8, bool, bool)> {
        let capacity: u8 = fs::read_to_string(self.dir.join("capacity"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        let status = fs::read_to_string(self.dir.join("status"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let charging = status.eq_ignore_ascii_case("charging");
        let full = status.eq_ignore_ascii_case("full");
        Some((capacity.min(100), charging, full))
    }
}

/// Artist and title from the first MPRIS player, if one is playing.
pub fn now_playing() -> Option<String> {
    let output = Command::new("playerctl")
        .args(["metadata", "--format", "{{artist}}\u{1f}{{title}}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (artist, title) = text.trim().split_once('\u{1f}')?;
    let (artist, title) = (artist.trim(), title.trim());
    let line = match (artist.is_empty(), title.is_empty()) {
        (true, true) => String::new(),
        (true, false) => title.to_string(),
        (false, true) => artist.to_string(),
        (false, false) => format!("{artist} — {title}"),
    };
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_aggregate_cpu_line() {
        // The helper reads /proc, so only the format assumptions are checked.
        if let Some((total, idle)) = read_cpu() {
            assert!(total > idle, "busy time must be part of the total");
            assert!(idle > 0);
        }
    }

    #[test]
    fn memory_is_plausible() {
        let (used, total) = memory();
        assert!(total > 0.0, "MemTotal must parse");
        assert!(used <= total + 0.01);
    }

    #[test]
    fn mpris_is_optional() {
        // Either a track or nothing; both are fine on a test machine.
        if let Some(line) = now_playing() {
            assert!(!line.contains('\u{1f}'));
        }
    }
}
