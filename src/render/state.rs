use std::os::raw::c_void;

use crate::render::egl::{
    eglDestroyContext, eglDestroySurface, eglMakeCurrent, eglTerminate, EGL_NO_CONTEXT,
    EGL_NO_DISPLAY, EGL_NO_SURFACE,
};
use crate::render::egl::{wl_egl_window_destroy, wl_egl_window_resize};

pub struct RenderState {
    pub egl_display: *mut c_void,
    pub egl_surface: *mut c_void,
    pub egl_context: *mut c_void,
    pub egl_window: *mut c_void,
    pub width: i32,
    pub height: i32,
    pub textures: Vec<gl::types::GLuint>,
}

unsafe impl Send for RenderState {}
unsafe impl Sync for RenderState {}

impl Drop for RenderState {
    fn drop(&mut self) {
        unsafe {
            if !self.textures.is_empty() {
                gl::DeleteTextures(self.textures.len() as i32, self.textures.as_ptr());
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

#[allow(dead_code)]
impl RenderState {
    pub fn resize(&mut self, width: i32, height: i32) {
        unsafe {
            wl_egl_window_resize(self.egl_window, width, height, 0, 0);
        }
        self.width = width;
        self.height = height;
    }
}
