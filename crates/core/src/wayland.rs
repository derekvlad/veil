//! Wayland backend: one background layer surface per monitor, rendered by EGL.
//!
//! Works on compositors implementing `zwlr-layer-shell-v1`: niri, Hyprland,
//! sway and friends. Compositors that do not expose a background layer to
//! external clients (KDE Plasma, GNOME) need their own plugin — see `plugins/`.

use std::collections::BTreeMap;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use khronos_egl as egl;
use wayland_client::protocol::{
    wl_compositor::WlCompositor,
    wl_output::{self, WlOutput},
    wl_region::WlRegion,
    wl_registry::{self, WlRegistry},
    wl_seat::WlSeat,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_egl::WlEglSurface;
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::ExtIdleNotifierV1,
};
use wayland_protocols_wlr::{
    foreign_toplevel::v1::client::{
        zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
        zwlr_foreign_toplevel_manager_v1::{self as manager, ZwlrForeignToplevelManagerV1},
    },
    layer_shell::v1::client::{
        zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
        zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
    },
};

use crate::config::{Config, Mode};
use crate::egl::Egl;
use crate::mpv::Player;
use crate::span::{self, Rect, Transform};

/// Index of an output in [`App::outputs`], used as Wayland object data.
#[derive(Debug, Clone, Copy)]
pub struct Slot(pub usize);

/// A fade-in that is driven by the daemon's tick.
pub struct Fade {
    started: Instant,
    secs: f64,
}

pub struct Output {
    pub alive: bool,
    pub global: u32,
    pub name: String,
    pub wl: WlOutput,
    pub scale: i32,
    /// Position in the compositor's logical layout, from wl_output.geometry.
    pub position: (i32, i32),
    pub surface: Option<WlSurface>,
    pub layer: Option<ZwlrLayerSurfaceV1>,
    pub egl_window: Option<WlEglSurface>,
    pub egl_surface: Option<egl::Surface>,
    pub context: Option<egl::Context>,
    pub player: Option<Player>,
    pub dirty: Arc<AtomicBool>,
    pub frames: Arc<AtomicU64>,
    pub logical: (i32, i32),
    pub pixels: (i32, i32),
    pub configured: bool,
    /// File currently loaded in the player (playlist changes are not saved).
    pub current: String,
    pub fade: Option<Fade>,
    /// Span transform currently applied to the player, if any.
    pub span_applied: Option<Transform>,
}

impl Output {
    fn new(global: u32, name: String, wl: WlOutput) -> Self {
        Self {
            alive: true,
            global,
            name,
            wl,
            scale: 1,
            position: (0, 0),
            surface: None,
            layer: None,
            egl_window: None,
            egl_surface: None,
            context: None,
            player: None,
            dirty: Arc::new(AtomicBool::new(false)),
            frames: Arc::new(AtomicU64::new(0)),
            logical: (0, 0),
            pixels: (0, 0),
            configured: false,
            current: String::new(),
            fade: None,
            span_applied: None,
        }
    }

    fn rect(&self) -> Rect {
        Rect::new(
            self.position.0,
            self.position.1,
            self.logical.0,
            self.logical.1,
        )
    }
}

pub struct Toplevel {
    pub handle: Option<ZwlrForeignToplevelHandleV1>,
    pub id: wayland_client::backend::ObjectId,
    pub alive: bool,
    pub fullscreen: bool,
    pub maximized: bool,
    pub title: String,
}

pub struct App {
    pub cfg: Config,
    pub cfg_path: std::path::PathBuf,
    pub egl: Egl,
    pub wl_display: *mut c_void,
    pub qh: QueueHandle<App>,
    pub registry: Option<WlRegistry>,
    pub compositor: Option<WlCompositor>,
    pub layer_shell: Option<ZwlrLayerShellV1>,
    pub seat: Option<WlSeat>,
    pub idle_notifier: Option<ExtIdleNotifierV1>,
    pub idle_notification: Option<ExtIdleNotificationV1>,
    pub idle_active: bool,
    pub idle_timeout: u64,
    pub toplevel_manager: Option<ZwlrForeignToplevelManagerV1>,
    pub outputs: Vec<Output>,
    pub toplevels: Vec<Toplevel>,
    pub pending_outputs: Vec<usize>,
    pub manual_paused: bool,
    pub on_battery: bool,
    pub paused: bool,
    pub exit: bool,
    pub wake_fd: i32,
    pub started: std::time::Instant,
    /// Cursor in the shared playlist (monitors without their own list).
    pub playlist_cursor: usize,
    /// Cursors for `[playlist.outputs]` lists.
    pub output_cursors: BTreeMap<String, usize>,
    pub last_advance: Instant,
    pub span_canvas: Option<Rect>,
    rng: u64,
}

impl App {
    pub fn new(
        cfg: Config,
        cfg_path: std::path::PathBuf,
        egl: Egl,
        wl_display: *mut c_void,
        qh: QueueHandle<App>,
        wake_fd: i32,
    ) -> Self {
        let idle_timeout = cfg.rules.idle_timeout_secs;
        Self {
            cfg,
            cfg_path,
            egl,
            wl_display,
            qh,
            registry: None,
            compositor: None,
            layer_shell: None,
            seat: None,
            idle_notifier: None,
            idle_notification: None,
            idle_active: false,
            idle_timeout,
            toplevel_manager: None,
            outputs: Vec::new(),
            toplevels: Vec::new(),
            pending_outputs: Vec::new(),
            manual_paused: false,
            on_battery: crate::on_battery(),
            paused: false,
            exit: false,
            wake_fd,
            started: std::time::Instant::now(),
            playlist_cursor: 0,
            output_cursors: BTreeMap::new(),
            last_advance: Instant::now(),
            span_canvas: None,
            rng: seed(),
        }
    }

    // ---------------------------------------------------------------- globals

    fn bind_global(&mut self, registry: &WlRegistry, name: u32, interface: &str, version: u32) {
        let qh = self.qh.clone();
        match interface {
            "wl_compositor" => {
                self.compositor = Some(registry.bind(name, version.min(6), &qh, ()));
            }
            "zwlr_layer_shell_v1" => {
                self.layer_shell = Some(registry.bind(name, version.min(5), &qh, ()));
            }
            "wl_seat" if self.seat.is_none() => {
                self.seat = Some(registry.bind(name, version.min(7), &qh, ()));
            }
            "ext_idle_notifier_v1" => {
                self.idle_notifier = Some(registry.bind(name, version.min(1), &qh, ()));
            }
            "zwlr_foreign_toplevel_manager_v1" => {
                self.toplevel_manager = Some(registry.bind(name, version.min(3), &qh, ()));
            }
            "wl_output" => {
                let slot = self.outputs.len();
                let wl: WlOutput = registry.bind(name, version.min(4), &qh, Slot(slot));
                self.outputs
                    .push(Output::new(name, format!("output-{slot}"), wl));
                self.pending_outputs.push(slot);
                log::debug!("new output, monitor slot {slot}");
            }
            _ => {}
        }
        self.maybe_create_idle();
    }

    fn remove_global(&mut self, name: u32) {
        if let Some(index) = self
            .outputs
            .iter()
            .position(|o| o.global == name && o.alive)
        {
            log::info!("output {} disconnected", self.outputs[index].name);
            self.teardown(index);
            self.outputs[index].alive = false;
        }
    }

    /// Process outputs discovered during the last dispatch round.
    pub fn process_pending(&mut self) {
        let pending = std::mem::take(&mut self.pending_outputs);
        for slot in pending {
            self.create_surface(slot);
        }
    }

    fn maybe_create_idle(&mut self) {
        let (Some(notifier), Some(seat)) = (self.idle_notifier.clone(), self.seat.clone()) else {
            return;
        };
        if !self.cfg.rules.pause_on_idle || self.idle_timeout == 0 {
            return;
        }
        if self.idle_notification.is_some() {
            return;
        }
        let timeout = (self.idle_timeout * 1000).min(u32::MAX as u64) as u32;
        let notification = notifier.get_idle_notification(timeout, &seat, &self.qh, ());
        self.idle_notification = Some(notification);
        log::debug!("idle notification armed after {}s", self.idle_timeout);
    }

    fn recreate_idle(&mut self) {
        if let Some(old) = self.idle_notification.take() {
            old.destroy();
        }
        self.idle_active = false;
        self.maybe_create_idle();
    }

    // --------------------------------------------------------------- surfaces

    fn create_surface(&mut self, slot: usize) {
        let (Some(compositor), Some(shell)) = (self.compositor.clone(), self.layer_shell.clone())
        else {
            log::error!("compositor lacks zwlr_layer_shell_v1, cannot draw wallpaper");
            return;
        };
        let Some(out) = self.outputs.get_mut(slot) else {
            return;
        };
        if out.surface.is_some() || !out.alive || !self.cfg.has_wallpaper() {
            return;
        }

        let surface = compositor.create_surface(&self.qh, Slot(slot));
        let layer = shell.get_layer_surface(
            &surface,
            Some(&out.wl),
            Layer::Background,
            "veil".to_string(),
            &self.qh,
            Slot(slot),
        );
        layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(0, 0);

        // Clicks belong to the icons, not to us.
        let region: WlRegion = compositor.create_region(&self.qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();

        out.surface = Some(surface.clone());
        out.layer = Some(layer);
        surface.commit();
        log::debug!("surface created for {}", out.name);
    }

    fn on_configure(&mut self, slot: usize, serial: u32, width: u32, height: u32) {
        let Some(out) = self.outputs.get_mut(slot) else {
            return;
        };
        if let Some(layer) = &out.layer {
            layer.ack_configure(serial);
        }

        let (mut lw, mut lh) = out.logical;
        if width > 0 && height > 0 {
            lw = width as i32;
            lh = height as i32;
        }
        if lw <= 0 || lh <= 0 {
            log::warn!("{}: empty configure", out.name);
            return;
        }

        let scale = out.scale.max(1);
        let pixels = (lw * scale, lh * scale);
        let size_changed = out.pixels != pixels;

        out.logical = (lw, lh);
        out.pixels = pixels;
        out.configured = true;

        if let Some(surface) = &out.surface {
            surface.set_buffer_scale(scale);
        }

        if let Some(window) = &out.egl_window {
            if size_changed {
                window.resize(pixels.0, pixels.1, 0, 0);
            }
        }

        self.ensure_stream(slot);
        self.paint(slot);
    }

    /// Create (or recreate after a pause) the EGL surface and player.
    fn ensure_stream(&mut self, slot: usize) {
        let egl = self.egl;
        let wl_display = self.wl_display;
        let playback = self.cfg.playback.clone();
        let mode = self.cfg.wallpaper.mode;

        let path = {
            let out = &self.outputs[slot];
            self.cfg.path_for(&out.name)
        };
        if path.is_empty() {
            return;
        }

        let (pw, ph) = {
            let out = &self.outputs[slot];
            if out.player.is_some() || out.pixels.0 <= 0 || out.pixels.1 <= 0 {
                return;
            }
            out.pixels
        };

        let out = &mut self.outputs[slot];
        let Some(surface) = out.surface.clone() else {
            return;
        };

        let window = match out.egl_window.take() {
            Some(window) => {
                window.resize(pw, ph, 0, 0);
                window
            }
            None => match WlEglSurface::new(surface.id(), pw, ph) {
                Ok(window) => window,
                Err(error) => {
                    log::error!("{}: cannot create EGL window: {error:?}", out.name);
                    return;
                }
            },
        };
        let egl_surface = match out.egl_surface {
            Some(surface) => surface,
            None => match egl.create_window_surface(&window) {
                Ok(surface) => surface,
                Err(error) => {
                    log::error!("{}: {error}", out.name);
                    return;
                }
            },
        };
        let context = match out.context {
            Some(context) => context,
            None => match egl.create_context() {
                Ok(context) => context,
                Err(error) => {
                    log::error!("{}: {error}", out.name);
                    return;
                }
            },
        };

        if let Err(error) = egl.make_current(context, egl_surface) {
            log::error!("{}: {error}", out.name);
            return;
        }
        egl.set_swap_interval(if playback.vsync { 1 } else { 0 });

        let mut player = match Player::new(&path, &playback, mode) {
            Ok(player) => player,
            Err(error) => {
                log::error!("{}: {error}", out.name);
                return;
            }
        };

        let dirty = out.dirty.clone();
        let wake_fd = self.wake_fd;
        let notify = Box::new(move || {
            dirty.store(true, Ordering::Release);
            crate::wake(wake_fd);
        });
        if let Err(error) = player.attach_render(wl_display, notify) {
            log::error!("{}: {error}", out.name);
            return;
        }
        // The render context must exist before playback starts, otherwise mpv
        // tries to open its video output on its own and gives up.
        if let Err(error) = player.load(&path) {
            log::error!("{}: {error}", out.name);
            return;
        }
        player.set_pause(self.paused);
        player.set_brightness(0.0);

        let name = out.name.clone();
        out.egl_window = Some(window);
        out.egl_surface = Some(egl_surface);
        out.context = Some(context);
        out.player = Some(player);
        out.current = path.clone();
        out.fade = None;
        out.span_applied = None;
        log::info!("{name}: playing {path} ({pw}x{ph})");
        if let Some(hwdec) = out
            .player
            .as_ref()
            .and_then(|p| p.get_property("hwdec-current"))
        {
            log::info!("{name}: decoder {hwdec}");
        }
    }

    pub fn paint(&mut self, slot: usize) {
        let egl = self.egl;
        let flip = self.cfg.playback.flip_y;
        let Some(out) = self.outputs.get_mut(slot) else {
            return;
        };
        out.dirty.store(false, Ordering::Release);

        let (Some(player), Some(surface), Some(context)) =
            (out.player.as_ref(), out.egl_surface, out.context)
        else {
            return;
        };
        if egl.make_current(context, surface).is_err() {
            return;
        }
        let (width, height) = out.pixels;
        if let Err(error) = player.render(width, height, flip) {
            log::warn!("{}: {error}", out.name);
            return;
        }
        if let Err(error) = egl.swap(surface) {
            log::warn!("{}: {error}", out.name);
            return;
        }
        player.report_swap();
        out.frames.fetch_add(1, Ordering::Relaxed);
    }

    pub fn paint_dirty(&mut self) {
        for slot in 0..self.outputs.len() {
            if self.outputs[slot].dirty.load(Ordering::Acquire) {
                self.paint(slot);
            }
        }
    }

    fn teardown(&mut self, slot: usize) {
        let egl = self.egl;
        let Some(out) = self.outputs.get_mut(slot) else {
            return;
        };
        out.player = None;
        if let (Some(context), Some(surface)) = (out.context.take(), out.egl_surface.take()) {
            egl.release();
            egl.destroy_surface(surface);
            egl.destroy_context(context);
        }
        out.egl_window = None;
        if let Some(layer) = out.layer.take() {
            layer.destroy();
        }
        if let Some(surface) = out.surface.take() {
            surface.destroy();
        }
        out.configured = false;
    }

    // ----------------------------------------------------------------- rules

    fn should_pause(&self) -> bool {
        if self.manual_paused {
            return true;
        }
        let rules = &self.cfg.rules;
        if rules.pause_on_idle && self.idle_active {
            return true;
        }
        if rules.pause_on_battery && self.on_battery {
            return true;
        }
        let busy =
            |t: &Toplevel| t.alive && (t.fullscreen || (rules.pause_on_maximized && t.maximized));
        if rules.pause_on_fullscreen && self.toplevels.iter().any(busy) {
            return true;
        }
        false
    }

    pub fn refresh_pause(&mut self) {
        let should = self.should_pause();
        if should == self.paused {
            return;
        }
        self.paused = should;
        for out in &mut self.outputs {
            if let Some(player) = out.player.as_mut() {
                player.set_pause(should);
            }
        }
        log::info!("playback {}", if should { "paused" } else { "resumed" });
    }

    pub fn tick(&mut self) {
        let battery = crate::on_battery();
        if battery != self.on_battery {
            self.on_battery = battery;
        }
        self.refresh_pause();
        self.tick_fades();
        self.maybe_advance_playlist();
        self.apply_span();
    }

    // ------------------------------------------------------------------ fades

    fn tick_fades(&mut self) {
        for out in &mut self.outputs {
            let Some(fade) = out.fade.as_ref() else {
                continue;
            };
            let (started, secs) = (fade.started, fade.secs.max(0.01));
            let Some(player) = out.player.as_ref() else {
                out.fade = None;
                continue;
            };
            let progress = (started.elapsed().as_secs_f64() / secs).clamp(0.0, 1.0);
            player.set_brightness(-100.0 * (1.0 - progress));
            if progress >= 1.0 {
                out.fade = None;
            }
        }
    }

    // ---------------------------------------------------------------- span

    /// Bounding box of every connected monitor.
    fn canvas(&self) -> Option<Rect> {
        span::bounding_box(
            self.outputs
                .iter()
                .filter(|out| out.alive && out.configured && out.logical.0 > 0)
                .map(Output::rect),
        )
    }

    /// Apply (or remove) the shared-canvas transform on every player.
    ///
    /// Safe to call on every tick: it only talks to mpv when the canvas or the
    /// video aspect changes.
    pub fn apply_span(&mut self) {
        let monitors = self
            .outputs
            .iter()
            .filter(|out| out.alive && out.configured)
            .count();
        let enabled = self.cfg.wallpaper.span && monitors > 1;

        if !enabled {
            self.span_canvas = None;
            for out in &mut self.outputs {
                if out.span_applied.take().is_some() {
                    if let Some(player) = out.player.as_ref() {
                        player.apply_mode(self.cfg.wallpaper.mode);
                    }
                }
            }
            return;
        }

        let Some(canvas) = self.canvas() else {
            return;
        };
        let canvas_changed = self.span_canvas != Some(canvas);
        self.span_canvas = Some(canvas);

        for slot in 0..self.outputs.len() {
            let out = &mut self.outputs[slot];
            if !out.alive || !out.configured {
                continue;
            }
            let Some(player) = out.player.as_ref() else {
                continue;
            };
            if !canvas_changed && out.span_applied.is_some() {
                continue;
            }
            let Some(aspect) = player.video_aspect() else {
                // mpv has not probed the file yet; retry on the next tick.
                continue;
            };
            let transform = span::transform(canvas, out.rect(), aspect);
            if out
                .span_applied
                .map_or(false, |old| old.approx_eq(&transform))
            {
                out.span_applied = Some(transform);
                continue;
            }
            player.set_span(transform);
            out.span_applied = Some(transform);
            log::debug!(
                "{}: span zoom {:.3}, pan {:.3}/{:.3}",
                out.name,
                transform.zoom,
                transform.pan_x,
                transform.pan_y
            );
        }
    }

    // -------------------------------------------------------------- playlist

    fn random_index(&mut self, len: usize) -> usize {
        // xorshift64: plenty for picking wallpapers, no extra dependency.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng % len.max(1) as u64) as usize
    }

    fn has_own_playlist(&self, output: &str) -> bool {
        self.cfg
            .playlist
            .outputs
            .get(output)
            .map_or(false, |list| !list.is_empty())
    }

    /// Next index in the playlist for `output`, advancing its cursor.
    fn next_index(&mut self, output: &str, len: usize, random: bool) -> usize {
        if len == 0 {
            return 0;
        }
        let own = self.has_own_playlist(output);
        let index = if random {
            self.random_index(len)
        } else if own {
            self.output_cursors.get(output).copied().unwrap_or(0) % len
        } else {
            self.playlist_cursor % len
        };
        let next = (index + 1) % len;
        if own {
            self.output_cursors.insert(output.to_string(), next);
        } else {
            self.playlist_cursor = next;
        }
        index
    }

    fn load_track(&mut self, slot: usize, path: String) {
        let secs = self.cfg.playlist.fade_secs;
        let paused = self.paused;
        let out = &mut self.outputs[slot];
        let Some(player) = out.player.as_mut() else {
            return;
        };
        player.set_brightness(-100.0);
        if let Err(error) = player.load(&path) {
            log::warn!("{}: {error}", out.name);
            player.set_brightness(0.0);
            return;
        }
        player.set_pause(paused);
        log::info!("{}: playlist → {path}", out.name);
        out.current = path;
        out.span_applied = None;
        if secs > 0.01 {
            out.fade = Some(Fade {
                started: Instant::now(),
                secs,
            });
        } else {
            player.set_brightness(0.0);
            out.fade = None;
        }
    }

    fn pick_for(&mut self, slot: usize, random: bool) -> Option<String> {
        let name = self.outputs[slot].name.clone();
        let files = self.cfg.playlist_for(&name);
        if files.is_empty() {
            return None;
        }
        let mut index = self.next_index(&name, files.len(), random);
        if files.len() > 1 && files[index] == self.outputs[slot].current {
            index = (index + 1) % files.len();
        }
        Some(files[index].clone())
    }

    /// Switch one monitor, or all of them, to the next playlist entry.
    ///
    /// Returns the file applied to the last monitor, for the control client.
    pub fn skip(&mut self, target: Option<String>, random: bool) -> Option<String> {
        self.last_advance = Instant::now();
        let slots: Vec<usize> = self
            .outputs
            .iter()
            .enumerate()
            .filter(|(_, out)| {
                out.alive
                    && out.player.is_some()
                    && target.as_ref().map_or(true, |name| &out.name == name)
            })
            .map(|(slot, _)| slot)
            .collect();
        if slots.is_empty() {
            return None;
        }

        if target.is_some() {
            let slot = slots[0];
            let path = self.pick_for(slot, random)?;
            self.load_track(slot, path.clone());
            return Some(path);
        }

        let mut last = None;

        // Monitors with their own `[playlist.outputs]` list rotate alone.
        for slot in slots.iter().copied() {
            let name = self.outputs[slot].name.clone();
            if !self.has_own_playlist(&name) {
                continue;
            }
            if let Some(path) = self.pick_for(slot, random) {
                self.load_track(slot, path.clone());
                last = Some(path);
            }
        }

        // Everybody else shares one list and one picture.
        let shared: Vec<usize> = slots
            .into_iter()
            .filter(|slot| !self.has_own_playlist(&self.outputs[*slot].name))
            .collect();
        if let Some(&first) = shared.first() {
            let name = self.outputs[first].name.clone();
            let files = self.cfg.playlist_for(&name);
            if !files.is_empty() {
                let mut index = self.next_index(&name, files.len(), random);
                if files.len() > 1 && files[index] == self.outputs[first].current {
                    index = (index + 1) % files.len();
                }
                let path = files[index].clone();
                for slot in shared {
                    self.load_track(slot, path.clone());
                }
                last = Some(path);
            }
        }

        last
    }

    fn maybe_advance_playlist(&mut self) {
        if !self.cfg.playlist.enabled {
            return;
        }
        let interval = Duration::from_secs(self.cfg.playlist.interval());
        if self.last_advance.elapsed() < interval {
            return;
        }
        self.last_advance = Instant::now();
        let shuffle = self.cfg.playlist.shuffle;
        if self.skip(None, shuffle).is_none() {
            log::debug!("playlist is enabled but has no files");
        }
    }

    // ----------------------------------------------------------- wallpaper ops

    pub fn set_wallpaper(&mut self, path: Option<String>, mode: Option<Mode>) {
        if let Some(mode) = mode {
            self.cfg.wallpaper.mode = mode;
            for out in &mut self.outputs {
                if let Some(player) = out.player.as_ref() {
                    player.apply_mode(mode);
                }
            }
        }
        if let Some(path) = path {
            self.cfg.wallpaper.path = path;
            self.restart_players();
        }
    }

    /// Forget every per-monitor override and fall back to the global wallpaper.
    pub fn clear_overrides(&mut self) {
        self.clear_override(None);
    }

    /// Forget one monitor's override, or all of them when `output` is `None`.
    pub fn clear_override(&mut self, output: Option<&str>) {
        let changed = match output {
            Some(name) => self.cfg.outputs.remove(name).is_some(),
            None if !self.cfg.outputs.is_empty() => {
                self.cfg.outputs.clear();
                true
            }
            None => false,
        };
        if changed {
            self.restart_players();
        }
    }

    /// Point one monitor at a different file.
    pub fn set_output_wallpaper(&mut self, output: &str, path: String) {
        // A per-monitor picture only makes sense without span; choosing one
        // is an implicit request to leave the shared canvas.
        self.cfg.wallpaper.span = false;
        self.cfg.outputs.insert(output.to_string(), path);
        let Some(slot) = self
            .outputs
            .iter()
            .position(|o| o.alive && o.name == output)
        else {
            return;
        };
        self.outputs[slot].player = None;
        self.outputs[slot].span_applied = None;
        self.create_surface(slot);
        self.ensure_stream(slot);
        self.paint(slot);
        self.apply_span();
    }

    /// Write the current configuration back to disk.
    ///
    /// Called after the daemon itself changes settings, so that `veilctl set`
    /// and the D-Bus service survive a restart. Playlist rotations are
    /// deliberately not saved: they are transient.
    pub fn persist(&self) {
        if let Err(error) = self.cfg.save_to(&self.cfg_path) {
            log::warn!("cannot save {}: {error}", self.cfg_path.display());
        }
    }

    pub fn restart_players(&mut self) {
        let slots: Vec<usize> = (0..self.outputs.len()).collect();
        for slot in &slots {
            let out = &mut self.outputs[*slot];
            out.player = None;
            out.span_applied = None;
            out.fade = None;
        }
        for slot in slots {
            self.create_surface(slot);
            self.ensure_stream(slot);
        }
        self.refresh_pause();
        self.apply_span();
    }

    pub fn reload(&mut self) {
        match Config::load_from(&self.cfg_path) {
            Ok((cfg, _)) => {
                let mode_changed = cfg.wallpaper.mode != self.cfg.wallpaper.mode;
                let path_changed = cfg.wallpaper.path != self.cfg.wallpaper.path;
                let span_changed = cfg.wallpaper.span != self.cfg.wallpaper.span;
                let idle_changed = cfg.rules.idle_timeout_secs != self.cfg.rules.idle_timeout_secs;
                let idle_enabled = cfg.rules.pause_on_idle;
                let playback_changed = cfg.playback.hwdec != self.cfg.playback.hwdec
                    || cfg.playback.scale_filter != self.cfg.playback.scale_filter
                    || cfg.playback.fps_limit != self.cfg.playback.fps_limit
                    || cfg.playback.vsync != self.cfg.playback.vsync
                    || cfg.playback.flip_y != self.cfg.playback.flip_y
                    || cfg.playback.mute != self.cfg.playback.mute;
                let playlist_changed = cfg.playlist.enabled != self.cfg.playlist.enabled
                    || cfg.playlist.interval_secs != self.cfg.playlist.interval_secs
                    || cfg.playlist.shuffle != self.cfg.playlist.shuffle
                    || cfg.playlist.items != self.cfg.playlist.items
                    || cfg.playlist.outputs != self.cfg.playlist.outputs;
                // Turning the rotation off returns every monitor to the file
                // stored in the config, instead of freezing on the last pick.
                let playlist_stopped = self.cfg.playlist.enabled && !cfg.playlist.enabled;
                self.cfg = cfg;
                if mode_changed {
                    for out in &mut self.outputs {
                        if let Some(player) = out.player.as_ref() {
                            player.apply_mode(self.cfg.wallpaper.mode);
                            out.span_applied = None;
                        }
                    }
                }
                if path_changed || playback_changed || span_changed || playlist_stopped {
                    self.restart_players();
                }
                if playlist_changed {
                    // Do not skip immediately after the user edited the list.
                    self.last_advance = Instant::now();
                }
                if idle_changed || idle_enabled != self.idle_notification.is_some() {
                    self.idle_timeout = self.cfg.rules.idle_timeout_secs;
                    self.recreate_idle();
                }
                self.refresh_pause();
                self.apply_span();
                log::info!("configuration reloaded");
            }
            Err(error) => log::error!("reload failed: {error}"),
        }
    }

    pub fn status(&self) -> serde_json::Value {
        let outputs: Vec<_> = self
            .outputs
            .iter()
            .filter(|o| o.alive)
            .map(|o| {
                let player = o.player.as_ref();
                serde_json::json!({
                    "name": o.name,
                    "logical": { "width": o.logical.0, "height": o.logical.1 },
                    "pixels": { "width": o.pixels.0, "height": o.pixels.1 },
                    "position": { "x": o.position.0, "y": o.position.1 },
                    "scale": o.scale,
                    "configured": o.configured,
                    "override": self.cfg.has_override(&o.name),
                    "path": if o.current.is_empty() { self.cfg.path_for(&o.name) } else { o.current.clone() },
                    "decoder": player.and_then(|p| p.get_property("hwdec-current")),
                    "codec": player.and_then(|p| p.get_property("video-codec")),
                    "frames": o.frames.load(Ordering::Relaxed),
                })
            })
            .collect();

        let reasons: Vec<&str> = {
            let mut reasons = Vec::new();
            if self.manual_paused {
                reasons.push("manual");
            }
            if self.idle_active {
                reasons.push("idle");
            }
            if self.on_battery {
                reasons.push("battery");
            }
            if self.toplevels.iter().any(|t| t.alive && t.fullscreen) {
                reasons.push("fullscreen");
            }
            reasons
        };

        serde_json::json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
            "mpv": format!("{}.{}", crate::mpv::api_version().0, crate::mpv::api_version().1),
            "paused": self.paused,
            "paused_by": reasons,
            "uptime_secs": self.started.elapsed().as_secs(),
            "mode": self.cfg.wallpaper.mode,
            "span": self.cfg.wallpaper.span && self.outputs.iter().filter(|o| o.alive).count() > 1,
            "hwdec": self.cfg.playback.hwdec,
            "playlist": {
                "enabled": self.cfg.playlist.enabled,
                "interval_secs": self.cfg.playlist.interval(),
                "shuffle": self.cfg.playlist.shuffle,
                "fade_secs": self.cfg.playlist.fade_secs,
                "next_in_secs": if self.cfg.playlist.enabled {
                    self.cfg.playlist.interval().saturating_sub(self.last_advance.elapsed().as_secs())
                } else {
                    0
                },
            },
            "outputs": outputs,
        })
    }
}

fn seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    (nanos ^ (std::process::id() as u64).rotate_left(32)) | 1
}

// ------------------------------------------------------------------ dispatch

macro_rules! noop_dispatch {
    ($iface:ty, $udata:ty) => {
        impl Dispatch<$iface, $udata> for App {
            fn event(
                _: &mut Self,
                _: &$iface,
                _: <$iface as Proxy>::Event,
                _: &$udata,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

noop_dispatch!(WlCompositor, ());
noop_dispatch!(WlRegion, ());
noop_dispatch!(WlSeat, ());
noop_dispatch!(WlSurface, Slot);
noop_dispatch!(ZwlrLayerShellV1, ());
noop_dispatch!(ExtIdleNotifierV1, ());

impl Dispatch<WlRegistry, ()> for App {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => state.bind_global(registry, name, &interface, version),
            wl_registry::Event::GlobalRemove { name } => state.remove_global(name),
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, Slot> for App {
    fn event(
        state: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        slot: &Slot,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(out) = state.outputs.get_mut(slot.0) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => out.name = name,
            wl_output::Event::Geometry { x, y, .. } => {
                out.position = (x, y);
                log::debug!("{}: logical position {x},{y}", out.name);
            }
            wl_output::Event::Scale { factor } => {
                if factor > 0 {
                    out.scale = factor;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, Slot> for App {
    fn event(
        state: &mut Self,
        _: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        slot: &Slot,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => state.on_configure(slot.0, serial, width, height),
            zwlr_layer_surface_v1::Event::Closed => {
                log::info!("surface closed by the compositor");
                state.teardown(slot.0);
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for App {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => {
                state.idle_active = true;
                state.refresh_pause();
            }
            ext_idle_notification_v1::Event::Resumed => {
                state.idle_active = false;
                state.refresh_pause();
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for App {
    fn event_created_child(
        _: u16,
        handle: &QueueHandle<Self>,
    ) -> Arc<dyn wayland_client::backend::ObjectData> {
        // The only server-created child object is a toplevel handle.
        handle.make_data::<ZwlrForeignToplevelHandleV1, ()>(())
    }

    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            manager::Event::Toplevel { toplevel } => {
                let id = toplevel.id();
                state.toplevels.push(Toplevel {
                    handle: Some(toplevel),
                    id,
                    alive: true,
                    fullscreen: false,
                    maximized: false,
                    title: String::new(),
                });
            }
            manager::Event::Finished => state.toplevel_manager = None,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for App {
    fn event(
        state: &mut Self,
        proxy: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(index) = state.toplevels.iter().position(|t| t.id == proxy.id()) else {
            return;
        };
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => {
                state.toplevels[index].title = title;
            }
            zwlr_foreign_toplevel_handle_v1::Event::State { state: raw } => {
                let mut fullscreen = false;
                let mut maximized = false;
                for chunk in raw.chunks_exact(4) {
                    match u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) {
                        0 => maximized = true,
                        3 => fullscreen = true,
                        _ => {}
                    }
                }
                let toplevel = &mut state.toplevels[index];
                toplevel.fullscreen = fullscreen;
                toplevel.maximized = maximized;
                state.refresh_pause();
            }
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                if let Some(handle) = state.toplevels[index].handle.take() {
                    handle.destroy();
                }
                state.toplevels.remove(index);
                state.refresh_pause();
            }
            _ => {}
        }
    }
}
