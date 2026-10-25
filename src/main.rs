use anyhow::Result;

fn main() -> Result<()> {
    waywall::logging::init_prod();

    let args = waywall::cli::args::parse();
    let bootstrap_output = waywall::app::bootstrap::bootstrap(&args)?;

    waywall::runtime::event_loop::run(&args, bootstrap_output)
}
