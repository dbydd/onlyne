#![allow(dead_code)]

use serde_json::{Value, json};
use std::{
    env,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
};

pub fn run_targets(default_format: &str) -> anyhow::Result<()> {
    let targets = env::var("ONLYNE_TARGETS").map_err(|_| {
        anyhow::anyhow!("set ONLYNE_TARGETS as channel:conversation[,channel:conversation...]")
    })?;
    let text = env::var("ONLYNE_TEXT").unwrap_or_else(|_| "zig".into());
    let format = env::var("ONLYNE_FORMAT").unwrap_or_else(|_| default_format.into());
    let attachments: Value =
        serde_json::from_str(&env::var("ONLYNE_ATTACHMENTS").unwrap_or_else(|_| "[]".into()))?;
    let socket = socket_path()?;
    for (n, target) in targets.split(',').enumerate() {
        let Some((channel, conversation)) = target.split_once(':') else {
            anyhow::bail!("bad target {target:?}; want channel:conversation");
        };
        let req = json!({
            "id": format!("send-{}", n + 1),
            "op": "send_message",
            "channel_id": channel,
            "conversation_id": conversation,
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
    let var = format!("ONLYNE_{env_prefix}_CONVERSATION_ID");
    let conversation = env::var(&var).map_err(|_| {
        anyhow::anyhow!("set {var}, or set ONLYNE_TARGETS='{channel}:conversation'")
    })?;
    let text = env::var("ONLYNE_TEXT").unwrap_or_else(|_| "zig".into());
    let format = env::var("ONLYNE_FORMAT").unwrap_or_else(|_| "plain".into());
    let attachments: Value =
        serde_json::from_str(&env::var("ONLYNE_ATTACHMENTS").unwrap_or_else(|_| "[]".into()))?;
    let req = json!({
        "id": "send-1",
        "op": "send_message",
        "channel_id": channel,
        "conversation_id": conversation,
        "text": text,
        "format": format,
        "attachments": attachments,
    });
    println!("{}", request(&socket_path()?, &req)?);
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
    let cwd = env::current_dir()?;
    for dir in cwd.ancestors() {
        let p = dir.join(".onlyne/run/onlyne.sock");
        if p.exists() {
            return Ok(p);
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let p = manifest.join("examples/.onlyne/run/onlyne.sock");
    if p.exists() {
        return Ok(p);
    }
    Ok(cwd.join(".onlyne/run/onlyne.sock"))
}
