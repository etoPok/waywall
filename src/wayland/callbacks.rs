use std::sync::atomic::{AtomicUsize, Ordering};

use tracing::debug;
use wayland_client::{protocol::wl_callback::WlCallback, Connection, Dispatch, Proxy, QueueHandle};

use crate::app::state::App;
use crate::bindings::egl::eglMakeCurrent;
use crate::render::frame::{has_new_frame, render_frame};

pub static WL_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);

impl Dispatch<WlCallback, ()> for App {
    fn event(
        state: &mut App,
        _proxy: &WlCallback,
        _event: <WlCallback as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<App>,
    ) {
        let wl_callback_count = WL_CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
        debug!(
            "WlCallback fired ({} total, {} pending)",
            wl_callback_count + 1,
            state.pending_wl_callbacks
        );

        state.pending_wl_callbacks = state.pending_wl_callbacks.saturating_sub(1);

        if state.pending_wl_callbacks == 0 {
            // All monitors have completed their frame callbacks.
            // Request the next round BEFORE rendering, so that
            // eglSwapBuffers (inside render_frame) commits the surface
            // including this request. Without a commit, the compositor ignores the
            // wl_callback and the frame loop dies.
            for monitor in state.monitors.iter_mut() {
                if let Some(surface) = &monitor.surface {
                    if let Some(qh) = &state.qh {
                        monitor.wl_callback = Some(surface.frame(qh, ()));
                    }
                }
            }
            state.pending_wl_callbacks = state.monitors.len();

            let has_new_frame = unsafe { has_new_frame(state.mpv_render_ctx) };
            unsafe {
                for rs in state.render_states.iter_mut() {
                    eglMakeCurrent(
                        rs.egl_display,
                        rs.egl_surface,
                        rs.egl_surface,
                        rs.egl_context,
                    );
                    if render_frame(rs, state.mpv_render_ctx, has_new_frame) {
                        state.frame_count += 1;
                    }
                }
            }
        }
    }
}
