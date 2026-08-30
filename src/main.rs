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
mod vaapi_converter;
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
    let bootstrap_output = app::bootstrap::bootstrap_drm_pipeline(&mut args)?;

    runtime::event_loop::run_drm(
        bootstrap_output.app,
        bootstrap_output.conn,
        bootstrap_output.queue,
        bootstrap_output.ping_source,
        bootstrap_output.error_ping_source,
    )
}
