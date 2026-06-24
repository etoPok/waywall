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
                .unwrap_or_else(|_| "mpvwall=info".parse().unwrap()),
        )
        .init();

    let args = cli::args::parse();

    let output = app::bootstrap::bootstrap(args)?;

    runtime::event_loop::run(output.app, output.conn, output.queue, output.ping_source)
}
