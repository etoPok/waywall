//! mpv-wallpaper
//!
//! Renders a video as an animated wallpaper on Wayland/Hyprland.
//!
//! Correct architecture (Wayland-native, no X11 hacks):
//!   1. Creates a wl_surface + zwlr_layer_surface_v1 (layer: BACKGROUND, fullscreen).
//!   2. Creates a wl_egl_window on the surface.
//!   3. Initializes EGL: EGLDisplay → EGLContext → EGLSurface.
//!   4. Initializes mpv_render_context (MPV_RENDER_API_TYPE_OPENGL) — mpv does NOT open any window.
//!   5. Loop: processes Wayland + mpv events, renders frames with mpv_render_context_render,
//!      presents with eglSwapBuffers.
//!
//! Usage:
//!   mpv-wallpaper /path/to/video.mp4

mod app;
mod bindings;
mod cli;
mod mpv;
mod render;
mod runtime;
mod wayland;

use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mpv_wallpaper=info".parse().unwrap()),
        )
        .init();

    let args = cli::args::parse();

    let output = app::bootstrap::bootstrap(args)?;

    runtime::event_loop::run(output.app, output.conn, output.queue, output.ping_source, output.render_ctx)
}
