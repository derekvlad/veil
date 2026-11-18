//! Minimal, hand-written bindings to libmpv's client and render API.
//!
//! We only need a fraction of the API: create a player, feed it options, render
//! frames into our own OpenGL FBO and be notified when a new frame is ready.
//! Writing the FFI by hand keeps the build free of bindgen and of crates that
//! would otherwise drag in a second copy of the player.

#![allow(non_camel_case_types)]

use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::config::Mode;

#[repr(C)]
pub struct mpv_handle {
    _private: [u8; 0],
}

#[repr(C)]
pub struct mpv_render_context {
    _private: [u8; 0],
}

#[repr(C)]
pub struct mpv_event {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_opengl_init_params {
    pub get_proc_address:
        Option<unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void>,
    pub get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
#[derive(Default)]
pub struct mpv_opengl_fbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

#[repr(C)]
pub struct mpv_render_param {
    pub type_: c_int,
    pub data: *mut c_void,
}

const MPV_RENDER_PARAM_INVALID: c_int = 0;
const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
const MPV_RENDER_PARAM_OPENGL_FBO: c_int = 3;
const MPV_RENDER_PARAM_FLIP_Y: c_int = 4;
const MPV_RENDER_PARAM_WL_DISPLAY: c_int = 9;

#[link(name = "mpv")]
extern "C" {
    fn mpv_client_api_version() -> u64;
    fn mpv_error_string(error: c_int) -> *const c_char;

    fn mpv_create() -> *mut mpv_handle;
    fn mpv_initialize(ctx: *mut mpv_handle) -> c_int;
    fn mpv_terminate_destroy(ctx: *mut mpv_handle);
    fn mpv_set_option_string(
        ctx: *mut mpv_handle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;
    fn mpv_set_property_string(
        ctx: *mut mpv_handle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;
    fn mpv_get_property_string(ctx: *mut mpv_handle, name: *const c_char) -> *mut c_char;
    fn mpv_command_string(ctx: *mut mpv_handle, args: *const c_char) -> c_int;
    fn mpv_free(data: *mut c_void);
    fn mpv_wait_event(ctx: *mut mpv_handle, timeout: f64) -> *mut mpv_event;

    fn mpv_render_context_create(
        out: *mut *mut mpv_render_context,
        ctx: *mut mpv_handle,
        params: *mut mpv_render_param,
    ) -> c_int;
    fn mpv_render_context_set_update_callback(
        ctx: *mut mpv_render_context,
        callback: Option<unsafe extern "C" fn(*mut c_void)>,
        data: *mut c_void,
    );
    fn mpv_render_context_render(
        ctx: *mut mpv_render_context,
        params: *mut mpv_render_param,
    ) -> c_int;
    fn mpv_render_context_report_swap(ctx: *mut mpv_render_context);
    fn mpv_render_context_free(ctx: *mut mpv_render_context);
}

fn error_string(code: c_int) -> String {
    unsafe {
        let p = mpv_error_string(code);
        if p.is_null() {
            format!("mpv error {code}")
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

fn check(code: c_int, what: &str) -> Result<(), String> {
    if code < 0 {
        Err(format!("{what}: {}", error_string(code)))
    } else {
        Ok(())
    }
}

/// Wrapper that makes a raw mpv handle sendable to the draining thread.
struct SendPtr<T>(*mut T);
unsafe impl<T> Send for SendPtr<T> {}

/// A libmpv instance plus its render context.
///
/// Not `Sync`: mpv guarantees thread safety internally, but the render context
/// must be used from the thread that owns the current GL context.
pub struct Player {
    handle: *mut mpv_handle,
    render: *mut mpv_render_context,
    notifier: Option<Box<Notifier>>,
    stop: Arc<AtomicBool>,
    drain: Option<JoinHandle<()>>,
    paused: bool,
}

struct Notifier {
    callback: Box<dyn Fn() + Send + Sync>,
}

unsafe extern "C" fn update_trampoline(ctx: *mut c_void) {
    let notifier = unsafe { &*(ctx as *const Notifier) };
    (notifier.callback)();
}

fn path_is_static_image(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let extension = lower.rsplit('.').next().unwrap_or("");
    matches!(
        extension,
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "avif" | "jxl" | "tiff" | "tif"
    )
}

pub fn api_version() -> (u32, u32) {
    let v = unsafe { mpv_client_api_version() };
    ((v >> 16) as u32, (v & 0xffff) as u32)
}

impl Player {
    /// Create a player and start playing `path`. The GL context used later for
    /// rendering does not have to be current yet.
    pub fn new(path: &str, playback: &crate::config::Playback, mode: Mode) -> Result<Self, String> {
        let handle = unsafe { mpv_create() };
        if handle.is_null() {
            return Err("mpv_create() failed".into());
        }

        let mut player = Player {
            handle,
            render: ptr::null_mut(),
            notifier: None,
            stop: Arc::new(AtomicBool::new(false)),
            drain: None,
            paused: false,
        };

        // A still image must not be looped: mpv would re-deliver it forever.
        let static_image = path_is_static_image(path);
        let options = [
            ("vo", "libmpv"),
            ("hwdec", playback.hwdec.as_str()),
            ("gpu-api", "opengl"),
            ("loop-file", if static_image { "no" } else { "inf" }),
            ("loop-playlist", "inf"),
            ("audio", "no"),
            ("terminal", "no"),
            ("input-terminal", "no"),
            ("input-default-bindings", "no"),
            ("osc", "no"),
            ("osd-level", "0"),
            ("image-display-duration", "inf"),
            ("scale", playback.scale_filter.as_str()),
            ("background-color", "#000000"),
            ("idle", "yes"),
            ("keep-open", "no"),
        ];
        for (key, value) in options {
            player.set_option(key, value)?;
        }

        player.set_option("mute", if playback.mute { "yes" } else { "no" })?;
        if std::env::var_os("VEIL_MPV_LOG").is_some() {
            player.set_option("terminal", "yes")?;
            player.set_option("msg-level", "all=v")?;
        }
        if playback.fps_limit > 0 {
            player.set_option("vf", &format!("fps={}", playback.fps_limit))?;
        }

        let rc = unsafe { mpv_initialize(handle) };
        if rc < 0 {
            unsafe { mpv_terminate_destroy(handle) };
            return Err(format!("mpv_initialize: {}", error_string(rc)));
        }

        player.start_event_drain();
        player.apply_mode(mode);
        Ok(player)
    }

    fn set_option(&self, name: &str, value: &str) -> Result<(), String> {
        let cname = std::ffi::CString::new(name).unwrap();
        let cvalue = std::ffi::CString::new(value).unwrap();
        check(
            unsafe { mpv_set_option_string(self.handle, cname.as_ptr(), cvalue.as_ptr()) },
            &format!("option {name}={value}"),
        )
    }

    fn set_property(&self, name: &str, value: &str) -> Result<(), String> {
        let cname = std::ffi::CString::new(name).unwrap();
        let cvalue = std::ffi::CString::new(value).unwrap();
        check(
            unsafe { mpv_set_property_string(self.handle, cname.as_ptr(), cvalue.as_ptr()) },
            &format!("property {name}={value}"),
        )
    }

    pub fn get_property(&self, name: &str) -> Option<String> {
        let cname = std::ffi::CString::new(name).ok()?;
        let raw = unsafe { mpv_get_property_string(self.handle, cname.as_ptr()) };
        if raw.is_null() {
            return None;
        }
        let value = unsafe { CStr::from_ptr(raw).to_string_lossy().into_owned() };
        unsafe { mpv_free(raw as *mut c_void) };
        Some(value)
    }

    pub fn load(&mut self, path: &str) -> Result<(), String> {
        let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
        let cmd = format!("loadfile \"{escaped}\" replace");
        let ccmd = std::ffi::CString::new(cmd).unwrap();
        check(
            unsafe { mpv_command_string(self.handle, ccmd.as_ptr()) },
            "loadfile",
        )
    }

    /// Sync the presentation with the screen refresh (or not).
    pub fn set_vsync(&mut self, _enabled: bool) {
        // Actual swap interval lives in the EGL layer; kept for symmetry.
    }

    pub fn apply_mode(&self, mode: Mode) {
        let (keepaspect, panscan, unscaled) = match mode {
            Mode::Fill => ("yes", "1.0", "no"),
            Mode::Fit => ("yes", "0", "no"),
            Mode::Stretch => ("no", "0", "no"),
            Mode::Center => ("yes", "0", "yes"),
        };
        let _ = self.set_property("keepaspect", keepaspect);
        let _ = self.set_property("panscan", panscan);
        let _ = self.set_property("video-unscaled", unscaled);
    }

    pub fn set_pause(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        if self.set_property("pause", if paused { "yes" } else { "no" }).is_ok() {
            self.paused = paused;
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Create the render context. The caller must have made its GL context
    /// current before calling this.
    pub fn attach_render(
        &mut self,
        wl_display: *mut c_void,
        update: Box<dyn Fn() + Send + Sync>,
    ) -> Result<(), String> {
        let notifier = Box::new(Notifier { callback: update });
        let mut init = mpv_opengl_init_params {
            get_proc_address: Some(crate::egl::get_proc_address),
            get_proc_address_ctx: ptr::null_mut(),
        };
        let mut params = [
            mpv_render_param {
                type_: MPV_RENDER_PARAM_API_TYPE,
                data: c"opengl".as_ptr() as *mut c_void,
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: &mut init as *mut _ as *mut c_void,
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_WL_DISPLAY,
                data: wl_display,
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];

        let mut ctx: *mut mpv_render_context = ptr::null_mut();
        let rc = unsafe { mpv_render_context_create(&mut ctx, self.handle, params.as_mut_ptr()) };
        if rc < 0 {
            return Err(format!(
                "mpv_render_context_create: {}",
                error_string(rc)
            ));
        }
        self.render = ctx;

        let notifier_ptr = &*notifier as *const Notifier as *mut c_void;
        unsafe {
            mpv_render_context_set_update_callback(ctx, Some(update_trampoline), notifier_ptr);
        }
        self.notifier = Some(notifier);
        Ok(())
    }

    /// Draw the current frame into the default framebuffer.
    pub fn render(&self, width: i32, height: i32, flip_y: bool) -> Result<(), String> {
        if self.render.is_null() {
            return Ok(());
        }
        let mut fbo = mpv_opengl_fbo {
            fbo: 0,
            w: width,
            h: height,
            internal_format: 0,
        };
        let mut flip: c_int = flip_y as c_int;
        let mut params = [
            mpv_render_param {
                type_: MPV_RENDER_PARAM_OPENGL_FBO,
                data: &mut fbo as *mut _ as *mut c_void,
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_FLIP_Y,
                data: &mut flip as *mut _ as *mut c_void,
            },
            mpv_render_param {
                type_: MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        let rc = unsafe { mpv_render_context_render(self.render, params.as_mut_ptr()) };
        check(rc, "render")
    }

    pub fn report_swap(&self) {
        if !self.render.is_null() {
            unsafe { mpv_render_context_report_swap(self.render) };
        }
    }

    /// mpv blocks when nobody reads its event queue, so keep a thread draining it.
    fn start_event_drain(&mut self) {
        let handle = SendPtr(self.handle);
        let stop = self.stop.clone();
        self.drain = Some(thread::spawn(move || {
            let handle = handle;
            while !stop.load(Ordering::Relaxed) {
                unsafe {
                    let event = mpv_wait_event(handle.0, 0.2);
                    if event.is_null() || (*event).event_id == -1 {
                        continue;
                    }
                }
            }
        }));
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.drain.take() {
            let _ = thread.join();
        }
        if !self.render.is_null() {
            unsafe { mpv_render_context_free(self.render) };
            self.render = ptr::null_mut();
        }
        if !self.handle.is_null() {
            unsafe { mpv_terminate_destroy(self.handle) };
            self.handle = ptr::null_mut();
        }
        self.notifier = None;
    }
}
