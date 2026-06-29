use std::os::raw::c_void;
use std::ptr;
use std::sync::Arc;
use std::time::Instant;

use calloop::LoopSignal;
use wayland_client::protocol::{
    wl_callback::WlCallback, wl_compositor::WlCompositor, wl_output::WlOutput,
    wl_surface::WlSurface,
};
use wayland_client::QueueHandle;
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::ZwlrLayerShellV1, zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
};

use crate::decoder::Decoder;
use crate::frame_queue::FrameQueue;
use crate::render::state::RenderState;
use crate::shader::{QuadGeometry, Shader};
use crate::timing::Timing;

pub struct Monitor {
    pub name: Option<String>,
    pub output: Option<WlOutput>,
    pub surface: Option<WlSurface>,

    /// Pointer to the native wl_surface (for wl_egl_window_create).
    pub wl_surface_ptr: *mut c_void,

    pub layer_surface: Option<ZwlrLayerSurfaceV1>,
    pub viewport: Option<WpViewport>,

    pub wl_callback: Option<WlCallback>,

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
            wl_callback: None,
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

pub struct App {
    pub compositor: WlCompositor,
    pub layer_shell: ZwlrLayerShellV1,
    pub viewporter: Option<WpViewporter>,

    pub monitors: Vec<Monitor>,
    pub loop_signal: Option<LoopSignal>,
    pub configured: bool,

    pub qh: Option<QueueHandle<App>>,

    pub render_states: Vec<RenderState>,

    // Decoder + Frame Queue
    pub decoder: Option<Decoder>,
    pub frame_queue: Arc<FrameQueue>,

    // Shaders + Geometry
    pub shader_yuv: Option<Shader>,
    pub shader_nv12: Option<Shader>,
    pub quad: Option<QuadGeometry>,

    // Timing
    pub timing: Option<Timing>,
    pub last_pts: Option<i64>,

    // Stats
    pub frame_count: u64,
    pub last_stats_time: Option<Instant>,
}

impl App {
    pub fn new(compositor: WlCompositor, layer_shell: ZwlrLayerShellV1) -> Self {
        Self {
            compositor,
            layer_shell,
            viewporter: None,
            monitors: Vec::new(),
            loop_signal: None,
            configured: false,
            qh: None,
            render_states: Vec::new(),
            decoder: None,
            frame_queue: Arc::new(FrameQueue::new()),
            shader_yuv: None,
            shader_nv12: None,
            quad: None,
            timing: None,
            last_pts: None,
            frame_count: 0,
            last_stats_time: None,
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(ref decoder) = self.decoder {
            decoder.stop();
        }
    }
}
