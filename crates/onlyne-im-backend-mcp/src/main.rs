//! `onlyne-im-backend-mcp`: a meta-programmable MCP platform backend for the
//! Onlyne IM gateway.
//!
//! Many chat, workspace, and collaboration platforms ship an official or
//! community MCP server long before a Rust SDK exists. This binary spawns any
//! stdio MCP server and bridges its notifications, polling tools, send tools,
//! and conversation-listing tools into the standard `onlyne-im-backend`
//! protocol using a declarative TOML spec (`--spec <path>` or
//! `ONLYNE_IM_MCP_SPEC`).

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "onlyne-im-backend-mcp", version)]
struct Cli {
    /// The `[[im.backend]].gateway` id this backend serves.
    #[arg(long)]
    gateway: Option<String>,
    /// Path to the declarative MCP-IM mapping TOML file. Falls back to
    /// `ONLYNE_IM_MCP_SPEC`.
    #[arg(long)]
    spec: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let gateway = cli
        .gateway
        .or_else(|| std::env::var(onlyne_im_backend::GATEWAY_ENV).ok())
        .unwrap_or_else(|| "mcp".to_string());
    let spec = cli
        .spec
        .or_else(|| std::env::var("ONLYNE_IM_MCP_SPEC").ok().map(PathBuf::from));
    onlyne_im_backend::init_backend_runtime!();
    onlyne_im_backend::exit_on_result(
        "onlyne-im-backend-mcp",
        onlyne_im_backend_mcp::run(gateway, spec),
    )
    .await
}
