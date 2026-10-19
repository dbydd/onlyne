//! Meta-programmable MCP-to-IM platform backend (`onlyne-im-backend-mcp`).
//!
//! Bridges any stdio Model Context Protocol (MCP) server into the standard
//! `onlyne-im-backend` protocol through a declarative TOML specification:
//!
//! - `[mcp]`: command, args, env, and optional `.env` file to spawn the MCP
//!   server over stdio.
//! - `[[inbound.notification]]`: JSON-RPC notification methods the MCP server
//!   pushes on its own, plus field extraction rules (`items_pointer`, `id`,
//!   `conversation`, `text`, `sender_id`, `sender_name`, `reply_parent`,
//!   `event_type`, `kind`, `action`).
//! - `[inbound.poll]`: an MCP tool called periodically (`every_secs`), with
//!   templated arguments (`{{cursor}}`, `{{since}}`), `cursor_pointer`, and the
//!   same field extraction rules plus echo/dedupe filtering.
//! - `[outbound.send]`: an MCP tool called on `Backend::send`, with templated
//!   JSON arguments (`{{conversation}}`, `{{text}}`, `{{format}}`,
//!   `{{reply_to}}`, `{{attachments_json}}`, `{{actions_json}}`) and
//!   `message_id_pointer` to extract the created message ID for reply chains.
//! - `[conversations.list]`: an optional MCP tool called on
//!   `Backend::list_conversations` to discover chats at startup.

pub mod client;
pub mod spec;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use client::McpClient;
use onlyne_im_backend::backend::Backend;
use onlyne_im_backend::frame::{HealthArgs, HelloArgs, ImConversation, SendArgs, SentArgs};
use onlyne_im_backend::payload::ImEvent;
use serde_json::{Value, json};
use spec::{EventMapping, McpImSpec, render_template, select_str, select_value};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DEDUPE_CAP: usize = 2048;

pub struct McpImBackend {
    gateway: String,
    spec: McpImSpec,
    client: Option<McpClient>,
    queue: VecDeque<ImEvent>,
    seen_ids: VecDeque<String>,
    seen_set: HashSet<String>,
    echo_ids: HashSet<String>,
    cursor: String,
    since: String,
    started: Instant,
    problem: Option<String>,
}

impl McpImBackend {
    pub async fn from_spec(gateway: impl Into<String>, spec: McpImSpec) -> Self {
        let gateway = gateway.into();
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let (client, problem) = match McpClient::spawn(&spec.mcp).await {
            Ok(c) => (Some(c), None),
            Err(err) => (None, Some(format!("{err:#}"))),
        };
        Self {
            gateway,
            spec,
            client,
            queue: VecDeque::new(),
            seen_ids: VecDeque::new(),
            seen_set: HashSet::new(),
            echo_ids: HashSet::new(),
            cursor: String::new(),
            since: now,
            started: Instant::now(),
            problem,
        }
    }

    fn remember_id(&mut self, id: &str) -> bool {
        if id.is_empty() {
            return true;
        }
        if self.echo_ids.contains(id) || self.seen_set.contains(id) {
            return false;
        }
        self.seen_set.insert(id.to_string());
        self.seen_ids.push_back(id.to_string());
        while self.seen_ids.len() > DEDUPE_CAP {
            if let Some(old) = self.seen_ids.pop_front() {
                self.seen_set.remove(&old);
            }
        }
        true
    }

    fn ingest_root(&mut self, root: &Value, mapping: &EventMapping) {
        let items: Vec<Value> = match mapping.items_pointer.as_deref() {
            Some(ptr) if !ptr.is_empty() => match select_value(root, ptr) {
                Some(Value::Array(arr)) => arr.clone(),
                Some(other) if !other.is_null() => vec![other.clone()],
                _ => Vec::new(),
            },
            _ => match root {
                Value::Array(arr) => arr.clone(),
                other => vec![other.clone()],
            },
        };
        for item in items {
            if let Some(ev) = mapping.extract(&self.gateway, &self.spec.platform, &item) {
                if self.remember_id(&ev.id) {
                    self.queue.push_back(ev);
                }
            }
        }
    }

    async fn run_one_poll(&mut self) -> Result<()> {
        let Some(poll) = self.spec.inbound.poll.clone() else {
            return Ok(());
        };
        let Some(client) = self.client.as_mut() else {
            return Ok(());
        };
        let mut vars = BTreeMap::new();
        vars.insert("cursor".to_string(), Value::String(self.cursor.clone()));
        vars.insert("since".to_string(), Value::String(self.since.clone()));
        vars.insert("gateway".to_string(), Value::String(self.gateway.clone()));
        let args = render_template(&poll.args, &vars);
        let result = client.call_tool(&poll.tool, args).await?;
        for notif in client.drain_notifications() {
            self.handle_notification(&notif);
        }
        if let Some(cursor_ptr) = poll.cursor_pointer.as_deref()
            && let Some(next_cursor) = select_str(&result, cursor_ptr)
            && !next_cursor.is_empty()
        {
            self.cursor = next_cursor;
        }
        self.since = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        self.ingest_root(&result, &poll.mapping);
        Ok(())
    }

    fn handle_notification(&mut self, notif: &Value) {
        let method = notif
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = notif.get("params").unwrap_or(&Value::Null);
        let matching: Vec<EventMapping> = self
            .spec
            .inbound
            .notification
            .iter()
            .filter(|n| n.method == "*" || n.method == method)
            .map(|n| n.mapping.clone())
            .collect();
        for mapping in matching {
            self.ingest_root(params, &mapping);
        }
    }
}

#[async_trait]
impl Backend for McpImBackend {
    fn hello(&self) -> HelloArgs {
        HelloArgs {
            platform: self.spec.platform.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec!["message".into(), "action".into()],
            authenticated: self.client.is_some() && self.problem.is_none(),
            onboarding: None,
        }
    }

    async fn list_conversations(&mut self) -> Result<Vec<ImConversation>> {
        let Some(list_spec) = self.spec.conversations.list.clone() else {
            return Ok(Vec::new());
        };
        let Some(client) = self.client.as_mut() else {
            return Ok(Vec::new());
        };
        let result = client
            .call_tool(&list_spec.tool, list_spec.args.clone())
            .await?;
        let items: Vec<Value> = match list_spec.items_pointer.as_deref() {
            Some(ptr) if !ptr.is_empty() => match select_value(&result, ptr) {
                Some(Value::Array(arr)) => arr.clone(),
                Some(other) if !other.is_null() => vec![other.clone()],
                _ => Vec::new(),
            },
            _ => match &result {
                Value::Array(arr) => arr.clone(),
                other => vec![other.clone()],
            },
        };
        let mut out = Vec::new();
        for item in items {
            let Some(id) = select_str(&item, &list_spec.id) else {
                continue;
            };
            let title = list_spec
                .title
                .as_deref()
                .and_then(|p| select_str(&item, p));
            let kind = list_spec.kind.as_deref().and_then(|p| select_str(&item, p));
            out.push(ImConversation { id, title, kind });
        }
        Ok(out)
    }

    async fn next_event(&mut self) -> Result<Option<ImEvent>> {
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return Ok(Some(ev));
            }
            if self.client.is_none() {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            let poll_every = self
                .spec
                .inbound
                .poll
                .as_ref()
                .map(|p| Duration::from_secs(p.every_secs.max(1)));
            match poll_every {
                Some(interval) => {
                    let notif_opt = {
                        let client = self.client.as_mut().expect("checked");
                        tokio::time::timeout(interval, client.next_notification()).await
                    };
                    match notif_opt {
                        Ok(Ok(Some(notif))) => {
                            self.handle_notification(&notif);
                        }
                        Ok(Ok(None)) => return Ok(None),
                        Ok(Err(err)) => {
                            tracing::warn!(error = %format!("{err:#}"), "mcp notification read failed");
                            return Ok(None);
                        }
                        Err(_elapsed) => {
                            if let Err(err) = self.run_one_poll().await {
                                tracing::warn!(error = %format!("{err:#}"), "mcp poll tool failed");
                            }
                        }
                    }
                }
                None => {
                    let client = self.client.as_mut().expect("checked");
                    match client.next_notification().await? {
                        Some(notif) => self.handle_notification(&notif),
                        None => return Ok(None),
                    }
                }
            }
        }
    }

    async fn send(&mut self, args: SendArgs) -> Result<SentArgs> {
        let Some(send_spec) = self.spec.outbound.send.clone() else {
            bail!("this MCP backend spec defines no `[outbound.send]` rule");
        };
        let Some(client) = self.client.as_mut() else {
            bail!(
                "the MCP server is not running: {}",
                self.problem.as_deref().unwrap_or("unknown error")
            );
        };
        let mut vars = BTreeMap::new();
        vars.insert(
            "conversation".to_string(),
            Value::String(args.conversation.clone()),
        );
        vars.insert("text".to_string(), Value::String(args.payload.text.clone()));
        vars.insert(
            "format".to_string(),
            Value::String(
                match args.payload.format {
                    onlyne_im_backend::payload::ImFormat::Plain => "plain",
                    onlyne_im_backend::payload::ImFormat::Markdown => "markdown",
                }
                .to_string(),
            ),
        );
        vars.insert(
            "reply_to".to_string(),
            args.payload
                .reply_to
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        vars.insert(
            "reply_reference".to_string(),
            args.payload
                .reply_reference
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        vars.insert(
            "attachments".to_string(),
            serde_json::to_value(&args.payload.attachments).unwrap_or_else(|_| json!([])),
        );
        vars.insert(
            "actions".to_string(),
            serde_json::to_value(&args.payload.actions).unwrap_or_else(|_| json!([])),
        );
        let tool_args = render_template(&send_spec.args, &vars);
        let result = client.call_tool(&send_spec.tool, tool_args).await?;
        for notif in client.drain_notifications() {
            self.handle_notification(&notif);
        }
        let message_id = send_spec
            .message_id_pointer
            .as_deref()
            .and_then(|ptr| select_str(&result, ptr));
        if let Some(id) = &message_id {
            self.echo_ids.insert(id.clone());
        }
        Ok(SentArgs {
            conversation: args.conversation,
            message_id,
            ref_handle: None,
            error: None,
        })
    }

    fn health(&self) -> HealthArgs {
        HealthArgs {
            state: if self.client.is_some() && self.problem.is_none() {
                "online"
            } else {
                "failed"
            }
            .to_string(),
            detail: self.problem.clone(),
            uptime_s: self.started.elapsed().as_secs(),
        }
    }
}

pub async fn run(gateway: String, spec_path: Option<PathBuf>) -> Result<()> {
    let path = spec_path.context(
        "pass `--spec <path>` or set `ONLYNE_IM_MCP_SPEC` to the MCP-IM TOML mapping file",
    )?;
    let spec = McpImSpec::load(&path)?;
    let backend = McpImBackend::from_spec(gateway, spec).await;
    onlyne_im_backend::serve(backend).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_im_backend::payload::{ImFormat, ImPayload};

    #[test]
    fn spec_parses_and_extracts_events_via_pointer_and_dot_paths() {
        let toml_text = r#"
platform = "matrix"

[mcp]
command = "python3"
args = ["-u", "fake_mcp.py"]

[inbound.poll]
tool = "matrix_list_messages"
every_secs = 2
args = { cursor = "{{cursor}}" }
cursor_pointer = "/next_batch"
items_pointer = "/messages"
id = "/event_id"
conversation = "room_id"
text = "content.body"
sender_id = "/sender"
reply_parent = "/relates_to/event_id"

[outbound.send]
tool = "matrix_send_message"
args = { room_id = "{{conversation}}", body = "{{text}}", reply_to = "{{reply_to}}", format = "{{format}}" }
message_id_pointer = "/event_id"
"#;
        let spec: McpImSpec = toml::from_str(toml_text).expect("parse spec");
        assert_eq!(spec.platform, "matrix");
        let poll = spec.inbound.poll.as_ref().expect("poll rule");
        let sample = json!({
            "event_id": "$ev_1",
            "room_id": "!room:example.org",
            "sender": "@alice:example.org",
            "content": { "body": "hello via mcp" },
            "relates_to": { "event_id": "$ev_0" }
        });
        let ev = poll
            .mapping
            .extract("matrix-gw", &spec.platform, &sample)
            .expect("extracted event");
        assert_eq!(ev.id, "$ev_1");
        assert_eq!(ev.conversation, "!room:example.org");
        assert_eq!(ev.text.as_deref(), Some("hello via mcp"));
        assert_eq!(ev.from.as_ref().unwrap().id, "@alice:example.org");
        assert_eq!(ev.reply.as_ref().unwrap().parent.as_deref(), Some("$ev_0"));
    }

    #[tokio::test]
    async fn end_to_end_stdio_mcp_server_poll_list_send_and_echo_suppression() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("mock_mcp.py");
        std::fs::write(
            &script_path,
            r#"import json, sys

sent = []
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    req_id = req.get("id")
    if method == "initialize":
        sys.stdout.write(json.dumps({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": "2025-03-26",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "mock-im-mcp", "version": "0.1.0"}
            }
        }) + "\n")
        sys.stdout.flush()
    elif method == "tools/call":
        params = req.get("params") or {}
        name = params.get("name")
        args = params.get("arguments") or {}
        if name == "list_rooms":
            payload = {"rooms": [{"id": "room-1", "name": "General", "kind": "group"}]}
        elif name == "send_msg":
            msg_id = f"m-out-{len(sent)+1}"
            sent.append({"id": msg_id, "room": args.get("room"), "text": args.get("text")})
            payload = {"created_id": msg_id}
        elif name == "poll_msgs":
            items = [
                {"id": "m-in-1", "room": "room-1", "user": "u-bob", "user_name": "Bob", "text": "ping from mcp"}
            ] + sent
            payload = {"next_cursor": "c-2", "items": items}
        else:
            payload = {}
        sys.stdout.write(json.dumps({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "content": [{"type": "text", "text": json.dumps(payload)}]
            }
        }) + "\n")
        sys.stdout.flush()
"#,
        )
        .unwrap();

        let spec_path = dir.path().join("mcp-im.toml");
        std::fs::write(
            &spec_path,
            format!(
                r#"
platform = "custom-im"

[mcp]
command = "python3"
args = ["-u", "{}"]

[conversations.list]
tool = "list_rooms"
items_pointer = "/rooms"
id = "/id"
title = "/name"
kind = "/kind"

[inbound.poll]
tool = "poll_msgs"
every_secs = 1
args = {{ cursor = "{{{{cursor}}}}" }}
cursor_pointer = "/next_cursor"
items_pointer = "/items"
id = "/id"
conversation = "/room"
text = "/text"
sender_id = "/user"
sender_name = "/user_name"

[outbound.send]
tool = "send_msg"
args = {{ room = "{{{{conversation}}}}", text = "{{{{text}}}}", reply_to = "{{{{reply_to}}}}" }}
message_id_pointer = "/created_id"
"#,
                script_path.display()
            ),
        )
        .unwrap();

        let spec = McpImSpec::load(&spec_path).expect("load spec");
        let mut backend = McpImBackend::from_spec("custom1", spec).await;
        assert!(backend.hello().authenticated);

        // 1. list_conversations
        let convs = backend
            .list_conversations()
            .await
            .expect("list_conversations");
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].id, "room-1");
        assert_eq!(convs[0].title.as_deref(), Some("General"));

        // 2. send (records `m-out-1` in `echo_ids` so poll will ignore it)
        let sent = backend
            .send(SendArgs {
                conversation: "room-1".into(),
                payload: ImPayload {
                    text: "outbound reply".into(),
                    format: ImFormat::Plain,
                    ..Default::default()
                },
            })
            .await
            .expect("send");
        assert_eq!(sent.message_id.as_deref(), Some("m-out-1"));

        // 3. next_event triggers poll: receives `m-in-1` and skips echo `m-out-1`
        let ev = backend
            .next_event()
            .await
            .expect("next_event")
            .expect("one event");
        assert_eq!(ev.id, "m-in-1");
        assert_eq!(ev.conversation, "room-1");
        assert_eq!(ev.text.as_deref(), Some("ping from mcp"));
        assert!(backend.queue.is_empty(), "echo m-out-1 was suppressed");
    }
}
