#![allow(dead_code)]

mod app;
mod cli;
mod decoder;
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

    let args = cli::args::parse();

    let output = app::bootstrap::bootstrap(args)?;

    runtime::event_loop::run(output.app, output.conn, output.queue, output.ping_source)
}
