#![allow(dead_code)]

use serde_json::{Value, json};
use std::{
    env,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
};

pub fn run_targets(default_format: &str) -> anyhow::Result<()> {
    let socket = socket_path()?;
    let targets = match env::var("ONLYNE_TARGETS") {
        Ok(v) => v,
        Err(_) => stored_targets(&socket)?,
    };
    let text = env::var("ONLYNE_TEXT").unwrap_or_else(|_| default_text(default_format));
    let format = env::var("ONLYNE_FORMAT").unwrap_or_else(|_| default_format.into());
    let attachments: Value =
        serde_json::from_str(&env::var("ONLYNE_ATTACHMENTS").unwrap_or_else(|_| "[]".into()))?;
    for (n, channel) in targets
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        let req = json!({
            "id": format!("send-{}", n + 1),
            "op": "send_message",
            "channel_id": channel,
            "text": text,
            "format": format,
            "attachments": attachments,
        });
        println!("{}", request(&socket, &req)?);
    }
    Ok(())
}

pub fn run_channel(channel: &str, env_prefix: &str) -> anyhow::Result<()> {
    if env::var("ONLYNE_TARGETS").is_ok() {
        return run_targets("plain");
    }
    let socket = socket_path()?;
    let _ = env_prefix;
    let text = env::var("ONLYNE_TEXT").unwrap_or_else(|_| "zig".into());
    let format = env::var("ONLYNE_FORMAT").unwrap_or_else(|_| "plain".into());
    let attachments: Value =
        serde_json::from_str(&env::var("ONLYNE_ATTACHMENTS").unwrap_or_else(|_| "[]".into()))?;
    let req = json!({
        "id": "send-1",
        "op": "send_message",
        "channel_id": channel,
        "text": text,
        "format": format,
        "attachments": attachments,
    });
    println!("{}", request(&socket, &req)?);
    Ok(())
}

pub fn list_channels() -> anyhow::Result<()> {
    println!(
        "{}",
        request(
            &socket_path()?,
            &json!({"id":"channels","op":"list_channels"})
        )?
    );
    Ok(())
}

pub fn fetch_all_history(limit: u32) -> anyhow::Result<()> {
    println!(
        "{}",
        request(
            &socket_path()?,
            &json!({"id":"hist","op":"fetch_all_history","limit":limit})
        )?
    );
    Ok(())
}

fn default_text(format: &str) -> String {
    if format == "markdown" {
        "# Onlyne 富文本测试\n\n- **粗体** / _斜体_\n- `inline code`\n- [Onlyne](https://github.com/dbydd/onlyne)\n\n| 渠道 | 状态 |\n| --- | --- |\n| Telegram | HTML 富文本 |\n| Feishu | Lark MD 卡片 |\n\n```rust\nprintln!(\"hello rich media\");\n```"
            .into()
    } else {
        "zig".into()
    }
}

fn stored_targets(socket: &PathBuf) -> anyhow::Result<String> {
    let res = request(
        socket,
        &json!({"id":"conversations","op":"list_conversations"}),
    )?;
    let Some(items) = res.get("data").and_then(Value::as_array) else {
        anyhow::bail!("list_conversations returned no data")
    };
    let targets: Vec<String> = items.iter().filter_map(channel_target).collect();
    if targets.is_empty() {
        anyhow::bail!(
            "no stored conversations in examples/.onlyne; send one inbound message first or set ONLYNE_TARGETS"
        )
    }
    Ok(targets.join(","))
}

fn channel_target(v: &Value) -> Option<String> {
    v.get("channel_id")?.as_str().map(str::to_string)
}

fn request(socket: &PathBuf, req: &Value) -> anyhow::Result<Value> {
    let mut stream = UnixStream::connect(socket).map_err(|e| {
        anyhow::anyhow!("connect {}: {e}; start onlyne run first", socket.display())
    })?;
    writeln!(stream, "{}", req)?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(serde_json::from_str(line.trim())?)
}

fn socket_path() -> anyhow::Result<PathBuf> {
    if let Ok(path) = env::var("ONLYNE_SOCKET") {
        return Ok(path.into());
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let examples = manifest.join("examples/.onlyne/run/onlyne.sock");
    if examples.exists() {
        return Ok(examples);
    }
    let cwd = env::current_dir()?;
    for dir in cwd.ancestors() {
        let p = dir.join(".onlyne/run/onlyne.sock");
        if p.exists() {
            return Ok(p);
        }
    }
    Ok(examples)
}
