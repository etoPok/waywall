use std::ffi::CString;
use std::ptr;

use gl::types::*;

const VERTEX_SRC: &str = r##"#version 330 core
layout(location = 0) in vec2 pos;
layout(location = 1) in vec2 tex_coord;
out vec2 tex_uv;
void main() {
    gl_Position = vec4(pos, 0.0, 1.0);
    tex_uv = tex_coord;
}
"##;

const FRAG_YUV420P_SRC: &str = r##"#version 330 core
uniform sampler2D y_tex;
uniform sampler2D u_tex;
uniform sampler2D v_tex;
in vec2 tex_uv;
out vec4 frag;
void main() {
    float y = texture(y_tex, tex_uv).r;
    float u = texture(u_tex, tex_uv).r - 0.5;
    float v = texture(v_tex, tex_uv).r - 0.5;
    // BT.709
    frag = vec4(
        y + 1.5748 * v,
        y - 0.1873 * u - 0.4681 * v,
        y + 1.8556 * u,
        1.0
    );
}
"##;

const FRAG_NV12_SRC: &str = r##"#version 330 core
uniform sampler2D y_tex;
uniform sampler2D uv_tex;
in vec2 tex_uv;
out vec4 frag;
void main() {
    float y = texture(y_tex, tex_uv).r;
    float u = texture(uv_tex, tex_uv).r - 0.5;
    float v = texture(uv_tex, tex_uv).g - 0.5;
    // BT.709
    frag = vec4(
        y + 1.5748 * v,
        y - 0.1873 * u - 0.4681 * v,
        y + 1.8556 * u,
        1.0
    );
}
"##;

pub struct Shader {
    pub program: GLuint,
    pub y_tex_loc: GLint,
    pub u_tex_loc: GLint,
    pub v_tex_loc: GLint,
    pub uses_uv: bool,
}

impl Shader {
    fn compile(source: &str, shader_type: GLenum) -> GLuint {
        let shader = unsafe { gl::CreateShader(shader_type) };
        let c_str = CString::new(source).unwrap();
        unsafe {
            gl::ShaderSource(shader, 1, &c_str.as_ptr(), ptr::null());
            gl::CompileShader(shader);

            let mut status: GLint = 0;
            gl::GetShaderiv(shader, gl::COMPILE_STATUS, &mut status);
            if status == 0 {
                let mut log_len: GLint = 0;
                gl::GetShaderiv(shader, gl::INFO_LOG_LENGTH, &mut log_len);
                let mut log: Vec<u8> = Vec::with_capacity(log_len as usize);
                gl::GetShaderInfoLog(shader, log_len, &mut log_len, log.as_mut_ptr() as *mut _);
                log.set_len(log_len as usize);
                let msg = String::from_utf8_lossy(&log);
                panic!("Shader compile error: {}", msg);
            }
        }
        shader
    }

    pub fn new_yuv420p() -> Self {
        Self::new(VERTEX_SRC, FRAG_YUV420P_SRC, false)
    }

    pub fn new_nv12() -> Self {
        Self::new(VERTEX_SRC, FRAG_NV12_SRC, true)
    }

    fn new(vertex_src: &str, frag_src: &str, uses_uv: bool) -> Self {
        let vs = Self::compile(vertex_src, gl::VERTEX_SHADER);
        let fs = Self::compile(frag_src, gl::FRAGMENT_SHADER);

        let program = unsafe { gl::CreateProgram() };
        unsafe {
            gl::AttachShader(program, vs);
            gl::AttachShader(program, fs);
            gl::LinkProgram(program);

            let mut status: GLint = 0;
            gl::GetProgramiv(program, gl::LINK_STATUS, &mut status);
            if status == 0 {
                let mut log_len: GLint = 0;
                gl::GetProgramiv(program, gl::INFO_LOG_LENGTH, &mut log_len);
                let mut log: Vec<u8> = Vec::with_capacity(log_len as usize);
                gl::GetProgramInfoLog(program, log_len, &mut log_len, log.as_mut_ptr() as *mut _);
                log.set_len(log_len as usize);
                let msg = String::from_utf8_lossy(&log);
                panic!("Program link error: {}", msg);
            }

            gl::DeleteShader(vs);
            gl::DeleteShader(fs);
        }

        let y_tex_loc =
            unsafe { gl::GetUniformLocation(program, CString::new("y_tex").unwrap().as_ptr()) };
        let u_tex_loc =
            unsafe { gl::GetUniformLocation(program, CString::new("u_tex").unwrap().as_ptr()) };
        let v_tex_loc =
            unsafe { gl::GetUniformLocation(program, CString::new("v_tex").unwrap().as_ptr()) };

        Self {
            program,
            y_tex_loc,
            u_tex_loc,
            v_tex_loc,
            uses_uv,
        }
    }

    pub fn use_program(&self) {
        unsafe {
            gl::UseProgram(self.program);
            gl::Uniform1i(self.y_tex_loc, 0);
            gl::Uniform1i(self.u_tex_loc, 1);
            if !self.uses_uv {
                gl::Uniform1i(self.v_tex_loc, 2);
            }
        }
    }

    pub fn destroy(&self) {
        unsafe {
            gl::DeleteProgram(self.program);
        }
    }
}

pub struct QuadGeometry {
    pub vao: GLuint,
    pub vbo: GLuint,
}

impl Default for QuadGeometry {
    fn default() -> Self {
        Self::new()
    }
}

impl QuadGeometry {
    pub fn new() -> Self {
        #[rustfmt::skip]
        let vertices: [f32; 16] = [
            // pos         tex (Y flipped for FFmpeg → OpenGL)
            -1.0, -1.0,    0.0, 1.0,
             1.0, -1.0,    1.0, 1.0,
             1.0,  1.0,    1.0, 0.0,
            -1.0,  1.0,    0.0, 0.0,
        ];

        let mut vao: GLuint = 0;
        let mut vbo: GLuint = 0;
        unsafe {
            gl::GenVertexArrays(1, &mut vao);
            gl::GenBuffers(1, &mut vbo);

            gl::BindVertexArray(vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (vertices.len() * std::mem::size_of::<f32>()) as isize,
                vertices.as_ptr() as *const _,
                gl::STATIC_DRAW,
            );

            // pos (location=0): 2 floats
            gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, 4 * 4, ptr::null());
            gl::EnableVertexAttribArray(0);

            // tex_coord (location=1): 2 floats, offset 8 bytes
            gl::VertexAttribPointer(
                1,
                2,
                gl::FLOAT,
                gl::FALSE,
                4 * 4,
                (2 * std::mem::size_of::<f32>()) as *const _,
            );
            gl::EnableVertexAttribArray(1);

            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
            gl::BindVertexArray(0);
        }

        Self { vao, vbo }
    }

    pub fn draw(&self) {
        unsafe {
            gl::BindVertexArray(self.vao);
            gl::DrawArrays(gl::TRIANGLE_FAN, 0, 4);
            gl::BindVertexArray(0);
        }
    }

    pub fn destroy(&self) {
        unsafe {
            gl::DeleteVertexArrays(1, &self.vao);
            gl::DeleteBuffers(1, &self.vbo);
        }
    }
}
