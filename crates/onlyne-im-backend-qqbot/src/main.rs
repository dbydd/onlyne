//! `onlyne-im-backend-qqbot`: a QQ Open Platform backend for the IM gateway.
//!
//! The gateway spawns this process and speaks the standard backend protocol
//! with it on stdio. Everything QQ-specific lives here.

use clap::Parser;

#[derive(Parser)]
#[command(name = "onlyne-im-backend-qqbot", version)]
struct Cli {
    /// The `[[gateway]]` id this backend serves, so an event names the right
    /// gateway.
    #[arg(long)]
    gateway: String,
    /// The app credentials as `QQBOT_APP_ID:QQBOT_APP_SECRET`; falls back to
    /// the credential file, then the environment.
    #[arg(long)]
    token: Option<String>,
    /// The API root, overridable for a fake in tests. Defaults to the real
    /// one; `--sandbox` is the documented shorthand for it.
    #[arg(long)]
    api_base: Option<String>,
    /// Talk to the sandbox environment.
    #[arg(long)]
    sandbox: bool,
}

#[tokio::main]
async fn main() {
    onlyne_im_backend::install_crypto_provider();
    init_logging();
    let cli = Cli::parse();
    let endpoint = if cli.sandbox {
        onlyne_im_backend_qqbot::Endpoint::Sandbox
    } else {
        onlyne_im_backend_qqbot::Endpoint::Production
    };
    let code =
        match onlyne_im_backend_qqbot::run(cli.gateway, cli.token, cli.api_base, endpoint).await {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("onlyne-im-backend-qqbot: {error:#}");
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
