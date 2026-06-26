use std::os::raw::c_void;
use std::ptr;
use std::time::Instant;

use calloop::LoopSignal;
use libmpv2::Mpv;
use wayland_client::protocol::{
    wl_callback::WlCallback, wl_compositor::WlCompositor, wl_output::WlOutput,
    wl_surface::WlSurface,
};
use wayland_client::QueueHandle;
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::ZwlrLayerShellV1, zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
};

use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use crate::bindings::mpv::{mpv_render_context, mpv_render_context_free};
use crate::render::state::RenderState;
use crate::runtime::wakeup::MpvUpdateState;

pub struct Monitor {
    pub name: Option<String>,
    pub output: Option<WlOutput>,
    pub surface: Option<WlSurface>,

    /// Pointer to the native wl_surface (for wl_egl_window_create).
    /// Obtained via wayland_backend::sys.
    pub wl_surface_ptr: *mut c_void,

    pub layer_surface: Option<ZwlrLayerSurfaceV1>,
    pub viewport: Option<WpViewport>,

    /// Wayland frame callback. MUST be kept alive; if dropped,
    /// wayland-client sends wl_proxy_destroy and the compositor cancels the callback.
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

    /// Wayland queue handle, needed to request frame callbacks.
    pub qh: Option<QueueHandle<App>>,

    /// EGL/mpv render state.
    pub render_states: Vec<RenderState>,

    pub mpv_render_ctx: *mut mpv_render_context,

    /// mpv instance.
    pub mpv: Option<Mpv>,

    /// true when mpv has a new frame ready to render.
    /// Raw pointer to the boxed MpvUpdateState; freed on cleanup.
    pub mpv_update_state: Option<*mut MpvUpdateState>,

    /// Number of wl_callback frames currently in-flight across all monitors.
    /// Decremented each time a callback fires. When it reaches 0, all monitors
    /// are ready for the next frame.
    pub pending_wl_callbacks: usize,

    /// First render attempt done (to render the first frame without depending on mpv_update_callback).
    pub first_render_attempted: bool,

    /// Rendered frame counter (for periodic stats).
    pub frame_count: u64,

    /// Timestamp of the last stats log.
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
            mpv_render_ctx: ptr::null_mut(),
            mpv: None,
            mpv_update_state: None,
            pending_wl_callbacks: 0,
            first_render_attempted: false,
            frame_count: 0,
            last_stats_time: None,
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        unsafe {
            if let Some(state_ptr) = self.mpv_update_state.take() {
                drop(Box::from_raw(state_ptr));
            }
            if !self.mpv_render_ctx.is_null() {
                mpv_render_context_free(self.mpv_render_ctx);
            }
        }

        self.render_states.clear();

        if let Some(mpv) = self.mpv.take() {
            drop(mpv);
        }
    }
}
