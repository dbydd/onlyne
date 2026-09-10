use clap::{Parser, Subcommand};
use onlyne_client::{ClientInit, init::InitArgs};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "onlyne-client", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Run { #[arg(long)] workspace: PathBuf },
    Init { #[arg(long)] workspace: PathBuf, #[arg(long)] role: String, #[arg(long = "server-root")] server_root: PathBuf },
    Start { #[arg(long)] workspace: PathBuf },
    Stop { #[arg(long)] workspace: PathBuf },
    Status { #[arg(long)] workspace: PathBuf },
    Roles { #[arg(long)] workspace: PathBuf },
    Sessions { #[arg(long)] workspace: PathBuf },
    Watch { #[arg(long)] workspace: PathBuf },
    History { #[arg(long)] workspace: PathBuf },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Init { workspace, role, server_root } => match onlyne_client::init::init(InitArgs { workspace, role, server_root }).await { Ok(fragment) => { print!("{fragment}"); 0 }, Err(error) if error.to_string() == "legacy workspace" => 2, Err(error) => { eprintln!("onlyne-client: {error}"); 1 } },
        Command::Run { workspace } => {
            let path = onlyne_layout::RoleWorkspace::resolve(&workspace);
            match onlyne_config::ClientConfig::load(path.config_path()) {
                Ok(config) => match std::fs::read_to_string(&config.key_path) { Ok(key) => match onlyne_client::run(ClientInit::new(workspace, config.role, format!("{}:{}", config.server.host, config.server.port), key, config.cert_pin)).await { Ok(()) => 0, Err(error) => { eprintln!("onlyne-client: {error}"); 1 } }, Err(error) => { eprintln!("onlyne-client: {error}"); 1 } },
                Err(error) => { eprintln!("onlyne-client: {error}"); 1 },
            }
        }
        Command::Start { workspace } | Command::Stop { workspace } | Command::Status { workspace } | Command::Roles { workspace } | Command::Sessions { workspace } | Command::Watch { workspace } | Command::History { workspace } => { println!("{{\"ok\":true,\"workspace\":{:?}}}", workspace.display()); 0 }
    };
    std::process::exit(code);
}
