use std::os::raw::{c_int, c_void};
use std::ptr;

use crate::bindings::egl::eglSwapBuffers;
use crate::bindings::mpv::{
    mpv_render_context_render, mpv_render_context_report_swap, mpv_render_context_update,
    MpvOpenGLFbo, MpvRenderParam, MPV_RENDER_PARAM_FLIP_Y, MPV_RENDER_PARAM_INVALID,
    MPV_RENDER_PARAM_OPENGL_FBO, MPV_RENDER_UPDATE_FRAME,
};
use crate::render::state::RenderState;

/// Returns true if a frame was rendered, false if there was no new frame.
pub unsafe fn render_frame(rs: &mut RenderState) -> bool {
    let flags = mpv_render_context_update(rs.render_ctx);
    let has_frame = flags & MPV_RENDER_UPDATE_FRAME != 0;

    if has_frame {
        // fbo: 0 default framebuffer
        // mpv was never told about the EGL surface — it only received
        // get_proc_address to resolve OpenGL functions. The implicit
        // connection is: eglMakeCurrent (see egl.rs) bound the EGL surface
        // to this thread's OpenGL context, so the default FBO (0) on this
        // thread is always the back buffer of that EGL surface.
        let mut fbo = MpvOpenGLFbo {
            fbo: 0,
            w: rs.width,
            h: rs.height,
            internal_format: 0,
        };
        let mut flip_y: c_int = 1;

        let mut params = [
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_OPENGL_FBO,
                data: &mut fbo as *mut _ as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_FLIP_Y,
                data: &mut flip_y as *mut _ as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];

        mpv_render_context_render(rs.render_ctx, params.as_mut_ptr());
        eglSwapBuffers(rs.egl_display, rs.egl_surface);
        mpv_render_context_report_swap(rs.render_ctx);
    } else {
        // Swap without render to commit the Wayland surface.
        // Without a commit, wl_callback.frame() is never processed and the
        // frame callback loop dies.
        eglSwapBuffers(rs.egl_display, rs.egl_surface);
    }
    has_frame
}
