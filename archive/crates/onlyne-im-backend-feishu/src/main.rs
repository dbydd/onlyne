//! `onlyne-im-backend-feishu`: a Feishu (Lark) platform backend for the IM
//! gateway.
//!
//! The gateway spawns this process and speaks the standard backend protocol
//! with it on stdio. Everything Feishu-specific lives here.

use clap::Parser;

#[derive(Parser)]
#[command(name = "onlyne-im-backend-feishu", version)]
struct Cli {
    /// The `[[gateway]]` id this backend serves, so an event names the right
    /// gateway.
    #[arg(long)]
    gateway: String,
    /// The credential as `app_id:app_secret`; falls back to the loader
    /// (credential file, then `FEISHU_APP_ID`/`FEISHU_APP_SECRET` joined with
    /// `:`).
    #[arg(long)]
    credential: Option<String>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let credential = cli
        .credential
        .or_else(|| std::env::var("FEISHU_CREDENTIAL").ok())
        .unwrap_or_default();
    onlyne_im_backend::init_backend_runtime!();
    onlyne_im_backend::exit_on_result(
        "onlyne-im-backend-feishu",
        onlyne_im_backend_feishu::run(cli.gateway, credential),
    )
    .await
}
