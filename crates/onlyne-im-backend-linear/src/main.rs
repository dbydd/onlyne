//! `onlyne-im-backend-linear`: a Linear platform backend for the IM gateway.
//!
//! The gateway spawns this process and speaks the standard backend protocol
//! with it on stdio. Everything Linear-specific lives here.

use clap::Parser;

#[derive(Parser)]
#[command(name = "onlyne-im-backend-linear", version)]
struct Cli {
    /// The `[[gateway]]` id this backend serves, so an event names the right
    /// gateway.
    #[arg(long)]
    gateway: String,
    /// The Linear API key; falls back to `LINEAR_API_KEY`.
    #[arg(long)]
    token: Option<String>,
    /// The GraphQL endpoint, overridable for a fake in tests. Defaults to the
    /// real one.
    #[arg(long)]
    api_base: Option<String>,
}

#[tokio::main]
async fn main() {
    onlyne_im_backend::install_crypto_provider();
    init_logging();
    let cli = Cli::parse();
    let token = cli
        .token
        .or_else(|| std::env::var(onlyne_im_backend_linear::API_KEY_ENV).ok())
        .unwrap_or_default();
    let code = match onlyne_im_backend_linear::run(cli.gateway, token, cli.api_base).await {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("onlyne-im-backend-linear: {error:#}");
            1
        }
    };
    std::process::exit(code);
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
