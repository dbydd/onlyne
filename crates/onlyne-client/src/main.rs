use clap::{Parser, Subcommand};
use onlyne_client::{
    ClientInit, daemon, init::InitArgs, intent::IntentMachine, local_cli, local_cli::LocalCli,
};
use onlyne_proto::QueryRolesArgs;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "onlyne-client", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        workspace: PathBuf,
    },
    Init {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        role: String,
        #[arg(long = "server-root")]
        server_root: PathBuf,
        #[arg(
            long,
            default_value = "",
            help = "Role control plane text; spec.toml holds it per plan §5 and the client caches it from welcome into prose_cache"
        )]
        prose: String,
    },
    Start {
        #[arg(long)]
        workspace: PathBuf,
    },
    Stop {
        #[arg(long)]
        workspace: PathBuf,
    },
    Status {
        #[arg(long)]
        workspace: PathBuf,
    },
    Roles {
        #[arg(long)]
        workspace: PathBuf,
    },
    Sessions {
        #[arg(long)]
        workspace: PathBuf,
    },
    Watch {
        #[arg(long)]
        workspace: PathBuf,
    },
    History {
        #[arg(long)]
        workspace: PathBuf,
    },
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
}

#[derive(Subcommand)]
enum AgentCommand {
    /// Install a plugin package directory or `.tar.gz` into the workspace.
    Install {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        package: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long)]
        agent: Option<String>,
    },
    /// Remove an installed plugin package and its config entry.
    Uninstall {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        id: String,
    },
}

#[tokio::main]
async fn main() {
    init_logging();
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Init {
            workspace,
            role,
            server_root,
            prose,
        } => match onlyne_client::init::init(InitArgs {
            workspace,
            role,
            server_root,
            prose,
        })
        .await
        {
            Ok(fragment) => {
                print!("{fragment}");
                0
            }
            Err(error) if error.to_string() == "legacy workspace" => 2,
            Err(error) => {
                eprintln!("onlyne-client: {error}");
                1
            }
        },
        Command::Run { workspace } => {
            let path = onlyne_layout::RoleWorkspace::resolve(&workspace);
            match onlyne_config::ClientConfig::load(path.config_path()) {
                Ok(config) => match onlyne_client::run(
                    ClientInit::new(
                        workspace,
                        config.role,
                        format!("{}:{}", config.server.host, config.server.port),
                        // A generated workspace stores `key_path` relative to
                        // `.onlyne` so the tree stays valid wherever it is moved
                        // (plan §11 line 391). `init` writes an absolute path, which
                        // reaches the same file unchanged.
                        path.resolve_key_path(&config.key_path),
                        config.cert_pin,
                    )
                    .with_orca_worktree(config.orca.worktree),
                )
                .await
                {
                    Ok(()) => 0,
                    Err(error) => {
                        eprintln!("onlyne-client: {error}");
                        1
                    }
                },
                Err(error) => {
                    eprintln!("onlyne-client: {error}");
                    1
                }
            }
        }
        Command::Start { workspace } => match daemon::start(&workspace) {
            Ok(pid) => {
                println!(
                    "{}",
                    daemon::start_line(pid, &daemon::socket_file(&workspace))
                );
                0
            }
            Err(error) => {
                eprintln!("onlyne-client: {error:#}");
                1
            }
        },
        Command::Stop { workspace } => stop_client(&workspace),
        Command::Status { workspace } => match daemon::status(&workspace).await {
            Ok(Some(report)) => {
                println!("{}", report.line());
                if !report.connected {
                    eprintln!("{}", daemon::NOT_CONNECTED);
                }
                report.exit_code()
            }
            Ok(None) => {
                eprintln!("{}", daemon::NOT_RUNNING);
                2
            }
            Err(error) => {
                eprintln!("onlyne-client: {error:#}");
                1
            }
        },
        Command::Roles { workspace } => local_roles(&workspace),
        Command::Sessions { .. } | Command::Watch { .. } | Command::History { .. } => {
            eprintln!(
                "onlyne: this verb needs the live role runtime; v1.0.0 has no local query socket"
            );
            1
        }
        Command::Agent { command } => match command {
            AgentCommand::Install {
                workspace,
                package,
                id,
                agent,
            } => plugin_verb(
                local_cli::install_verb(&workspace, &package, &id, agent.as_deref()).await,
            ),
            AgentCommand::Uninstall { workspace, id } => {
                plugin_verb(local_cli::uninstall_verb(&workspace, &id).await)
            }
        },
    };
    std::process::exit(code);
}

/// Route `tracing` output to stderr, which `start` redirects into
/// `.onlyne/logs/client.log` (plan §2).
///
/// A process that already installed a subscriber keeps it, so a test harness
/// can capture its own spans.
fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Print one line per plugin action and hint on stderr when no client ran.
fn plugin_verb(result: anyhow::Result<local_cli::PluginAction>) -> i32 {
    match result {
        Ok(action) => {
            for line in &action.lines {
                println!("{line}");
            }
            if let Some(hint) = &action.hint {
                eprintln!("{hint}");
            }
            0
        }
        Err(error) => {
            eprintln!("{error}");
            local_cli::plugin_exit_code(&error)
        }
    }
}

/// Stop the recorded client and answer with the verb's exit code.
fn stop_client(workspace: &Path) -> i32 {
    match daemon::stop(workspace) {
        Ok(outcome) => {
            if let Some(line) = outcome.line() {
                println!("{line}");
            }
            if outcome.is_refusal() {
                eprintln!("{}", daemon::NOT_RUNNING);
            }
            outcome.exit_code()
        }
        Err(error) => {
            eprintln!("onlyne-client: {error:#}");
            1
        }
    }
}

/// Answer the role query from the prose cache in `client.db`.
fn local_roles(workspace: &Path) -> i32 {
    let layout = onlyne_layout::RoleWorkspace::resolve(workspace);
    if !layout.client_db_path().exists() {
        println!("{}", serde_json::json!({"ok": true, "data": {"roles": []}}));
        return 0;
    }
    let store = match onlyne_store::ClientStore::open(layout.client_db_path()) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("onlyne-client: {error}");
            return 1;
        }
    };
    let cli = LocalCli::new(IntentMachine::new(store, 0, Vec::new()));
    match cli.query_roles_local(&QueryRolesArgs { role: None }) {
        Ok(body) => {
            println!("{}", serde_json::json!({"ok": body.ok, "data": body.data}));
            0
        }
        Err(error) => {
            eprintln!("onlyne-client: {error}");
            1
        }
    }
}
