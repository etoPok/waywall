use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

use anyhow::{bail, Ok, Result};
use tracing::debug;

#[link(name = "EGL")]
extern "C" {
    pub fn eglGetDisplay(native_display: *mut c_void) -> *mut c_void;
    pub fn eglInitialize(display: *mut c_void, major: *mut c_int, minor: *mut c_int) -> u32;
    pub fn eglBindAPI(api: u32) -> u32;
    pub fn eglChooseConfig(
        display: *mut c_void,
        attrib_list: *const c_int,
        configs: *mut *mut c_void,
        config_size: c_int,
        num_config: *mut c_int,
    ) -> u32;
    pub fn eglCreateWindowSurface(
        display: *mut c_void,
        config: *mut c_void,
        native_window: *mut c_void,
        attrib_list: *const c_int,
    ) -> *mut c_void;
    pub fn eglCreateContext(
        display: *mut c_void,
        config: *mut c_void,
        share_context: *mut c_void,
        attrib_list: *const c_int,
    ) -> *mut c_void;
    pub fn eglMakeCurrent(
        display: *mut c_void,
        draw: *mut c_void,
        read: *mut c_void,
        ctx: *mut c_void,
    ) -> u32;
    pub fn eglSwapBuffers(display: *mut c_void, surface: *mut c_void) -> u32;
    pub fn eglSwapInterval(display: *mut c_void, interval: c_int) -> u32;
    pub fn eglGetProcAddress(procname: *const c_char) -> *mut c_void;
    pub fn eglDestroyContext(display: *mut c_void, ctx: *mut c_void) -> u32;
    pub fn eglDestroySurface(display: *mut c_void, surface: *mut c_void) -> u32;
    pub fn eglTerminate(display: *mut c_void) -> u32;
}

#[link(name = "wayland-egl")]
extern "C" {
    pub fn wl_egl_window_create(surface: *mut c_void, width: c_int, height: c_int) -> *mut c_void;
    pub fn wl_egl_window_destroy(egl_window: *mut c_void);
    pub fn wl_egl_window_resize(
        // unused but kept for resize support
        egl_window: *mut c_void,
        width: c_int,
        height: c_int,
        dx: c_int,
        dy: c_int,
    );
}

pub const EGL_OPENGL_API: u32 = 0x30A2;
pub const EGL_NONE: c_int = 0x3038;
pub const EGL_SURFACE_TYPE: c_int = 0x3033;
pub const EGL_WINDOW_BIT: c_int = 0x0004;
pub const EGL_RENDERABLE_TYPE: c_int = 0x3040;
pub const EGL_OPENGL_BIT: c_int = 0x0008;
pub const EGL_RED_SIZE: c_int = 0x3024;
pub const EGL_GREEN_SIZE: c_int = 0x3023;
pub const EGL_BLUE_SIZE: c_int = 0x3022;
pub const EGL_ALPHA_SIZE: c_int = 0x3021;
pub const EGL_DEPTH_SIZE: c_int = 0x3025;
pub const EGL_CONTEXT_MAJOR_VERSION: c_int = 0x3098;
pub const EGL_CONTEXT_MINOR_VERSION: c_int = 0x30FB;
pub const EGL_NO_DISPLAY: *mut c_void = ptr::null_mut();
pub const EGL_NO_CONTEXT: *mut c_void = ptr::null_mut();
pub const EGL_NO_SURFACE: *mut c_void = ptr::null_mut();

#[allow(clippy::missing_safety_doc)]
pub unsafe fn init_egl_display(wl_display: *mut c_void) -> Result<(*mut c_void, *mut c_void)> {
    let egl_display = eglGetDisplay(wl_display);
    if egl_display == EGL_NO_DISPLAY {
        bail!("eglGetDisplay failed");
    }

    let mut major: c_int = 0;
    let mut minor: c_int = 0;
    if eglInitialize(egl_display, &mut major, &mut minor) == 0 {
        bail!("eglInitialize failed")
    }
    debug!("EGL {}.{} initialized", major, minor);

    if eglBindAPI(EGL_OPENGL_API) == 0 {
        bail!("eglBindAPI failed");
    }

    #[rustfmt::skip]
    let attribs_config: [c_int; 15] = [
      EGL_SURFACE_TYPE,     EGL_WINDOW_BIT,
      EGL_RENDERABLE_TYPE,  EGL_OPENGL_BIT,
      EGL_RED_SIZE,         8,
      EGL_GREEN_SIZE,       8,
      EGL_BLUE_SIZE,        8,
      EGL_ALPHA_SIZE,       8,
      EGL_DEPTH_SIZE,       0,
      EGL_NONE,
    ];
    let mut egl_config: *mut c_void = ptr::null_mut();
    let mut num_configs: c_int = 0;
    if eglChooseConfig(
        egl_display,
        attribs_config.as_ptr(),
        &mut egl_config,
        1,
        &mut num_configs,
    ) == 0
        || num_configs == 0
    {
        bail!("eglChooseConfig failed or no valid configs found");
    }

    Ok((egl_display, egl_config))
}

#[allow(clippy::missing_safety_doc)]
pub unsafe fn create_egl_surface(
    egl_display: *mut c_void,
    wl_surface: *mut c_void,
    egl_config: *mut c_void,
    width: i32,
    height: i32,
) -> Result<(*mut c_void, *mut c_void)> {
    let egl_window = wl_egl_window_create(wl_surface, width, height);
    if egl_window.is_null() {
        bail!("wl_egl_window_create failed");
    }

    let egl_surface = eglCreateWindowSurface(egl_display, egl_config, egl_window, ptr::null());
    if egl_surface == EGL_NO_SURFACE {
        wl_egl_window_destroy(egl_window);
        bail!("eglCreateWindowSurface failed");
    }

    Ok((egl_surface, egl_window))
}

#[allow(clippy::missing_safety_doc)]
pub unsafe fn create_egl_ctx(
    egl_display: *mut c_void,
    egl_config: *mut c_void,
) -> Result<*mut c_void> {
    #[rustfmt::skip]
    let attribs_ctx: [c_int; 5] = [
        EGL_CONTEXT_MAJOR_VERSION, 3,
        EGL_CONTEXT_MINOR_VERSION, 3,
        EGL_NONE,
    ];
    let egl_context = eglCreateContext(
        egl_display,
        egl_config,
        EGL_NO_CONTEXT,
        attribs_ctx.as_ptr(),
    );
    if egl_context == EGL_NO_CONTEXT {
        bail!("eglCreateContext failed");
    }
    Ok(egl_context)
}
