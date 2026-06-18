use std::os::raw::c_void;
use std::ptr;
use std::time::Instant;

use calloop::LoopSignal;
use libmpv2::Mpv;
use wayland_client::protocol::{
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_output::WlOutput,
    wl_surface::WlSurface,
};
use wayland_client::QueueHandle;
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::ZwlrLayerShellV1,
    zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
};

use crate::render::state::RenderState;
use crate::runtime::wakeup::MpvUpdateState;

pub struct App {
    pub compositor: WlCompositor,
    pub layer_shell: ZwlrLayerShellV1,

    pub surface: Option<WlSurface>,
    pub layer_surface: Option<ZwlrLayerSurfaceV1>,

    pub width: u32,
    pub height: u32,

    pub output: Option<WlOutput>,
    pub loop_signal: Option<LoopSignal>,
    pub configured: bool,

    /// Pointer to the native wl_surface (for wl_egl_window_create).
    /// Obtained via wayland_backend::sys.
    pub wl_surface_ptr: *mut c_void,

    /// Wayland queue handle, needed to request frame callbacks.
    pub qh: Option<QueueHandle<App>>,

    /// EGL/mpv render state.
    pub render_state: Option<RenderState>,

    /// mpv instance.
    pub mpv: Option<Mpv>,

    /// true when mpv has a new frame ready to render.
    /// Raw pointer to the boxed MpvUpdateState; freed on cleanup.
    pub mpv_update_state: Option<*mut MpvUpdateState>,

    /// true when a wl_callback frame has been requested and has not fired yet.
    pub frame_pending: bool,

    /// Wayland frame callback. MUST be kept alive; if dropped,
    /// wayland-client sends wl_proxy_destroy and the compositor cancels the callback.
    pub wl_callback: Option<WlCallback>,

    /// First frame already rendered (to avoid rendering twice).
    pub first_frame_rendered: bool,

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
            surface: None,
            layer_surface: None,
            width: 0,
            height: 0,
            output: None,
            loop_signal: None,
            configured: false,
            wl_surface_ptr: ptr::null_mut(),
            qh: None,
            render_state: None,
            mpv: None,
            mpv_update_state: None,
            frame_pending: false,
            wl_callback: None,
            first_frame_rendered: false,
            first_render_attempted: false,
            frame_count: 0,
            last_stats_time: None,
        }
    }
}
