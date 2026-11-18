//! Keeps `linux-wallpaperengine` running for the monitors that need it.
//!
//! The engine is a separate process by design: Wallpaper Engine scenes need
//! their own shader runtime, and veil only hands over the screens that show
//! one. Every other screen stays with veil's own mpv player.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use veil::config::Config;
use veil::engine::{self, Target};

/// Seconds to wait before respawning an engine that died on its own.
const RETRY: Duration = Duration::from_secs(10);

pub struct Host {
    child: Option<Child>,
    targets: Vec<Target>,
    /// The command line the running child was started with; a change of scene
    /// settings shows up here and means the child must be restarted.
    args: Vec<String>,
    failed: Option<Instant>,
    missing_logged: bool,
}

impl Host {
    pub fn new() -> Self {
        Self {
            child: None,
            targets: Vec::new(),
            args: Vec::new(),
            failed: None,
            missing_logged: false,
        }
    }

    /// Start, restart or stop the engine so that it matches `targets`.
    pub fn sync(&mut self, cfg: &Config, targets: Vec<Target>) {
        if targets.is_empty() {
            self.stop();
            self.missing_logged = false;
            return;
        }

        let assets = if cfg.engine.assets.trim().is_empty() {
            engine::assets_dir()
        } else {
            Some(PathBuf::from(veil::config::expand(&cfg.engine.assets)))
        };
        let args = engine::arguments(
            &targets,
            assets.as_deref(),
            &cfg.engine.scaling,
            &cfg.engine.properties,
        );

        if self.targets == targets && self.args == args {
            if let Some(child) = self.child.as_mut() {
                match child.try_wait() {
                    Ok(None) => return,
                    Ok(Some(status)) => {
                        log::warn!("engine exited ({status}), will retry");
                        self.child = None;
                        self.failed = Some(Instant::now());
                    }
                    Err(error) => {
                        log::warn!("engine state is unknown: {error}");
                        self.child = None;
                        self.failed = Some(Instant::now());
                    }
                }
            }
            if let Some(failed) = self.failed {
                if failed.elapsed() < RETRY {
                    return;
                }
            }
        }

        self.stop();
        let Some(binary) = engine::binary(&cfg.engine.binary) else {
            if !self.missing_logged {
                log::warn!("no linux-wallpaperengine found; install it or set `[engine] binary`");
                self.missing_logged = true;
            }
            self.targets = targets;
            self.args = args;
            self.failed = Some(Instant::now());
            return;
        };

        if assets.is_none() {
            log::warn!(
                "Wallpaper Engine assets not found; scenes may fail until it is installed in Steam"
            );
        }
        log::info!("engine: {} {}", binary.display(), args.join(" "));

        match Command::new(&binary)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // The engine explains its failures on stderr; keep them in our log.
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(child) => {
                log::info!("engine started, pid {}", child.id());
                self.child = Some(child);
                self.targets = targets;
                self.args = args;
                self.failed = None;
                self.missing_logged = false;
            }
            Err(error) => {
                log::warn!("cannot start {}: {error}", binary.display());
                self.targets = targets;
                self.args = args;
                self.failed = Some(Instant::now());
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            log::info!("engine stopped");
        }
        self.targets.clear();
        self.args.clear();
    }

    /// Whether a child process is (still) running.
    pub fn running(&mut self) -> Option<u32> {
        let child = self.child.as_mut()?;
        match child.try_wait() {
            Ok(None) => Some(child.id()),
            _ => {
                self.child = None;
                self.failed = Some(Instant::now());
                None
            }
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop();
    }
}
