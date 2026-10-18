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

mod setup;
mod workspace;

use clap::{Parser, Subcommand};
use setup::SetupArgs;
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
    /// Store a platform credential under the gateway root, optionally
    /// authenticating a backend interactively and verifying it against the
    /// platform first.
    Login(LoginCmd),
    /// Guided setup: pick a backend, authenticate it, pick the conversations to
    /// serve, and bind each one as a role of the cluster.
    Setup(SetupCmd),
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
    /// The role this gateway is in the cluster. One gateway root is one role.
    #[arg(long)]
    role: Option<String>,
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
    /// The conversation's slug: the directory name under `conversations/`.
    /// Defaults to a name derived from the gateway and conversation ids. The
    /// slug names no identity — the chat key beside it does.
    #[arg(long = "slug", visible_alias = "role")]
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
    /// Ask the backend for the credential instead of taking it from a flag:
    /// it is verified against the platform and written only once accepted.
    #[arg(long)]
    interactive: bool,
}

#[derive(Debug, clap::Args)]
struct SetupCmd {
    /// Gateway root directory.
    #[arg(long)]
    root: PathBuf,
    /// The `[[im.backend]]` to set up; asked for when the root declares more
    /// than one.
    #[arg(long)]
    gateway: Option<String>,
    /// The credential, for a scripted run. Skipped when one is already stored
    /// and the platform accepts it.
    #[arg(long)]
    credential: Option<String>,
    /// A conversation id to bind; repeat for several. Asked for when omitted.
    #[arg(long = "conversation", allow_hyphen_values = true)]
    conversations: Vec<String>,
    /// A role each conversation may address and be addressed by; repeat for
    /// several. Defaults to `_supervisor`, the cluster's operator agent.
    #[arg(long = "peer")]
    peers: Vec<String>,
    /// The server root whose `spec.toml` gains the roles.
    #[arg(long)]
    server_root: Option<PathBuf>,
    /// The admin socket to apply the roles through; overrides discovery.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Answer with defaults and refuse anything that needs a human.
    #[arg(long)]
    non_interactive: bool,
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
            role: cmd.role,
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

impl From<SetupCmd> for setup::SetupArgs {
    fn from(cmd: SetupCmd) -> Self {
        setup::SetupArgs {
            root: cmd.root,
            gateway: cmd.gateway,
            credential: cmd.credential,
            conversations: cmd.conversations,
            peers: cmd.peers,
            server_root: cmd.server_root,
            socket: cmd.socket,
            non_interactive: cmd.non_interactive,
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

/// `login` is two verbs in one: a credential handed over on the command line is
/// written and done with, while `--interactive` (or `setup`) asks the backend
/// what it needs and verifies the answer against the platform before anything
/// is stored.
async fn login(cmd: LoginCmd) -> i32 {
    if !cmd.interactive {
        return report(workspace::login(&cmd.into()));
    }
    // The credential an interactive run starts from is whatever the flags
    // named, read now so the wizard verifies exactly what the operator passed
    // rather than a second copy read a moment later.
    let credential = match (&cmd.token, &cmd.token_file) {
        (Some(_), Some(_)) => {
            eprintln!("onlyne-im: pass `--token` or `--token-file`, not both");
            return 2;
        }
        (Some(token), None) => Some(token.clone()),
        (None, Some(path)) => match std::fs::read_to_string(path) {
            Ok(text) => Some(text.trim().to_string()),
            Err(error) => {
                eprintln!("onlyne-im: reading {}: {error}", path.display());
                return 2;
            }
        },
        (None, None) => None,
    };
    report(
        setup::login(&SetupArgs {
            root: cmd.root,
            gateway: Some(cmd.gateway),
            credential,
            conversations: Vec::new(),
            peers: Vec::new(),
            server_root: None,
            socket: None,
            non_interactive: false,
        })
        .await,
    )
}

#[tokio::main]
async fn main() {
    // Before any TLS: this process links rustls through both the core's net
    // layer and the platform backends' HTTP clients, and the two do not agree on
    // a provider (`onlyne_im_backend::install_crypto_provider`).
    onlyne_im_backend::install_crypto_provider();
    init_logging();
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Init(cmd) => report(workspace::init(&cmd.into())),
        Command::Generate(cmd) => report(workspace::generate(&cmd.into()).await),
        Command::Login(cmd) => login(cmd).await,
        Command::Setup(cmd) => report(setup::setup(&cmd.into()).await),
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
            if error
                .chain()
                .any(|e| e.is::<onlyne_im::config::NeedsMigration>())
            {
                onlyne_proto::EXIT_NEEDS_MIGRATION
            } else {
                2
            }
        }
    }
}

async fn run(root: PathBuf, socket: Option<String>) -> i32 {
    let gateway = match onlyne_im::Gateway::load(&root) {
        Ok(gateway) => gateway,
        Err(error) => {
            eprintln!("onlyne-im: {error:#}");
            return if error
                .chain()
                .any(|e| e.is::<onlyne_im::config::NeedsMigration>())
            {
                onlyne_proto::EXIT_NEEDS_MIGRATION
            } else {
                2
            };
        }
    };
    tracing::info!(
        root = %root.display(),
        conversations = gateway.conversations().len(),
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
                .conversations()
                .iter()
                .map(|conversation| {
                    serde_json::json!({
                        "slug": conversation.slug,
                        "chat": conversation.chat_key,
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
                    "role": gateway.role,
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
            if error
                .chain()
                .any(|e| e.is::<onlyne_im::config::NeedsMigration>())
            {
                onlyne_proto::EXIT_NEEDS_MIGRATION
            } else {
                2
            }
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
