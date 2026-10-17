//! `onlyne-im-backend-slack`: a Slack platform backend for the IM gateway.
//!
//! The gateway spawns this process and speaks the standard backend protocol
//! with it on stdio. Everything Slack-specific lives here.

use clap::Parser;

#[derive(Parser)]
#[command(name = "onlyne-im-backend-slack", version)]
struct Cli {
    /// The `[[gateway]]` id this backend serves, so an event names the right
    /// gateway.
    #[arg(long)]
    gateway: String,
    /// The credentials as `xapp-…:xoxb-…` (app-level token, colon, bot token);
    /// falls back to the credential file, then the environment.
    #[arg(long)]
    token: Option<String>,
    /// The Web API root; falls back to ONLYNE_SLACK_API_BASE. Defaults to
    /// Slack's own, and is what lets a local mock stand in for it.
    #[arg(long)]
    api_base: Option<String>,
}

#[tokio::main]
async fn main() {
    onlyne_im_backend::install_crypto_provider();
    init_logging();
    let cli = Cli::parse();
    let code = match onlyne_im_backend_slack::run(cli.gateway, cli.token, cli.api_base).await {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("onlyne-im-backend-slack: {error:#}");
            1
        }
    };
    std::process::exit(code);
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    // An explicit `RUST_LOG` wins over the shipped default; the default keeps
    // the SDK's socket client quiet, because its dial failures name the minted
    // URL and that URL carries a live ticket (`lib.rs`, `DEFAULT_LOG_FILTER`).
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(onlyne_im_backend_slack::DEFAULT_LOG_FILTER));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
