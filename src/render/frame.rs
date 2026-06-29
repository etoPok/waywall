use ffmpeg_sys_next::{AVFrame, AVPixelFormat};
use gl::types::*;
use tracing::warn;

use crate::render::egl::{eglMakeCurrent, eglSwapBuffers};
use crate::render::state::RenderState;
use crate::shader::{QuadGeometry, Shader};

pub unsafe fn init_textures(_rs: &RenderState, frame: *mut AVFrame) -> Vec<GLuint> {
    let fmt = (*frame).format;
    let w = (*frame).width;
    let h = (*frame).height;

    if fmt == AVPixelFormat::AV_PIX_FMT_YUV420P as i32 {
        let mut textures: [GLuint; 3] = [0; 3];
        gl::GenTextures(3, textures.as_mut_ptr());

        for i in 0..3 {
            gl::BindTexture(gl::TEXTURE_2D, textures[i]);
            let (tw, th) = if i == 0 { (w, h) } else { (w / 2, h / 2) };
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::R8 as i32,
                tw,
                th,
                0,
                gl::RED,
                gl::UNSIGNED_BYTE,
                std::ptr::null(),
            );
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
        }
        textures.to_vec()
    } else if fmt == AVPixelFormat::AV_PIX_FMT_NV12 as i32 {
        let mut textures: [GLuint; 2] = [0; 2];
        gl::GenTextures(2, textures.as_mut_ptr());

        // Y plane
        gl::BindTexture(gl::TEXTURE_2D, textures[0]);
        gl::TexImage2D(
            gl::TEXTURE_2D,
            0,
            gl::R8 as i32,
            w,
            h,
            0,
            gl::RED,
            gl::UNSIGNED_BYTE,
            std::ptr::null(),
        );
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);

        // UV plane (interleaved)
        gl::BindTexture(gl::TEXTURE_2D, textures[1]);
        gl::TexImage2D(
            gl::TEXTURE_2D,
            0,
            gl::RG8 as i32,
            w / 2,
            h / 2,
            0,
            gl::RG,
            gl::UNSIGNED_BYTE,
            std::ptr::null(),
        );
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);

        textures.to_vec()
    } else {
        warn!("Unsupported pixel format: {}", fmt);
        Vec::new()
    }
}

pub unsafe fn upload_frame(textures: &[GLuint], frame: *mut AVFrame) {
    let fmt = (*frame).format;
    let w = (*frame).width;
    let h = (*frame).height;

    if fmt == AVPixelFormat::AV_PIX_FMT_YUV420P as i32 {
        for (i, &texture) in textures.iter().enumerate() {
            let data = (*frame).data[i];
            let stride = (*frame).linesize[i];
            if data.is_null() {
                continue;
            }
            let (tw, th) = if i == 0 { (w, h) } else { (w / 2, h / 2) };

            gl::BindTexture(gl::TEXTURE_2D, texture);
            gl::PixelStorei(gl::UNPACK_ROW_LENGTH, stride);
            gl::TexSubImage2D(
                gl::TEXTURE_2D,
                0,
                0,
                0,
                tw,
                th,
                gl::RED,
                gl::UNSIGNED_BYTE,
                data as *const _,
            );
            gl::PixelStorei(gl::UNPACK_ROW_LENGTH, 0);
        }
    } else if fmt == AVPixelFormat::AV_PIX_FMT_NV12 as i32 {
        // Y plane
        let y_data = (*frame).data[0];
        let y_stride = (*frame).linesize[0];
        if !y_data.is_null() {
            gl::BindTexture(gl::TEXTURE_2D, textures[0]);
            gl::PixelStorei(gl::UNPACK_ROW_LENGTH, y_stride);
            gl::TexSubImage2D(
                gl::TEXTURE_2D,
                0,
                0,
                0,
                w,
                h,
                gl::RED,
                gl::UNSIGNED_BYTE,
                y_data as *const _,
            );
            gl::PixelStorei(gl::UNPACK_ROW_LENGTH, 0);
        }

        // UV plane
        let uv_data = (*frame).data[1];
        let uv_stride = (*frame).linesize[1];
        if !uv_data.is_null() {
            gl::BindTexture(gl::TEXTURE_2D, textures[1]);
            gl::PixelStorei(gl::UNPACK_ROW_LENGTH, uv_stride / 2);
            gl::TexSubImage2D(
                gl::TEXTURE_2D,
                0,
                0,
                0,
                w / 2,
                h / 2,
                gl::RG,
                gl::UNSIGNED_BYTE,
                uv_data as *const _,
            );
            gl::PixelStorei(gl::UNPACK_ROW_LENGTH, 0);
        }
    } else {
        warn!("Cannot upload frame: unsupported pixel format");
    }
}

pub unsafe fn render_only(rs: &mut RenderState, shader: &Shader, quad: &QuadGeometry) {
    eglMakeCurrent(
        rs.egl_display,
        rs.egl_surface,
        rs.egl_surface,
        rs.egl_context,
    );

    gl::Viewport(0, 0, rs.width, rs.height);
    gl::ClearColor(0.0, 0.0, 0.0, 1.0);
    gl::Clear(gl::COLOR_BUFFER_BIT);

    shader.use_program();

    let num_textures = rs.textures.len();
    for i in 0..num_textures {
        gl::ActiveTexture(gl::TEXTURE0 + i as u32);
        gl::BindTexture(gl::TEXTURE_2D, rs.textures[i]);
    }

    quad.draw();

    eglSwapBuffers(rs.egl_display, rs.egl_surface);
}
