use std::path::PathBuf;

use clap::Parser;
use onlyne_adapter::AdapterClient;
use onlyne_testkit::{
    AgentScript, FakeAgent, default_agent_capabilities, parse_capability_csv,
    read_script_from_stdin, role_from_workspace, script_from_path, socket_from_workspace,
};
use tokio::time::{Duration, sleep};

#[derive(Debug, Parser)]
#[command(name = "onlyne-agent-fake")]
struct Args {
    #[arg(long)]
    workspace: Option<PathBuf>,
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Role to mount; the workspace config names it when omitted.
    #[arg(long)]
    role: Option<String>,
    #[arg(long)]
    script: Option<PathBuf>,
    #[arg(long)]
    capabilities: Option<String>,
    #[arg(long)]
    stdin_script: bool,
    #[arg(long)]
    once: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let workspace = args.workspace.unwrap_or(std::env::current_dir()?);
    let socket = args
        .socket
        .unwrap_or_else(|| socket_from_workspace(&workspace));
    let role = match args.role {
        Some(role) => role,
        None => role_from_workspace(&workspace)?,
    };
    let script = if args.stdin_script {
        read_script_from_stdin().await?
    } else if let Some(path) = args.script.as_deref() {
        script_from_path(path)?
    } else {
        AgentScript {
            hello: onlyne_testkit::ScriptHello {
                capabilities: args
                    .capabilities
                    .as_deref()
                    .map(parse_capability_csv)
                    .transpose()?
                    .unwrap_or_else(default_agent_capabilities),
            },
            steps: Vec::new(),
        }
    };
    let capabilities = args
        .capabilities
        .as_deref()
        .map(parse_capability_csv)
        .transpose()?
        .unwrap_or_else(default_agent_capabilities);
    let mut last_error = None;
    for _attempt in 0..20 {
        match AdapterClient::connect_unix(&socket).await {
            Ok(handle) => {
                let agent = FakeAgent::new(
                    role.clone(),
                    capabilities.clone(),
                    script.clone(),
                    workspace.clone(),
                );
                agent.run(&handle).await?;
                if args.once {
                    return Ok(());
                }
                loop {
                    sleep(Duration::from_secs(60)).await;
                }
            }
            Err(err) => last_error = Some(err),
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow::anyhow!(
        "unable to connect to adapter socket {}: {}",
        socket.display(),
        last_error
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown error".to_string())
    ))
}
