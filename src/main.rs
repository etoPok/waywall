use anyhow::Result;

fn main() -> Result<()> {
    waywall::logging::init_prod();

    let mut args = waywall::cli::args::parse();
    let bootstrap_output = waywall::app::bootstrap::bootstrap(&mut args)?;

    if args.use_hwdec {
        waywall::runtime::event_loop::run_drm(
            bootstrap_output.app,
            bootstrap_output.conn,
            bootstrap_output.queue,
            bootstrap_output.ping_source,
            bootstrap_output.error_ping_source,
        )
    } else {
        waywall::runtime::event_loop::run(
            bootstrap_output.app,
            bootstrap_output.conn,
            bootstrap_output.queue,
            bootstrap_output.ping_source,
            bootstrap_output.error_ping_source,
        )
    }
}
