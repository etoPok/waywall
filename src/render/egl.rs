use std::os::raw::{c_int, c_void};
use std::ptr;

use anyhow::{bail, Result};
use tracing::info;

use crate::bindings::egl::{
    eglBindAPI, eglChooseConfig, eglCreateContext, eglCreateWindowSurface, eglDestroyContext,
    eglDestroySurface, eglGetDisplay, eglInitialize, eglMakeCurrent, eglSwapInterval,
    EGL_ALPHA_SIZE, EGL_BLUE_SIZE, EGL_CONTEXT_MAJOR_VERSION, EGL_CONTEXT_MINOR_VERSION,
    EGL_DEPTH_SIZE, EGL_GREEN_SIZE, EGL_NONE, EGL_NO_CONTEXT, EGL_NO_DISPLAY, EGL_NO_SURFACE,
    EGL_OPENGL_API, EGL_OPENGL_BIT, EGL_RED_SIZE, EGL_RENDERABLE_TYPE, EGL_SURFACE_TYPE,
    EGL_WINDOW_BIT,
};
use crate::bindings::wayland_egl::{wl_egl_window_create, wl_egl_window_destroy};

/// Initializes EGL/OpenGL on the given wl_surface.
///
/// Receives:
///   - wl_display_ptr: pointer to wl_display* (from Connection::backend().display_ptr())
///   - wl_surface_ptr: pointer to wl_surface* (from proxy_to_raw_ptr)
///   - width, height: output dimensions
///
/// Returns: (egl_display, egl_surface, egl_context, egl_window)
pub unsafe fn init_egl(
    wl_display_ptr: *mut c_void,
    wl_surface_ptr: *mut c_void,
    width: i32,
    height: i32,
) -> Result<(*mut c_void, *mut c_void, *mut c_void, *mut c_void)> {
    // use client connection with the compositor
    let egl_display = eglGetDisplay(wl_display_ptr);
    if egl_display == EGL_NO_DISPLAY {
        bail!("eglGetDisplay falló");
    }

    let mut major: c_int = 0;
    let mut minor: c_int = 0;
    if eglInitialize(egl_display, &mut major, &mut minor) == 0 {
        bail!("eglInitialize falló");
    }
    info!("EGL {}.{} initialized", major, minor);

    if eglBindAPI(EGL_OPENGL_API) == 0 {
        bail!("eglBindAPI(OPENGL) falló");
    }

    #[rustfmt::skip]
    let attribs_config: [c_int; 15] = [
        EGL_SURFACE_TYPE,    EGL_WINDOW_BIT,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT,
        EGL_RED_SIZE,        8,
        EGL_GREEN_SIZE,      8,
        EGL_BLUE_SIZE,       8,
        EGL_ALPHA_SIZE,      8,
        EGL_DEPTH_SIZE,      0,
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
        bail!("eglChooseConfig falló o no encontró configuraciones válidas");
    }

    // wrap native wayland surface in a structure for EGL
    let egl_window = wl_egl_window_create(wl_surface_ptr, width, height);
    if egl_window.is_null() {
        bail!("wl_egl_window_create falló");
    }

    let egl_surface = eglCreateWindowSurface(egl_display, egl_config, egl_window, ptr::null());
    if egl_surface == EGL_NO_SURFACE {
        wl_egl_window_destroy(egl_window);
        bail!("eglCreateWindowSurface falló");
    }

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
        eglDestroySurface(egl_display, egl_surface);
        wl_egl_window_destroy(egl_window);
        bail!("eglCreateContext falló (requiere OpenGL >= 3.3)");
    }

    // bind context and surface to the execution thread
    if eglMakeCurrent(egl_display, egl_surface, egl_surface, egl_context) == 0 {
        eglDestroyContext(egl_display, egl_context);
        eglDestroySurface(egl_display, egl_surface);
        wl_egl_window_destroy(egl_window);
        bail!("eglMakeCurrent falló");
    }

    // no vsync forced at EGL level; mpv manages its own timing
    eglSwapInterval(egl_display, 0);

    info!("EGL initialized successfully ({}x{})", width, height);
    Ok((egl_display, egl_surface, egl_context, egl_window))
}
