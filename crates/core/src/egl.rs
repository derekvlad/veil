//! EGL glue: one display, one config, one context per output.
//!
//! Everything is rendered through libmpv into the default framebuffer of a
//! `wl_egl_window`, which the compositor turns into a hardware overlay — no
//! shm copies, no CPU readback.

use khronos_egl as egl;
use std::ffi::CStr;
use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::OnceLock;
use wayland_egl::WlEglSurface;

static INSTANCE: OnceLock<egl::Instance<egl::Static>> = OnceLock::new();

fn instance() -> &'static egl::Instance<egl::Static> {
    INSTANCE.get_or_init(|| egl::Instance::new(egl::Static))
}

/// `get_proc_address` callback handed to libmpv.
///
/// libmpv never links against OpenGL itself; it resolves every entry point
/// through this function.
pub unsafe extern "C" fn get_proc_address(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    if name.is_null() {
        return ptr::null_mut();
    }
    let Ok(name) = CStr::from_ptr(name).to_str() else {
        return ptr::null_mut();
    };
    match instance().get_proc_address(name) {
        Some(function) => function as *mut c_void,
        None => ptr::null_mut(),
    }
}

fn describe(error: egl::Error) -> String {
    format!("EGL error: {error:?}")
}

#[derive(Clone, Copy)]
pub struct Egl {
    display: egl::Display,
    config: egl::Config,
}

impl Egl {
    pub fn new(native_display: *mut c_void) -> Result<Self, String> {
        let egl = instance();
        let display = unsafe { egl.get_display(native_display) }
            .ok_or_else(|| "EGL: eglGetDisplay() failed".to_string())?;
        let (major, minor) = egl.initialize(display).map_err(describe)?;
        log::debug!("EGL {major}.{minor} initialised");
        egl.bind_api(egl::OPENGL_API).map_err(describe)?;

        let attributes = [
            egl::SURFACE_TYPE,
            egl::WINDOW_BIT,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_BIT,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            8,
            egl::NONE,
        ];
        let config = egl
            .choose_first_config(display, &attributes)
            .map_err(describe)?
            .ok_or_else(|| "EGL: no matching framebuffer configuration".to_string())?;

        Ok(Egl { display, config })
    }

    pub fn create_context(&self) -> Result<egl::Context, String> {
        instance()
            .create_context(self.display, self.config, None, &[egl::NONE])
            .map_err(describe)
    }

    pub fn create_window_surface(&self, window: &WlEglSurface) -> Result<egl::Surface, String> {
        unsafe {
            instance().create_window_surface(
                self.display,
                self.config,
                window.ptr() as *mut c_void,
                None,
            )
        }
        .map_err(describe)
    }

    pub fn make_current(&self, context: egl::Context, surface: egl::Surface) -> Result<(), String> {
        instance()
            .make_current(self.display, Some(surface), Some(surface), Some(context))
            .map_err(describe)
    }

    pub fn swap(&self, surface: egl::Surface) -> Result<(), String> {
        instance()
            .swap_buffers(self.display, surface)
            .map_err(describe)
    }

    pub fn set_swap_interval(&self, interval: i32) {
        let _ = instance().swap_interval(self.display, interval);
    }

    /// Drop the current context (must happen before destroying it).
    pub fn release(&self) {
        let _ = instance().make_current(self.display, None, None, None);
    }

    pub fn destroy_surface(&self, surface: egl::Surface) {
        let _ = instance().destroy_surface(self.display, surface);
    }

    pub fn destroy_context(&self, context: egl::Context) {
        let _ = instance().destroy_context(self.display, context);
    }
}
