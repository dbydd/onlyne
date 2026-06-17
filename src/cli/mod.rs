use crate::{app::App, auth, ipc, workspace::Workspace};
use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
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

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init => {
            let ws = Workspace::current()?;
            ws.bootstrap()?;
            println!("initialized {}", ws.dir().display());
            Ok(())
        }
        Cmd::Run { debug } => {
            let ws = Workspace::current()?;
            init_logging(&ws)?;
            let app = App::load_with_debug(ws, debug).await?;
            app.start_all().await?;
            ipc::serve_socket(app).await
        }
        Cmd::Stdio => {
            let ws = Workspace::current()?;
            let app = App::load(ws).await?;
            app.start_all().await?;
            ipc::handle_stdio(app).await
        }
        Cmd::Client { json } => client(json).await,
        Cmd::Auth(args) => auth_cmd(args).await,
        Cmd::ConfigCheck => {
            let ws = Workspace::current()?;
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
async fn auth_cmd(args: AuthArgs) -> anyhow::Result<()> {
    let ws = Workspace::current()?;
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

fn init_logging(ws: &Workspace) -> anyhow::Result<()> {
    let file = tracing_appender::rolling::never(ws.log_path().parent().unwrap(), "daemon.log");
    fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(file)
        .init();
    Ok(())
}
async fn client(line: String) -> anyhow::Result<()> {
    let ws = Workspace::current()?;
    let mut s = UnixStream::connect(ws.socket_path())
        .await
        .context("connect onlyne socket")?;
    s.write_all(line.as_bytes()).await?;
    s.write_all(b"\n").await?;
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
