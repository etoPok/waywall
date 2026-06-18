use std::os::raw::{c_char, c_void};
use std::ptr;

use anyhow::{bail, Result};
use libmpv2::Mpv;
use tracing::info;

use crate::bindings::egl::eglGetProcAddress;
use crate::bindings::mpv::{
    mpv_handle, mpv_render_context, mpv_render_context_create, MpvOpenGLInitParams, MpvRenderParam,
    MPV_RENDER_API_TYPE_OPENGL, MPV_RENDER_PARAM_API_TYPE, MPV_RENDER_PARAM_INVALID,
    MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
};

extern "C" fn get_proc_address(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    unsafe { eglGetProcAddress(name) }
}

pub unsafe fn create_render_context(mpv: &Mpv) -> Result<*mut mpv_render_context> {
    // libmpv2 exposes the native handle via the `ctx` field (NonNull<mpv_handle>)
    let mpv_handle_ptr = mpv.ctx.as_ptr() as *mut mpv_handle;

    let mut opengl_init_params = MpvOpenGLInitParams {
        get_proc_address,
        get_proc_address_ctx: ptr::null_mut(),
    };

    let api_type_ptr = MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *const c_char;

    let mut params = [
        MpvRenderParam {
            type_: MPV_RENDER_PARAM_API_TYPE,
            data: api_type_ptr as *mut c_void,
        },
        MpvRenderParam {
            type_: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
            data: &mut opengl_init_params as *mut _ as *mut c_void,
        },
        MpvRenderParam {
            type_: MPV_RENDER_PARAM_INVALID,
            data: ptr::null_mut(),
        },
    ];

    let mut render_ctx: *mut mpv_render_context = ptr::null_mut();
    let ret = mpv_render_context_create(&mut render_ctx, mpv_handle_ptr, params.as_mut_ptr());
    if ret < 0 {
        bail!("mpv_render_context_create falló con código {}", ret);
    }

    info!("mpv_render_context created successfully");
    Ok(render_ctx)
}
