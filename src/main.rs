use anyhow::Result;

fn main() -> Result<()> {
    waywall::logging::init_prod();

    let args = waywall::cli::args::parse();
    let bootstrap_output = waywall::app::bootstrap::bootstrap(&args)?;

    waywall::runtime::event_loop::run(
        &args,
        bootstrap_output.app,
        bootstrap_output.conn,
        bootstrap_output.queue,
        bootstrap_output.ping_source,
        bootstrap_output.error_ping_source,
    )
}
