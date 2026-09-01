use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "waywall=info".parse().unwrap()),
        )
        .init();

    let mut args = waywall::cli::args::parse();
    let bootstrap_output = waywall::app::bootstrap::bootstrap_drm_pipeline(&mut args)?;

    waywall::runtime::event_loop::run_drm(
        bootstrap_output.app,
        bootstrap_output.conn,
        bootstrap_output.queue,
        bootstrap_output.ping_source,
        bootstrap_output.error_ping_source,
    )
}
