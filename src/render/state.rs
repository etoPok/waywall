use std::os::raw::c_void;

use crate::bindings::egl::{
    eglDestroyContext, eglDestroySurface, eglMakeCurrent, eglTerminate, EGL_NO_CONTEXT,
    EGL_NO_DISPLAY, EGL_NO_SURFACE,
};
use crate::bindings::mpv::{mpv_render_context, mpv_render_context_free};
use crate::bindings::wayland_egl::wl_egl_window_destroy;

pub struct RenderState {
    pub egl_display: *mut c_void,
    pub egl_surface: *mut c_void,
    pub egl_context: *mut c_void,
    pub egl_window: *mut c_void,
    pub render_ctx: *mut mpv_render_context,
    pub width: i32,
    pub height: i32,
}

// SAFETY: accessed only from the main thread
unsafe impl Send for RenderState {}
unsafe impl Sync for RenderState {}

impl Drop for RenderState {
    fn drop(&mut self) {
        unsafe {
            if !self.render_ctx.is_null() {
                mpv_render_context_free(self.render_ctx);
            }
            eglMakeCurrent(
                self.egl_display,
                EGL_NO_SURFACE,
                EGL_NO_SURFACE,
                EGL_NO_CONTEXT,
            );
            if self.egl_surface != EGL_NO_SURFACE {
                eglDestroySurface(self.egl_display, self.egl_surface);
            }
            if self.egl_context != EGL_NO_CONTEXT {
                eglDestroyContext(self.egl_display, self.egl_context);
            }
            if self.egl_display != EGL_NO_DISPLAY {
                eglTerminate(self.egl_display);
            }
            if !self.egl_window.is_null() {
                wl_egl_window_destroy(self.egl_window);
            }
        }
    }
}
