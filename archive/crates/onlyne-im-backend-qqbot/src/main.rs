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
    /// Talk to the sandbox environment.
    #[arg(long)]
    sandbox: bool,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let endpoint = if cli.sandbox {
        onlyne_im_backend_qqbot::Endpoint::Sandbox
    } else {
        onlyne_im_backend_qqbot::Endpoint::Production
    };
    onlyne_im_backend::init_backend_runtime!();
    onlyne_im_backend::exit_on_result(
        "onlyne-im-backend-qqbot",
        onlyne_im_backend_qqbot::run(cli.gateway, cli.token, endpoint),
    )
    .await
}
