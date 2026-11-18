//! `org.veil.Veil` on the session bus.
//!
//! The service runs in its own thread and talks to the daemon through the
//! same Unix-socket protocol as `veilctl` and the GUI. That keeps every
//! mutation on the Wayland thread and means the D-Bus side cannot corrupt the
//! renderer state.

use veil::ipc::{self, Request};
use zbus::blocking::Connection;
use zbus::interface;

/// The GUI owns `org.veil.Veil` as its GApplication id, so the daemon uses a
/// distinct well-known name.
pub const BUS_NAME: &str = "org.veil.Veild";
pub const OBJECT_PATH: &str = "/org/veil/Veil";

struct VeilService;

fn reply(request: Request) -> String {
    match ipc::call(&request) {
        Ok(value) => value.to_string(),
        Err(error) => serde_json::json!({ "ok": false, "error": error }).to_string(),
    }
}

#[interface(name = "org.veil.Veil")]
impl VeilService {
    /// Pause playback and keep it paused until Resume is called.
    fn pause(&self) -> String {
        reply(Request::Pause)
    }

    fn resume(&self) -> String {
        reply(Request::Resume)
    }

    /// Toggle the manual pause state.
    fn toggle(&self) -> String {
        reply(Request::Toggle)
    }

    /// Advance the playlist on every monitor.
    fn next(&self) -> String {
        reply(Request::Next { output: None })
    }

    /// Pick a random wallpaper from the playlist.
    fn random(&self) -> String {
        reply(Request::Random { output: None })
    }

    /// The full daemon status document, as JSON.
    fn status(&self) -> String {
        reply(Request::Status)
    }

    #[zbus(property)]
    fn playback_status(&self) -> String {
        let paused = ipc::call(&Request::Status)
            .ok()
            .and_then(|value| value["paused"].as_bool())
            .unwrap_or(false);
        if paused { "Paused" } else { "Playing" }.to_string()
    }

    #[zbus(property)]
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }
}

/// Start the session-bus service. A missing bus is not fatal: the daemon and
/// the CLI keep working without it.
pub fn spawn() {
    let spawned = std::thread::Builder::new()
        .name("veil-dbus".into())
        .spawn(|| {
            if let Err(error) = serve() {
                log::warn!("D-Bus service unavailable: {error}");
            }
        });
    if let Err(error) = spawned {
        log::warn!("cannot start the D-Bus thread: {error}");
    }
}

fn serve() -> zbus::Result<()> {
    let connection = Connection::session()?;
    connection.request_name(BUS_NAME)?;
    connection.object_server().at(OBJECT_PATH, VeilService)?;
    log::info!("D-Bus: {BUS_NAME} on {OBJECT_PATH}");
    loop {
        std::thread::park();
    }
}
