use std::os::raw::c_void;
use std::ptr;
use std::sync::Arc;
use std::time::Instant;

use calloop::LoopSignal;
use ffmpeg_sys_next::{AVFrame, av_frame_free, av_frame_unref};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::{
    wl_compositor::WlCompositor, wl_output::WlOutput, wl_surface::WlSurface,
};
use wayland_client::{Connection, QueueHandle};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::{
    Flags, ZwpLinuxBufferParamsV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::ZwlrLayerShellV1, zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
};

use crate::decoder::Decoder;
use crate::drm_frame::DrmFrame;
use crate::frame_queue::FrameQueue;
use crate::render::egl::eglTerminate;
use crate::render::state::{GlContext, RenderState};
use crate::timing::Timing;
use crate::vaapi_converter::VaapiConverter;

pub struct Monitor {
    pub name: Option<String>,
    pub output: Option<WlOutput>,
    pub surface: Option<WlSurface>,

    /// Pointer to the native wl_surface (for wl_egl_window_create).
    pub wl_surface_ptr: *mut c_void,

    pub layer_surface: Option<ZwlrLayerSurfaceV1>,
    pub viewport: Option<WpViewport>,

    pub physical_width: u32,
    pub physical_height: u32,
    pub logical_width: u32,
    pub logical_height: u32,
    pub configured: bool,
}

impl Monitor {
    pub fn new(output: WlOutput) -> Self {
        Self {
            name: None,
            output: Some(output),
            surface: None,
            wl_surface_ptr: ptr::null_mut(),
            layer_surface: None,
            viewport: None,
            physical_width: 0,
            physical_height: 0,
            logical_width: 0,
            logical_height: 0,
            configured: false,
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        if let Some(ls) = self.layer_surface.take() {
            ls.destroy();
        }
        if let Some(s) = self.surface.take() {
            s.destroy();
        }
        if let Some(vp) = self.viewport.take() {
            vp.destroy();
        }
    }
}

pub struct WlBufferState {
    pub wl_buffer: Option<WlBuffer>,
    pub bgra_frame: *mut AVFrame,
    pub drm_frame: *mut AVFrame,
    pub drm_frame_wrapper: DrmFrame,
    pub params: Option<ZwpLinuxBufferParamsV1>,
    pub in_use: bool,
}

impl Default for WlBufferState {
    fn default() -> Self {
        Self::new()
    }
}

impl WlBufferState {
    pub fn new() -> Self {
        Self {
            wl_buffer: None,
            bgra_frame: std::ptr::null_mut(),
            drm_frame: std::ptr::null_mut(),
            drm_frame_wrapper: DrmFrame::new(),
            params: None,
            in_use: false,
        }
    }
}

impl Drop for WlBufferState {
    fn drop(&mut self) {
        unsafe {
            if !self.drm_frame.is_null() {
                av_frame_free(&mut self.drm_frame);
            }
            if !self.bgra_frame.is_null() {
                av_frame_free(&mut self.bgra_frame);
            }
            if let Some(p) = &mut self.params {
                p.destroy();
            }
        }
    }
}

pub struct App {
    pub conn: Connection,
    pub qh: QueueHandle<App>,
    pub compositor: WlCompositor,
    pub layer_shell: ZwlrLayerShellV1,
    pub viewporter: Option<WpViewporter>,
    pub wl_display: *mut c_void,

    // DMA-BUF
    pub dmabuf: Option<ZwpLinuxDmabufV1>,
    pub dmabuf_main_device: Option<Vec<u8>>,
    pub wl_buffer_states: [Option<WlBufferState>; 3],
    pub converter: Option<VaapiConverter>,

    pub monitors: Vec<Monitor>,
    pub main_loop_signal: Option<LoopSignal>,
    pub configured: bool,

    pub render_states: Vec<RenderState>,

    // Decoder + Frame Queue
    pub decoder: Decoder,
    pub frame_queue: Arc<FrameQueue>,

    // shaders + geometry + egl_ctx
    pub gl_ctx: Option<GlContext>,

    // Timing
    pub timing: Timing,
    pub last_pts: Option<i64>,

    // Stats
    pub committed_frames: u64,
    pub dropped_frames: u64,
    pub frames_per_stats_sample: u64,
    pub last_stats_time: Option<Instant>,
}

impl App {
    pub fn new(
        conn: Connection,
        qh: QueueHandle<App>,
        compositor: WlCompositor,
        layer_shell: ZwlrLayerShellV1,
        wl_display: *mut c_void,
        viewporter: Option<WpViewporter>,
        dmabuf: Option<ZwpLinuxDmabufV1>,
        decoder: Decoder,
        timing: Timing,
    ) -> Self {
        Self {
            conn,
            qh,
            compositor,
            layer_shell,
            viewporter,
            wl_display,
            dmabuf,
            decoder,
            timing,
            dmabuf_main_device: None,
            wl_buffer_states: std::array::from_fn(|_| None),
            converter: None,
            monitors: Vec::new(),
            main_loop_signal: None,
            configured: false,
            render_states: Vec::new(),
            frame_queue: Arc::new(FrameQueue::new()),
            gl_ctx: None,
            last_pts: None,
            committed_frames: 0,
            dropped_frames: 0,
            frames_per_stats_sample: 0,
            last_stats_time: None,
        }
    }

    pub fn get_or_create_wl_buffer(
        &mut self,
        vaapi_frame: &mut AVFrame,
    ) -> anyhow::Result<Option<&WlBufferState>> {
        let Some(wl_buffer_state) = self
            .wl_buffer_states
            .iter_mut()
            .find(|s| s.as_ref().is_some_and(|wbs| !wbs.in_use) || s.is_none())
        else {
            return Ok(None);
        };

        if wl_buffer_state.is_some() {
            let wbs = wl_buffer_state.as_mut().unwrap();

            unsafe {
                if !wbs.bgra_frame.is_null() {
                    av_frame_unref(wbs.bgra_frame);
                }
                if !wbs.drm_frame.is_null() {
                    av_frame_unref(wbs.drm_frame);
                }
            }

            if let Some(old_buf) = wbs.wl_buffer.take() {
                old_buf.destroy();
            }
            if let Some(old_params) = wbs.params.take() {
                old_params.destroy();
            }
        } else {
            wl_buffer_state.replace(WlBufferState::new());
        }

        let converter = self
            .converter
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("VaapiConverter not initialized"))?;

        let wbs = wl_buffer_state.as_mut().unwrap();
        unsafe { converter.convert(vaapi_frame as *mut AVFrame, &mut wbs.bgra_frame) }?;
        unsafe {
            wbs.drm_frame_wrapper
                .map(wbs.bgra_frame, &mut wbs.drm_frame)
        }?;

        let params = self.dmabuf.as_ref().unwrap().create_params(&self.qh, ());

        let mut plane_idx = 0u32;
        for layer in wbs.drm_frame_wrapper.layers.iter() {
            for plane in layer.planes.iter() {
                let borrowed_fd = unsafe { std::os::unix::io::BorrowedFd::borrow_raw(plane.fd) };
                params.add(
                    borrowed_fd,
                    plane_idx,
                    plane.offset,
                    plane.stride,
                    plane.modifier_hi,
                    plane.modifier_lo,
                );
                plane_idx += 1;
            }
        }

        let wl_buffer = params.create_immed(
            wbs.drm_frame_wrapper.width,
            wbs.drm_frame_wrapper.height,
            wbs.drm_frame_wrapper.format,
            Flags::empty(),
            &self.qh,
            (),
        );

        wbs.wl_buffer = Some(wl_buffer);
        wbs.params = Some(params);
        wbs.in_use = true;

        Ok(wl_buffer_state.as_ref())
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.render_states.clear();

        if let Some(gl) = self.gl_ctx.take() {
            drop(gl);
        }

        if !self.wl_display.is_null() {
            unsafe {
                eglTerminate(self.wl_display);
            }
        }
    }
}
