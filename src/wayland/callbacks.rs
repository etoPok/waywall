use std::sync::atomic::{AtomicUsize, Ordering};

use tracing::debug;
use wayland_client::{protocol::wl_callback::WlCallback, Connection, Dispatch, Proxy, QueueHandle};

use crate::app::state::App;
use crate::render::frame::render_frame;

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
        debug!("WlCallback::event called {} times", wl_callback_count + 1);

        state.frame_pending = false;

        // Request the next frame callback BEFORE rendering, so that
        // eglSwapBuffers (inside render_frame) commits the surface
        // including this request. Without a commit, the compositor ignores the
        // wl_callback and the frame loop dies.
        if let Some(surface) = &state.surface {
            if let Some(qh) = &state.qh {
                state.wl_callback = Some(surface.frame(qh, ()));
                state.frame_pending = true;
            }
        }

        // render_frame ALWAYS calls eglSwapBuffers (with or without a frame),
        // which commits the surface and delivers the frame request to the server.
        // Internally it also calls mpv_render_context_update() which rearms
        // mpv_update_callback for the next frame.
        if let Some(rs) = &mut state.render_state {
            if unsafe { render_frame(rs) } {
                state.frame_count += 1;
            }
        }
    }
}
