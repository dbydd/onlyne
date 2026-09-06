use crate::{app::App, core::*};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::Mutex,
};
use tracing::info;

/// Default per-tier wait for a `consume` verdict before lower tiers run.
const DEFAULT_CONSUME_TIMEOUT_MS: u64 = 150;

#[derive(Debug, Deserialize)]
pub struct Request {
    pub id: Option<String>,
    pub op: String,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub format: Option<MessageFormat>,
    #[serde(default)]
    pub raw_text: bool,
    #[serde(default)]
    pub attachments: Vec<AttachmentRef>,
    #[serde(default)]
    pub limit: Option<u32>,
    /// Priority delivery tier for `subscribe_events`.
    ///
    /// Subscribers carry a `priority` number; the daemon relays each event
    /// to subscriber tiers from highest to lowest, waiting
    /// `consume_timeout_ms` per tier. Any connection in the tier may reply
    /// with the `consume` op to stop delivery to lower tiers.
    /// Default 0 preserves the legacy broadcast-to-all behavior.
    #[serde(default)]
    pub priority: u32,
    /// Milliseconds the daemon waits per priority tier for a `consume`
    /// verdict before continuing to the next lower tier. Only read from
    /// `subscribe_events`. Defaults to 150ms.
    #[serde(default)]
    pub consume_timeout_ms: Option<u64>,
    /// Consumed verdict for the `consume` op: the referenced event was
    /// handled and lower-priority tiers must not receive it. Carries the
    /// `event_seq` of the consumed event line.
    #[serde(default)]
    pub event_seq: Option<u64>,
}
#[derive(Serialize)]
struct Resp<'a> {
    id: &'a Option<String>,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

pub async fn bind_socket(path: &Path) -> anyhow::Result<UnixListener> {
    if path.exists() {
        match UnixStream::connect(path).await {
            Ok(_) => anyhow::bail!("onlyne daemon already running at {}", path.display()),
            Err(_) => {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))
}

pub async fn serve_socket(app: Arc<App>) -> anyhow::Result<()> {
    let path = app.workspace.socket_path();
    let listener = bind_socket(&path).await?;
    serve_bound_socket(app, listener, &path).await
}

pub async fn serve_bound_socket(
    app: Arc<App>,
    listener: UnixListener,
    path: &Path,
) -> anyhow::Result<()> {
    info!(socket = %path.display(), "ipc socket listening");
    loop {
        let (s, _) = listener.accept().await?;
        let a = app.clone();
        tokio::spawn(async move {
            let _ = handle_stream(a, s).await;
        });
    }
}
pub async fn handle_stdio(app: Arc<App>) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    handle_rw(app, stdin, stdout).await
}
async fn handle_stream(app: Arc<App>, s: UnixStream) -> anyhow::Result<()> {
    let (r, w) = s.into_split();
    handle_rw(app, r, w).await
}
async fn handle_rw<R, W>(app: Arc<App>, reader: R, writer: W) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut lines = BufReader::new(reader).lines();
    let writer = Arc::new(Mutex::new(writer));
    let mut event_task = None;
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let req: Result<Request, _> = serde_json::from_str(&line);
        match req {
            Ok(req) => {
                if req.op == "consume" {
                    let Some(seq) = req.event_seq else {
                        write(
                            &writer,
                            &Resp {
                                id: &req.id,
                                ok: false,
                                data: None,
                                error: Some(json!({"code":"error","message":"event_seq required"})),
                            },
                        )
                        .await?;
                        continue;
                    };
                    app.mark_consumed(seq).await;
                    write(
                        &writer,
                        &Resp {
                            id: &req.id,
                            ok: true,
                            data: Some(json!({"consumed":seq})),
                            error: None,
                        },
                    )
                    .await?;
                } else if req.op == "subscribe_events" {
                    let priority = req.priority;
                    let timeout = req.consume_timeout_ms.unwrap_or(DEFAULT_CONSUME_TIMEOUT_MS);
                    let w = writer.clone();
                    let pump_app = app.clone();
                    event_task = Some(tokio::spawn(async move {
                        pump_app
                            .pump_subscription(priority, timeout, w)
                            .await;
                    }));
                    write(
                        &writer,
                        &Resp {
                            id: &req.id,
                            ok: true,
                            data: Some(json!({"subscribed":true})),
                            error: None,
                        },
                    )
                    .await?;
                } else if req.op == "unsubscribe_events" {
                    if let Some(t) = event_task.take() {
                        t.abort();
                    }
                    write(
                        &writer,
                        &Resp {
                            id: &req.id,
                            ok: true,
                            data: Some(json!({"subscribed":false})),
                            error: None,
                        },
                    )
                    .await?;
                } else if req.op == "shutdown" {
                    write(
                        &writer,
                        &Resp {
                            id: &req.id,
                            ok: true,
                            data: Some(json!({"shutting_down":true})),
                            error: None,
                        },
                    )
                    .await?;
                    tokio::spawn(async {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        std::process::exit(0);
                    });
                } else {
                    let id = req.id.clone();
                    match app.handle(req).await {
                        Ok(data) => {
                            write(
                                &writer,
                                &Resp {
                                    id: &id,
                                    ok: true,
                                    data: Some(data),
                                    error: None,
                                },
                            )
                            .await?
                        }
                        Err(e) => {
                            write(
                                &writer,
                                &Resp {
                                    id: &id,
                                    ok: false,
                                    data: None,
                                    error: Some(json!({"code":"error","message":e.to_string()})),
                                },
                            )
                            .await?
                        }
                    }
                }
            }
            Err(e) => {
                let id = None;
                write(
                    &writer,
                    &Resp {
                        id: &id,
                        ok: false,
                        data: None,
                        error: Some(json!({"code":"bad_json","message":e.to_string()})),
                    },
                )
                .await?
            }
        }
    }
    Ok(())
}
async fn write<W: AsyncWrite + Unpin>(w: &Arc<Mutex<W>>, r: &Resp<'_>) -> anyhow::Result<()> {
    let mut g = w.lock().await;
    g.write_all(serde_json::to_string(r)?.as_bytes()).await?;
    g.write_all(b"\n").await?;
    g.flush().await?;
    Ok(())
}
pub(crate) fn event_type(ev: &Event) -> &'static str {
    match ev {
        Event::InboundMessage(_) => "inbound_message",
        Event::OutboundMessage(_) => "outbound_message",
        Event::DeliveryUpdate { .. } => "delivery_update",
        Event::AdapterStarted { .. } => "adapter_started",
        Event::AdapterStopped { .. } => "adapter_stopped",
        Event::AdapterReconnecting { .. } => "adapter_reconnecting",
        Event::AdapterFailed { .. } => "adapter_failed",
        Event::HistoryAppended { .. } => "history_appended",
        Event::WorkspaceStateChanged { .. } => "workspace_state_changed",
        Event::Warning { .. } => "warning",
        Event::Error { .. } => "error",
    }
}

#[cfg(test)]
mod priority_tests {
    use super::*;
    use crate::{app::App, workspace::Workspace};

    #[test]
    fn request_parses_priority_and_timeout() {
        let r: Request = serde_json::from_str(
            r#"{"id":"s","op":"subscribe_events","priority":4294967295,"consume_timeout_ms":50}"#,
        )
        .unwrap();
        assert_eq!(r.priority, u32::MAX);
        assert_eq!(r.consume_timeout_ms, Some(50));
        // Legacy subscriptions default to tier 0 with no timeout override.
        let r: Request =
            serde_json::from_str(r#"{"id":"s","op":"subscribe_events"}"#).unwrap();
        assert_eq!(r.priority, 0);
        assert_eq!(r.consume_timeout_ms, None);
        // consume verdict requires an event_seq.
        let r: Request =
            serde_json::from_str(r#"{"id":"c","op":"consume","event_seq":41}"#).unwrap();
        assert_eq!(r.event_seq, Some(41));
    }

    #[tokio::test]
    async fn consumed_seq_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::resolve(dir.path());
        let app = App::load(ws).await.unwrap();
        app.mark_consumed(7).await;
        assert!(app.consumed_seqs.lock().await.contains(&7));
    }

    #[tokio::test]
    async fn subscriber_tiers_sort_high_first() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::resolve(dir.path());
        let app = App::load(ws).await.unwrap();
        let _lo = app.register_subscriber(0).await;
        let _hi = app.register_subscriber(99).await;
        let subs = app.subscribers.lock().await;
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].priority, 99);
        assert_eq!(subs[1].priority, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_fails() {
        assert!(serde_json::from_str::<Request>("{").is_err());
    }
    #[test]
    fn request_parses() {
        let r: Request = serde_json::from_str(r#"{"id":"1","op":"ping"}"#).unwrap();
        assert_eq!(r.op, "ping");
        assert_eq!(r.format, None);
        assert!(!r.raw_text);
    }

    #[test]
    fn request_parses_markdown_format() {
        let r: Request = serde_json::from_str(
            r##"{"id":"1","op":"send_message","conversation_id":"group:g1","text":"# hi","format":"markdown"}"##,
        )
        .unwrap();
        assert_eq!(r.format, Some(MessageFormat::Markdown));
        assert_eq!(r.conversation_id.as_deref(), Some("group:g1"));
    }

    #[test]
    fn request_parses_raw_text() {
        let r: Request = serde_json::from_str(
            r##"{"id":"1","op":"send_message","text":"# literal","raw_text":true}"##,
        )
        .unwrap();
        assert!(r.raw_text);
    }
}
