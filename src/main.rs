#![allow(dead_code)]

mod app;
mod cli;
mod decoder;
mod drm_frame;
mod frame_queue;
mod notifier;
mod render;
mod runtime;
mod shader;
mod timing;
mod wayland;

use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "waywall=info".parse().unwrap()),
        )
        .init();

    let mut args = cli::args::parse();
    let mut app = app::bootstrap::bootstrap_wayland(&mut args)?;
    decoder::Decoder::start_vaapi_render_check(&mut app, &args.video_path, args.use_vaapi)

    // let output = app::bootstrap::bootstrap(&mut args)?;
    //
    // runtime::event_loop::run(
    //     output.app,
    //     output.conn,
    //     output.queue,
    //     output.ping_source,
    //     output.error_ping_source,
    // )
}
