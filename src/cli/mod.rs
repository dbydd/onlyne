use crate::{app::App, auth, ipc, workspace::Workspace};
use anyhow::Context;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{
    generate,
    shells::{Fish, Zsh},
};
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};
use tracing_subscriber::{EnvFilter, fmt};

#[derive(Parser)]
#[command(
    name = "onlyne",
    version,
    about = "Workspace-local IM channel daemon/broker"
)]
struct Cli {
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Init,
    Run {
        #[arg(long)]
        debug: bool,
    },
    Stdio,
    Client {
        json: String,
    },
    ConfigCheck,
    Auth(AuthArgs),
    ShellCompletions {
        shell: CompletionShell,
    },
}

#[derive(Args)]
struct AuthArgs {
    #[arg(value_enum)]
    channel: AuthChannel,
    #[arg(long)]
    app_id: Option<String>,
    #[arg(long)]
    app_secret: Option<String>,
    #[arg(long)]
    token: Option<String>,
    #[arg(long, default_value = "https://ilinkai.weixin.qq.com")]
    api_url: String,
    #[arg(long, default_value = "3")]
    bot_type: String,
    #[arg(long, default_value_t = 480)]
    timeout: u64,
}

#[derive(Copy, Clone, ValueEnum)]
enum AuthChannel {
    Feishu,
    Weixin,
}

#[derive(Copy, Clone, ValueEnum)]
enum CompletionShell {
    Zsh,
    Fish,
}

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let workspace = cli.workspace.clone();
    match cli.cmd {
        Cmd::Init => {
            let ws = resolve_workspace(workspace.clone())?;
            ws.bootstrap()?;
            println!("initialized {}", ws.dir().display());
            Ok(())
        }
        Cmd::Run { debug } => {
            let ws = resolve_workspace(workspace.clone())?;
            init_logging(&ws)?;
            let app = App::load_with_debug(ws, debug).await?;
            app.start_all().await?;
            ipc::serve_socket(app).await
        }
        Cmd::Stdio => {
            let ws = resolve_workspace(workspace.clone())?;
            let app = App::load(ws).await?;
            app.start_all().await?;
            ipc::handle_stdio(app).await
        }
        Cmd::Client { json } => client(workspace.clone(), json).await,
        Cmd::Auth(args) => auth_cmd(workspace.clone(), args).await,
        Cmd::ShellCompletions { shell } => {
            shell_completions(shell);
            Ok(())
        }
        Cmd::ConfigCheck => {
            let ws = resolve_workspace(workspace.clone())?;
            let app = App::load(ws).await?;
            for (id, res) in app.check().await? {
                match res {
                    Ok(()) => println!("{id}: ok"),
                    Err(e) => println!("{id}: error: {}", redact(&e)),
                }
            }
            Ok(())
        }
    }
}

async fn auth_cmd(workspace: Option<PathBuf>, args: AuthArgs) -> anyhow::Result<()> {
    let ws = resolve_workspace(workspace)?;
    match args.channel {
        AuthChannel::Feishu => {
            if args.token.is_some() {
                anyhow::bail!("feishu auth does not use --token; use --app-id/--app-secret or QR");
            }
            auth::auth_feishu(
                &ws,
                auth::FeishuAuthOptions {
                    app_id: args.app_id,
                    app_secret: args.app_secret,
                    timeout: Duration::from_secs(args.timeout),
                },
            )
            .await
        }
        AuthChannel::Weixin => {
            if args.app_id.is_some() || args.app_secret.is_some() {
                anyhow::bail!("weixin auth does not use --app-id/--app-secret; use --token or QR");
            }
            auth::auth_weixin(
                &ws,
                auth::WeixinAuthOptions {
                    token: args.token,
                    api_url: args.api_url,
                    bot_type: args.bot_type,
                    timeout: Duration::from_secs(args.timeout),
                },
            )
            .await
        }
    }
}

fn resolve_workspace(path: Option<PathBuf>) -> anyhow::Result<Workspace> {
    match path {
        Some(path) => Ok(Workspace::resolve(path)),
        None => Workspace::current(),
    }
}

fn shell_completions(shell: CompletionShell) {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    match shell {
        CompletionShell::Zsh => generate(Zsh, &mut cmd, name, &mut io::stdout()),
        CompletionShell::Fish => generate(Fish, &mut cmd, name, &mut io::stdout()),
    }
}

fn init_logging(ws: &Workspace) -> anyhow::Result<()> {
    let file = tracing_appender::rolling::never(ws.log_path().parent().unwrap(), "daemon.log");
    fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(file)
        .init();
    Ok(())
}
async fn client(workspace: Option<PathBuf>, line: String) -> anyhow::Result<()> {
    let ws = resolve_workspace(workspace)?;
    let mut s = UnixStream::connect(ws.socket_path())
        .await
        .context("connect onlyne socket")?;
    s.write_all(line.as_bytes()).await?;
    s.write_all(
        b"
",
    )
    .await?;
    let mut lines = BufReader::new(s).lines();
    if let Some(resp) = lines.next_line().await? {
        println!("{resp}");
    }
    Ok(())
}
fn redact(s: &str) -> String {
    let mut out = s.to_string();
    for key in ["token", "secret", "authorization", "password"] {
        if out.to_lowercase().contains(key) {
            out = "<redacted error containing secret-like text>".into();
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap_complete::{
        generate,
        shells::{Fish, Zsh},
    };

    fn completion_text(shell: impl clap_complete::Generator) -> String {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        let mut out = Vec::new();
        generate(shell, &mut cmd, name, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn completion_command_is_exposed() {
        let cmd = Cli::command();
        assert!(
            cmd.get_subcommands()
                .any(|sc| sc.get_name() == "shell-completions")
        );
    }

    #[test]
    fn workspace_flag_is_global() {
        let before =
            Cli::try_parse_from(["onlyne", "--workspace", "/tmp/onlyne-ws", "config-check"])
                .unwrap();
        assert_eq!(before.workspace, Some(PathBuf::from("/tmp/onlyne-ws")));

        let after =
            Cli::try_parse_from(["onlyne", "config-check", "--workspace", "/tmp/onlyne-ws"])
                .unwrap();
        assert_eq!(after.workspace, Some(PathBuf::from("/tmp/onlyne-ws")));
    }

    #[test]
    fn zsh_completion_mentions_onlyne() {
        let text = completion_text(Zsh);
        assert!(text.contains("#compdef onlyne"));
        assert!(text.contains("shell-completions"));
    }

    #[test]
    fn fish_completion_mentions_onlyne() {
        let text = completion_text(Fish);
        assert!(text.contains("complete -c onlyne"));
        assert!(text.contains("shell-completions"));
    }
}
