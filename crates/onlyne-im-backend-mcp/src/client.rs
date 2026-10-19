//! Minimal JSON-RPC 2.0 stdio MCP client for `onlyne-im-backend-mcp`.
//!
//! Handles MCP `initialize` + `notifications/initialized`, `tools/call`
//! ( automatically unwrapping MCP `content[0].text` JSON when `structuredContent`
//! is absent), and server-pushed JSON-RPC notifications.

use crate::spec::McpServerConfig;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub struct McpClient {
    _child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    notifications: VecDeque<Value>,
}

impl McpClient {
    pub async fn spawn(cfg: &McpServerConfig) -> Result<Self> {
        let mut cmd = Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(cwd) = &cfg.cwd {
            cmd.current_dir(cwd);
        }
        if let Some(env_file) = &cfg.env_file
            && let Ok(text) = std::fs::read_to_string(env_file)
        {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let line = line.strip_prefix("export ").unwrap_or(line).trim();
                if let Some((k, v)) = line.split_once('=') {
                    let k = k.trim();
                    let v = v.trim().trim_matches('"').trim_matches('\'');
                    if !k.is_empty() {
                        cmd.env(k, v);
                    }
                }
            }
        }
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning MCP server {:?}", cfg.command))?;
        let stdin = child.stdin.take().context("MCP child has no stdin")?;
        let stdout = child.stdout.take().context("MCP child has no stdout")?;
        let mut client = Self {
            _child: child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 1,
            notifications: VecDeque::new(),
        };
        client.initialize().await?;
        Ok(client)
    }

    async fn initialize(&mut self) -> Result<()> {
        let _ = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "onlyne-im-backend-mcp",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
            )
            .await
            .context("MCP initialize handshake failed")?;
        self.write_json(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .await?;
        Ok(())
    }

    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let raw = self
            .request(
                "tools/call",
                json!({
                    "name": name,
                    "arguments": arguments,
                }),
            )
            .await
            .with_context(|| format!("calling MCP tool {name}"))?;
        if raw.get("isError").and_then(Value::as_bool).unwrap_or(false) {
            bail!("MCP tool {name} returned isError=true: {raw}");
        }
        Ok(unwrap_tool_result(raw))
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;
        loop {
            let Some(line) = self.lines.next_line().await? else {
                bail!("MCP server closed stdout while waiting for response to {method} (id={id})");
            };
            if line.trim().is_empty() {
                continue;
            }
            let msg: Value = serde_json::from_str(&line)
                .with_context(|| format!("MCP server emitted invalid JSON: {line}"))?;
            if msg.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = msg.get("error") {
                    bail!("MCP {method} error: {err}");
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            if msg.get("method").is_some() && msg.get("id").is_none() {
                self.notifications.push_back(msg);
            }
        }
    }

    pub fn drain_notifications(&mut self) -> Vec<Value> {
        self.notifications.drain(..).collect()
    }

    pub async fn next_notification(&mut self) -> Result<Option<Value>> {
        if let Some(buffered) = self.notifications.pop_front() {
            return Ok(Some(buffered));
        }
        loop {
            let Some(line) = self.lines.next_line().await? else {
                return Ok(None);
            };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if msg.get("method").is_some() && msg.get("id").is_none() {
                return Ok(Some(msg));
            }
        }
    }

    async fn write_json(&mut self, msg: &Value) -> Result<()> {
        let mut line = serde_json::to_string(msg)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }
}

/// Normalize an MCP `CallToolResult` so selectors can address fields directly:
///
/// 1. If `structuredContent` is an object/array, return it.
/// 2. If `content[0].text` parses as JSON, return that parsed JSON.
/// 3. Otherwise return the `CallToolResult` as-is.
pub fn unwrap_tool_result(result: Value) -> Value {
    if let Some(structured) = result.get("structuredContent")
        && (structured.is_object() || structured.is_array())
    {
        return structured.clone();
    }
    if let Some(text) = result.pointer("/content/0/text").and_then(Value::as_str)
        && let Ok(parsed) = serde_json::from_str::<Value>(text)
    {
        return parsed;
    }
    result
}
