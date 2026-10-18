//! `onlyne-im-backend-telegram`: a Telegram platform backend for the IM gateway.
//!
//! The gateway spawns this process and speaks the standard backend protocol with
//! it on stdio. Everything Telegram-specific lives here.

use clap::Parser;

#[derive(Parser)]
#[command(name = "onlyne-im-backend-telegram", version)]
struct Cli {
    /// The `[[gateway]]` id this backend serves, so an event names the right
    /// gateway.
    #[arg(long)]
    gateway: String,
    /// The bot token; falls back to `ONLYNE_TELEGRAM_TOKEN`.
    #[arg(long)]
    token: Option<String>,
    /// The API root, overridable for a fake in tests. Defaults to the real one.
    #[arg(long)]
    api_base: Option<String>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let token = cli
        .token
        .or_else(|| std::env::var("ONLYNE_TELEGRAM_TOKEN").ok())
        .unwrap_or_default();
    onlyne_im_backend::init_backend_runtime!();
    onlyne_im_backend::exit_on_result(
        "onlyne-im-backend-telegram",
        onlyne_im_backend_telegram::run(cli.gateway, token, cli.api_base),
    )
    .await
}
