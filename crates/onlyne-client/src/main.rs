use clap::{Parser, Subcommand};
use onlyne_client::{
    ClientInit, ops::init::InitArgs, ops::local_cli, ops::local_cli::LocalCli, runtime::daemon,
    runtime::intent::IntentMachine,
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
    /// Run one role client against its workspace config.
    ///
    /// Each session this client holds needs a terminal host for its pane, so a
    /// run that detects no host and names no backend in ONLYNE_BACKEND or in
    /// the workspace config's `backend` key stops at startup with exit 5.
    #[command(
        after_help = "config: --workspace names the role workspace; its `.onlyne/config.toml` \
                      carries `backend` (herdr|orca|zellij|exec|headless|acp|fake|auto; empty \
                      probes the host, ONLYNE_BACKEND wins) and, for the acp backend, the \
                      `[acp]` table: `mode`, `model`, `reasoning_effort` (each validated by \
                      the agent, empty keeps its default) and `permission` (`deny`, the \
                      default, or `allow`)."
    )]
    Run {
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Create a role workspace and print its `[[client]]` spec fragment. Writes
    /// `.onlyne/config.toml` and `.onlyne/keys/role.key` under `--workspace`,
    /// takes `cert_pin` and the endpoint from `--server-root`'s spec, and appends
    /// nothing to that spec. Exits 2 on a legacy workspace.
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
    /// Report the client answering this workspace's socket. Prints uptime, the
    /// served socket path, and the recorded fault count, and says whether the
    /// client holds a ready server link. Exits 2 when no client answers or the
    /// answering client holds no ready server link.
    Status {
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Answer the role query from the prose cache in this workspace's
    /// `client.db`; opens no socket.
    Roles {
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Point at `onlyne sessions`, the admin verb that answers this query.
    Sessions {
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Point at `onlyne watch`, the admin verb that answers this stream.
    Watch {
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Point at `onlyne history`, the admin verb that answers this replay.
    History {
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Manage the plugin packages installed in this workspace.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Print host detection JSON. Needs no socket. Always exits 0.
    Doctor,
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
        } => match onlyne_client::ops::init::init(InitArgs {
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
            // The operator may name the workspace relatively. `absolute_path`
            // canonicalizes what exists and falls back to a lexical absolute
            // spelling, so the tree the client resolves, binds, and hands the
            // session backends as `SpawnSpec.cwd` is one absolute answer. A
            // relative cwd would be read against whatever directory a host
            // surface happens to start its pane in.
            let workspace = onlyne_layout::absolute_path(&workspace);
            let path = onlyne_layout::RoleWorkspace::resolve(&workspace);
            local_cli::heal_workspace_config(&workspace);
            match load_workspace_config(&path.config_path()) {
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
                    .with_orca_worktree(config.orca.worktree)
                    .with_stall_report_secs(config.stall_report_secs)
                    .with_reconnect_grace_secs(config.reconnect_grace_secs)
                    .with_backend(config.backend)
                    .with_acp(config.acp),
                )
                .await
                {
                    Ok(()) => 0,
                    Err(error)
                        if error
                            .downcast_ref::<onlyne_session::NoSupportedHost>()
                            .is_some() =>
                    {
                        eprintln!("{error}");
                        5
                    }
                    Err(error) => {
                        // `:#` prints the whole chain, which is where a bind
                        // failure keeps the path it tried, the spelling it stands
                        // for, each length, and the OS reason.
                        eprintln!("onlyne-client: {error:#}");
                        1
                    }
                },
                Err(error) => {
                    eprintln!("onlyne-client: {error}");
                    1
                }
            }
        }
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
                "onlyne: no local query socket here; these three answer on the admin surface as \
                 onlyne sessions, onlyne watch, and onlyne history"
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
        Command::Doctor => {
            let report = onlyne_client::host::doctor_report(&onlyne_session::process_env());
            println!("{report}");
            0
        }
    };
    std::process::exit(code);
}

/// Load a workspace's `config.toml` and resolve every `$NAME` value from the
/// process environment. `cert_pin`, `key_path`, and `server.host` may each hold
/// an environment variable reference. A reference whose variable is unset ends
/// startup, and the message names the variable.
fn load_workspace_config(
    path: &Path,
) -> Result<onlyne_config::ClientConfig, onlyne_config::SpecError> {
    let mut config = onlyne_config::ClientConfig::load(path)?;
    config.resolve_secrets(&onlyne_config::Env::current())?;
    Ok(config)
}

/// Route `tracing` output to stderr, which the operator redirects into
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

#[cfg(test)]
mod tests {
    use super::load_workspace_config;

    /// `cert_pin = "$NAME"` with the variable unset: the loader refuses and the
    /// operator-visible message names the variable. The name carries the process
    /// id, so no ambient environment can make it resolve.
    #[test]
    fn missing_env_secret_names_the_variable_in_the_error() {
        let var = format!("ONLYNE_TEST_MISSING_PIN_{}", std::process::id());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            format!(
                "role = \"planner\"\ncert_pin = \"${var}\"\nkey_path = \"keys/role.key\"\n\n[server]\nhost = \"127.0.0.1\"\nport = 7811\n"
            ),
        )
        .unwrap();
        let error = load_workspace_config(&path).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("missing secret ${var} for cert_pin; set the environment variable")
        );
    }
}
