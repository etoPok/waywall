use tracing_subscriber::EnvFilter;

const DEFAULT_PROD_FILTER: &str = "waywall=info";
const DEFAULT_TEST_FILTER: &str = "waywall=debug";

fn init_with_default(default: &str) {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default.parse().expect("invalid default log filter")),
        )
        .init();
}

pub fn init_prod() {
    init_with_default(DEFAULT_PROD_FILTER);
}

pub fn init_test() {
    init_with_default(DEFAULT_TEST_FILTER);
}

pub fn init(default: &str) {
    init_with_default(default);
}
