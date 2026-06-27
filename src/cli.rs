use crate::{app::App, auth, ipc, workspace::Workspace};
use anyhow::Context;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{
    generate,
    shells::{Fish, Zsh},
};
use std::fs;
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
    ExportSkill,
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
    #[arg(long)]
    sandbox: bool,
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
    Qqbot,
    #[value(alias = "weixin")]
    Wechat,
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
        Cmd::ExportSkill => {
            let ws = resolve_workspace(workspace.clone())?;
            let path = export_agent_skill(&ws)?;
            println!("exported skill {}", path.display());
            Ok(())
        }
        Cmd::Run { debug } => {
            let ws = resolve_workspace(workspace.clone())?;
            init_logging(&ws)?;
            let app = App::load_with_debug(ws, debug).await?;
            app.start_all().await?;
            app.start_channel_io().await?;
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
            if args.sandbox {
                anyhow::bail!("feishu auth does not use --sandbox");
            }
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
        AuthChannel::Qqbot => {
            if args.token.is_some() {
                anyhow::bail!("qqbot auth does not use --token; use --app-id/--app-secret");
            }
            if args.api_url != "https://ilinkai.weixin.qq.com" || args.bot_type != "3" {
                anyhow::bail!("qqbot auth does not use --api-url/--bot-type");
            }
            auth::auth_qqbot(
                &ws,
                auth::QqBotAuthOptions {
                    app_id: args.app_id,
                    app_secret: args.app_secret,
                    sandbox: args.sandbox,
                },
            )
            .await
        }
        AuthChannel::Wechat => {
            if args.sandbox {
                anyhow::bail!("wechat auth does not use --sandbox");
            }
            if args.app_id.is_some() || args.app_secret.is_some() {
                anyhow::bail!("wechat auth does not use --app-id/--app-secret; use --token or QR");
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

const ONLYNE_AGENT_SKILL: &str = r###"---
name: onlyne
description: Use when an agent needs to send, receive, subscribe to, or inspect workspace-local IM channel messages through Onlyne.
---

# Onlyne

## Overview

Onlyne is a workspace-local IM channel broker. Use it only as a local messaging bridge: send messages, receive subscribed events, and inspect local history through the workspace `.onlyne/` daemon state.

## Rules

- Run commands inside the project tree, or pass `--workspace <dir>`.
- Do not write credentials into global home directories. Secrets belong in the selected workspace `.onlyne/.env`.
- Do not commit `.onlyne/`, logs, runtime databases, sockets, or channel tokens.
- Do not treat Onlyne as an agent runtime, model runner, scheduler, or prompt system.
- Use `onlyne run --debug` only while discovering channel/conversation/thread metadata; debug replies are for setup, not normal operation.

## Quick Reference

| Need | Command |
| --- | --- |
| Initialize workspace | `onlyne init` |
| Export/update local skill | `onlyne export-skill` |
| Run daemon | `onlyne run` |
| Run with metadata replies | `onlyne run --debug` |
| Health check | `onlyne client '{"id":"ping","op":"ping"}'` |
| Status/channels | `onlyne client '{"id":"status","op":"status"}'` |
| Send Markdown | `onlyne client '{"id":"send","op":"send_message","channel_id":"qqbot","text":"# Report\\n\\n| A | B |\\n|---|---|\\n| 1 | 2 |"}'` |
| Send literal text | `onlyne client '{"id":"send","op":"send_message","channel_id":"telegram","text":"# not a heading","raw_text":true}'` |
| Wake local agent | `onlyne client '{"id":"wake","op":"loopback","text":"background job needs attention","raw_text":true}'` |
| Reply text | `onlyne client '{"id":"reply","op":"reply_message","channel_id":"telegram","text":"hello","raw_text":true}'` |
| Read channel history | `onlyne client '{"id":"hist","op":"fetch_channel_history","channel_id":"telegram","limit":20}'` |
| Read merged history | `onlyne client '{"id":"all","op":"fetch_all_history","limit":50}'` |
| FIFO send | `printf '# report\n' > .onlyne/channels/qqbot/in` |
| FIFO receive | `cat .onlyne/channels/qqbot/out` |

## File Descriptor IO

When the daemon is running, each enabled channel plus `loopback` exposes FIFO files under `.onlyne/channels/<channel>/`:

```text
.onlyne/channels/qqbot/in
.onlyne/channels/qqbot/out
```

- Write one message to `in`; EOF ends the message.
- Read one inbound message from `out`; it blocks until a message is available.
- FIFO input format is configured with `in_format = "markdown" | "raw_text"`.
- FIFO output behavior is configured with `out_content = "latest_only" | "with_history"` and `out_cursor = "retain" | "consume"`.
- `loopback/in` wakes the local agent session through Onlyne loopback.
- `examples/fifo/smoke-fifo-all-qq.sh` writes all channels via FIFO and reads QQ inbound through `.onlyne/channels/qqbot/out`.

## Config Schema

Workspace config starts with Taplo's schema hint:

```toml
#:schema ./onlyne-config.schema.json
```

Refresh the generated schema after changing Rust config types:

```bash
cargo run --bin gen-schema > onlyne-config.schema.json
```

## Subscribe to Events

`onlyne client` prints one response and exits, so long-lived subscriptions should keep the Unix socket open. The socket path is always workspace-local: `.onlyne/run/onlyne.sock`.

```bash
python3 - <<'PY'
import json, socket
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect('.onlyne/run/onlyne.sock')
sock.sendall(b'{"id":"sub","op":"subscribe_events"}\n')
while True:
    print(sock.recv(65536).decode(), end='')
PY
```

Subscribed event lines have `event:true`; request responses have `ok:true` or `ok:false`.

## Markdown Semantics

External callers send one whole Markdown document in `text`; Markdown is the default. Do not split tables, formulas, or code blocks before sending. Set `raw_text:true` only for literal plain text.

- QQ Bot receives the whole document as QQ extended Markdown (`msg_type=2`, `markdown.content`), including tables and formulas.
- Telegram and WeChat may internally split Markdown tables into rendered image parts.
- Feishu sends Markdown as an interactive card and keeps supported table content in-card.
- The response/history may contain `platform_metadata.delivery_parts` when one logical send becomes multiple platform messages.

If the host agent has Onlyne tools, prefer:

```text
onlyne_send({ channelId, text })
onlyne_broadcast({ targets, text })
onlyne_loopback({ text, rawText? })
// raw literal text only:
onlyne_send({ channelId, text, rawText: true })
```

Otherwise use the CLI/socket request shown above.

## Discover Conversation IDs

1. Start `onlyne run --debug` in the workspace.
2. Send a normal message to the target platform bot/account.
3. Read the platform reply; it contains redacted channel/conversation/thread metadata.
4. Put the returned conversation value into that adapter's `bind_conversation_id`, then send with only `channel_id`.

## Common Mistakes

- If `connect onlyne socket` fails, start `onlyne run` in the same workspace or pass the same `--workspace <dir>` to both commands.
- If history is empty, first verify the adapter is enabled and `status` shows the expected channel.
- If sends go to the wrong place, rediscover the conversation with `--debug`; platform IDs are not interchangeable across Telegram, Feishu, QQ Bot, and WeChat.
- If multiple examples should share config, initialize the parent directory once and run child commands under it so upward workspace discovery finds the same `.onlyne/`.
"###;

fn export_agent_skill(ws: &Workspace) -> anyhow::Result<PathBuf> {
    let dir = ws.root().join(".agents/skills/onlyne");
    fs::create_dir_all(&dir)?;
    let path = dir.join("SKILL.md");
    fs::write(&path, ONLYNE_AGENT_SKILL)?;
    Ok(path)
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
    fn export_skill_command_is_parsed() {
        let cli = Cli::try_parse_from(["onlyne", "export-skill"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::ExportSkill));
    }

    #[test]
    fn export_skill_writes_workspace_local_agents_skill() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::resolve(dir.path());

        let path = export_agent_skill(&ws).unwrap();

        let expected = dir.path().join(".agents/skills/onlyne/SKILL.md");
        assert_eq!(path, expected);
        let body = std::fs::read_to_string(expected).unwrap();
        assert!(body.contains("name: onlyne"));
        assert!(body.contains("send_message"));
        assert!(body.contains("loopback"));
        assert!(body.contains("onlyne_loopback"));
        assert!(body.contains("raw_text"));
        assert!(body.contains(".onlyne/channels/qqbot/out"));
        assert!(body.contains("gen-schema"));
        assert!(body.contains(".onlyne/.env"));
        assert!(!dir.path().join(".agents/skills/SKILL.md").exists());
    }

    #[test]
    fn completions_include_export_skill() {
        let text = completion_text(Zsh);
        assert!(text.contains("export-skill"));
        assert!(!text.contains("--export-skill"));
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
