//! `onlyne-im`: run one IM gateway root, and lay one down.
//!
//! A gateway root is a directory tree, not a template tree: `init` writes its
//! document, `generate` mints one conversation workspace (a role's key, its
//! coordinates, its directories), `login` stores a platform credential, and
//! `run` serves every workspace under `conversations/` from one process. The
//! verb set is this binary's own — the IM gateway is an optional, separately
//! installed module, and `generate` here writes a conversation, not a cluster
//! role workspace (`docs/v2.2-IM-GATEWAY-PLAN.md` §7.1).
//!
//! `generate` has two halves and writes them in one run: the conversation
//! workspace on disk, and the cluster's `[[client]]` row plus both ACL lists
//! through the server's admin socket. The second half is what makes a
//! conversation reachable, so a `generate` that cannot reach the server refuses
//! before it mints a workspace, and an operator is told which remedy to take.

mod workspace;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "onlyne-im",
    version,
    about = "The IM gateway: conversations as roles"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a gateway root: its document, and the `conversations/` directory.
    Init(InitCmd),
    /// Mint one conversation workspace and register its role in the cluster.
    Generate(GenerateCmd),
    /// Store a platform credential under the gateway root.
    Login(LoginCmd),
    /// Serve every conversation under this gateway root.
    Run(RunCmd),
    /// Parse and print the gateway's configuration, with the roles it serves.
    /// Needs no server, and reads every conversation workspace.
    Check(CheckCmd),
}

#[derive(Debug, clap::Args)]
struct InitCmd {
    /// Gateway root directory.
    #[arg(long)]
    root: PathBuf,
    /// The server endpoint every conversation link dials, as `host:port`.
    /// Read from the server root's spec when `--server-root` is given instead.
    #[arg(long)]
    server: Option<String>,
    /// The server certificate's SPKI pin (`sha256/…`). Read from the server
    /// root's spec when `--server-root` is given instead.
    #[arg(long)]
    cert_pin: Option<String>,
    /// A server root to take the endpoint and pin from, instead of naming them.
    #[arg(long)]
    server_root: Option<PathBuf>,
    /// Overwrite an existing `config.toml`.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, clap::Args)]
struct GenerateCmd {
    /// Gateway root directory.
    #[arg(long)]
    root: PathBuf,
    /// The `[[im.backend]]` gateway id this conversation belongs to.
    #[arg(long)]
    gateway: String,
    /// The conversation id as the platform addresses it, e.g. a chat id.
    ///
    /// Telegram ids are negative for groups and channels, so a leading `-`
    /// is a value, not a flag.
    #[arg(long, allow_hyphen_values = true)]
    conversation: String,
    /// The role name; defaults to a name derived from the gateway and
    /// conversation ids. The directory name is the role.
    #[arg(long)]
    role: Option<String>,
    /// A role this conversation may address and be addressed by; repeat for
    /// several. Defaults to `_supervisor`, the cluster's operator agent.
    #[arg(long = "peer")]
    peers: Vec<String>,
    /// The server root whose `spec.toml` gains this role.
    #[arg(long)]
    server_root: Option<PathBuf>,
    /// The admin socket to apply the role through; overrides discovery.
    #[arg(long)]
    socket: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
struct LoginCmd {
    /// Gateway root directory.
    #[arg(long)]
    root: PathBuf,
    /// The `[[im.backend]]` gateway id this credential belongs to.
    #[arg(long)]
    gateway: String,
    /// The platform token. Prefer `--token-file` or an environment variable:
    /// an argument is visible in the process table.
    #[arg(long)]
    token: Option<String>,
    /// A file holding the token.
    #[arg(long)]
    token_file: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
struct RunCmd {
    /// Gateway root directory.
    #[arg(long)]
    root: PathBuf,
    /// The served socket a trigger script reaches as `ONLYNE_SOCKET`.
    #[arg(long)]
    socket: Option<String>,
}

#[derive(Debug, clap::Args)]
struct CheckCmd {
    /// Gateway root directory.
    #[arg(long)]
    root: PathBuf,
}

impl From<InitCmd> for workspace::InitArgs {
    fn from(cmd: InitCmd) -> Self {
        workspace::InitArgs {
            root: cmd.root,
            server: cmd.server,
            cert_pin: cmd.cert_pin,
            server_root: cmd.server_root,
            force: cmd.force,
        }
    }
}

impl From<GenerateCmd> for workspace::GenerateArgs {
    fn from(cmd: GenerateCmd) -> Self {
        workspace::GenerateArgs {
            root: cmd.root,
            gateway: cmd.gateway,
            conversation: cmd.conversation,
            role: cmd.role,
            peers: cmd.peers,
            server_root: cmd.server_root,
            socket: cmd.socket,
        }
    }
}

impl From<LoginCmd> for workspace::LoginArgs {
    fn from(cmd: LoginCmd) -> Self {
        workspace::LoginArgs {
            root: cmd.root,
            gateway: cmd.gateway,
            token: cmd.token,
            token_file: cmd.token_file,
        }
    }
}

#[tokio::main]
async fn main() {
    init_logging();
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Init(cmd) => report(workspace::init(&cmd.into())),
        Command::Generate(cmd) => report(workspace::generate(&cmd.into()).await),
        Command::Login(cmd) => report(workspace::login(&cmd.into())),
        Command::Run(cmd) => run(cmd.root, cmd.socket).await,
        Command::Check(cmd) => check(cmd.root),
    };
    std::process::exit(code);
}

/// A layout verb's answer as an exit code: 0 on success, 2 on a refusal an
/// operator can act on (a taken directory, an unreachable server, a bad flag
/// combination). Nothing here fails for a reason that would be a 1.
fn report(outcome: anyhow::Result<()>) -> i32 {
    match outcome {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("onlyne-im: {error:#}");
            2
        }
    }
}

async fn run(root: PathBuf, socket: Option<String>) -> i32 {
    let gateway = match onlyne_im::Gateway::load(&root) {
        Ok(gateway) => gateway,
        Err(error) => {
            eprintln!("onlyne-im: {error:#}");
            return 2;
        }
    };
    tracing::info!(
        root = %root.display(),
        conversations = gateway.conversations.len(),
        backends = gateway.backends.len(),
        "the IM gateway is starting"
    );
    let options = onlyne_im::RunOptions { socket };
    match onlyne_im::run(gateway, options).await {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("onlyne-im: {error:#}");
            1
        }
    }
}

fn check(root: PathBuf) -> i32 {
    match onlyne_im::Gateway::load(&root) {
        Ok(gateway) => {
            let conversations: Vec<serde_json::Value> = gateway
                .conversations
                .iter()
                .map(|conversation| {
                    serde_json::json!({
                        "role": conversation.role,
                        "gateway": conversation.gateway,
                        "conversation": conversation.conversation,
                        "workspace": conversation.workspace.display().to_string(),
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "root": root.display().to_string(),
                    "server": gateway.server,
                    "backends": gateway.backends.iter().map(|b| &b.gateway).collect::<Vec<_>>(),
                    "conversations": conversations,
                    "pushes": gateway.pushes.len(),
                })
            );
            0
        }
        Err(error) => {
            eprintln!("onlyne-im: {error:#}");
            2
        }
    }
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
