use std::os::raw::c_void;

use crate::render::egl::{
    eglDestroyContext, eglDestroySurface, eglMakeCurrent, EGL_NO_CONTEXT, EGL_NO_SURFACE,
};
use crate::render::egl::{wl_egl_window_destroy, wl_egl_window_resize};
use crate::shader::{QuadGeometry, Shader};

pub struct RenderState {
    pub egl_display: *mut c_void,
    pub egl_surface: *mut c_void,
    pub egl_window: *mut c_void,
    pub width: i32,
    pub height: i32,
}

impl Drop for RenderState {
    fn drop(&mut self) {
        unsafe {
            if !self.egl_window.is_null() {
                wl_egl_window_destroy(self.egl_window);
            }

            if self.egl_surface != EGL_NO_SURFACE {
                eglDestroySurface(self.egl_display, self.egl_surface);
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

pub struct GlContext {
    pub egl_display: *mut c_void,
    pub egl_ctx: *mut c_void,
    pub textures: Vec<gl::types::GLuint>,
    pub shader_yuv: Shader,
    pub shader_nv12: Shader,
    pub quad: QuadGeometry,
}

impl GlContext {
    pub fn new(egl_display: *mut c_void, egl_ctx: *mut c_void) -> Self {
        let shader_yuv = Shader::new_yuv420p();
        let shader_nv12 = Shader::new_nv12();
        let quad = QuadGeometry::new();

        Self {
            egl_display,
            egl_ctx,
            shader_yuv,
            shader_nv12,
            quad,
            textures: Vec::new(),
        }
    }
}

impl Drop for GlContext {
    fn drop(&mut self) {
        unsafe {
            eglMakeCurrent(
                self.egl_display,
                EGL_NO_SURFACE,
                EGL_NO_SURFACE,
                self.egl_ctx,
            );

            if !self.textures.is_empty() {
                gl::DeleteTextures(self.textures.len() as i32, self.textures.as_ptr());
            }
            self.shader_nv12.destroy();
            self.shader_yuv.destroy();
            self.quad.destroy();

            eglMakeCurrent(
                self.egl_display,
                EGL_NO_SURFACE,
                EGL_NO_SURFACE,
                EGL_NO_CONTEXT,
            );
            eglDestroyContext(self.egl_display, self.egl_ctx);
        }
    }
}
