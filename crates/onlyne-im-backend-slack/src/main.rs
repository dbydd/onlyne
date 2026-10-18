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
    let cli = Cli::parse();
    onlyne_im_backend::init_backend_runtime!(onlyne_im_backend_slack::DEFAULT_LOG_FILTER);
    onlyne_im_backend::exit_on_result(
        "onlyne-im-backend-slack",
        onlyne_im_backend_slack::run(cli.gateway, cli.token, cli.api_base),
    )
    .await
}
