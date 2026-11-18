//! veil — hardware-decoded video wallpapers for Wayland.
//!
//! The daemon (`veild`) owns a background layer surface per monitor and asks
//! libmpv to render frames straight into an EGL window. The control client
//! (`veilctl`) talks to it over a Unix socket.

pub mod config;
pub mod egl;
pub mod ipc;
pub mod mpv;
pub mod span;
pub mod wayland;

use std::os::raw::c_void;

/// Wake the daemon's poll loop from another thread or a signal handler.
pub fn wake(fd: i32) {
    if fd < 0 {
        return;
    }
    let one: u64 = 1;
    unsafe {
        libc::write(fd, &one as *const u64 as *const c_void, 8);
    }
}

/// True when we are running on battery power.
pub fn on_battery() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = std::fs::read_to_string(path.join("type")) else {
            continue;
        };
        if kind.trim() != "Battery" {
            continue;
        }
        let Ok(status) = std::fs::read_to_string(path.join("status")) else {
            continue;
        };
        if status.trim() == "Discharging" {
            return true;
        }
    }
    false
}
