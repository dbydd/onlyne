//! Adapter SDK for the Onlyne v1 local socket.
//!
//! One protocol is mounted by agent plugins on role client sockets and by
//! platform gateways on server sockets.

pub mod conn {
    pub use super::{
        AdapterClient, AdapterError, AdapterIo, AdapterServer, IncomingFrame, Result,
        ServerConnection,
    };
}

pub mod handshake {
    pub use super::{AdapterServer, HELLO_TIMEOUT, reject_late_hello};
}

pub mod caps {
    pub use super::{CapabilitySet, HostGap, degrade_for};
}

pub mod report {
    pub use super::{ReportSender, accept_report_generation};
}

pub mod dispatch {
    pub use super::{Host, HostDispatcher};
}

pub mod plugin;
pub use plugin::{
    AdapterHealth, GatewayHost, GatewayPlugin, OnboardingKind, OnboardingPrompt, Outbound,
    SendReceipt,
};

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::Stream;
use onlyne_frame::{read_frame, write_frame};
use onlyne_proto::{
    AdapterMsg, AgentMount, AssignAckArgs, AssignArgs, Body, ConfigGetArgs, Delivery, DetachArgs,
    Envelope, ErrorCode, GatewayHealth, GatewayMount, HELLO_REQUIRED_MESSAGE, HealthArgs, HostOp,
    ImagePart, Outcome, PluginOp, Principal, Receipt, RegisterChannelArgs, RenderSendArgs, Report,
    ResBody, SessionRegisterArgs, TypingArgs,
};
pub use onlyne_proto::{Capability, HelloAck, HelloArgs, Mount, MountKind, PROTOCOL_VERSION};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf, split};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::timeout;
use tracing::warn;

pub mod prelude {
    pub use crate::{
        AdapterClient, AdapterError, AdapterHealth, AdapterIo, AdapterServer, AgentHandle,
        AgentSurface, CapabilitySet, GatewayHandle, GatewayHost, GatewayPlugin, Host,
        HostDispatcher, HostGap, IncomingFrame, MountKind, OnboardingKind, OnboardingPrompt,
        Outbound, ReportSender, SendReceipt, SurfaceGap, SurfaceGaps, WakeUser,
        accept_report_generation, degrade_for,
    };
    pub use onlyne_proto::{
        AdapterMsg, AgentMount, AssignAckArgs, AssignArgs, ByeNotice, Capability, ConfigGetArgs,
        Delivery, DetachArgs, Envelope, ErrorCode, GatewayHealth, GatewayMount, HealthArgs,
        HelloAck, HelloArgs, HostOp, Mount, Outcome, PluginOp, Principal, Receipt, RecycleArgs,
        RegisterChannelArgs, RenderSendArgs, Report, ResBody, ServerInfo, SessionRegisterArgs,
        TypingArgs,
    };
}

pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum AdapterError {
    Io(std::io::Error),
    Serde(serde_json::Error),
    Closed,
    Timeout(&'static str),
    Protocol(ResBody),
    Code { code: ErrorCode, message: String },
    Unexpected(String),
    RecvDropped,
    SendDropped,
}

impl AdapterError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        AdapterError::Code {
            code,
            message: message.into(),
        }
    }

    pub fn with_code(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(code, message)
    }
}

impl AdapterError {
    fn wire(code: ErrorCode, message: impl Into<String>, field: Option<&str>) -> Self {
        AdapterError::Protocol(ResBody::err(
            code,
            message,
            field.map(std::string::ToString::to_string),
        ))
    }

    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            AdapterError::Protocol(body) => body.error.as_ref().map(|e| e.code),
            AdapterError::Code { code, .. } => Some(*code),
            _ => None,
        }
    }

    pub fn error_code(&self) -> Option<ErrorCode> {
        self.code()
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdapterError::Io(err) => write!(f, "adapter io error: {err}"),
            AdapterError::Serde(err) => write!(f, "adapter json error: {err}"),
            AdapterError::Closed => f.write_str("adapter connection closed"),
            AdapterError::Timeout(op) => write!(f, "adapter {op} timed out"),
            AdapterError::Protocol(body) => match &body.error {
                Some(err) => write!(f, "adapter error {}: {}", err.code, err.message),
                None => f.write_str("adapter error"),
            },
            AdapterError::Code { code, message } => write!(f, "adapter error {code}: {message}"),
            AdapterError::Unexpected(msg) => write!(f, "adapter unexpected message: {msg}"),
            AdapterError::RecvDropped => f.write_str("adapter reader task dropped"),
            AdapterError::SendDropped => f.write_str("adapter writer task dropped"),
        }
    }
}

impl std::error::Error for AdapterError {}

impl From<std::io::Error> for AdapterError {
    fn from(err: std::io::Error) -> Self {
        AdapterError::Io(err)
    }
}

impl From<serde_json::Error> for AdapterError {
    fn from(err: serde_json::Error) -> Self {
        AdapterError::Serde(err)
    }
}

pub type Result<T, E = AdapterError> = std::result::Result<T, E>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct WireMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<u64>,
    #[serde(flatten)]
    pub msg: AdapterMsg,
}

impl WireMessage {
    fn request(id: Option<u64>, msg: AdapterMsg) -> Self {
        WireMessage {
            id,
            reply_to: None,
            msg,
        }
    }

    fn response(reply_to: u64, body: ResBody) -> Self {
        WireMessage {
            id: None,
            reply_to: Some(reply_to),
            msg: AdapterMsg::Res(body),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IncomingFrame {
    pub id: Option<u64>,
    pub reply_to: Option<u64>,
    pub msg: AdapterMsg,
}

struct QueuedFrame {
    id: Option<u64>,
    reply_to: Option<u64>,
    msg: AdapterMsg,
}

#[derive(Clone)]
pub struct AdapterIo {
    tx: mpsc::Sender<QueuedFrame>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<ResBody>>>>,
    id: Arc<AtomicU64>,
    read_timeout: Duration,
    write_timeout: Duration,
}

impl AdapterIo {
    pub fn new<S>(stream: S, read_timeout: Duration, write_timeout: Duration) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (io, _) = Self::new_with_inbound(stream, read_timeout, write_timeout);
        io
    }

    pub fn new_with_inbound<S>(
        stream: S,
        read_timeout: Duration,
        write_timeout: Duration,
    ) -> (Self, mpsc::Receiver<IncomingFrame>)
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (reader, writer) = split(stream);
        let (tx, rx) = mpsc::channel(128);
        let (incoming_tx, incoming_rx) = mpsc::channel(128);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        tokio::spawn(writer_loop(writer, rx, write_timeout));
        tokio::spawn(reader_loop(
            reader,
            read_timeout,
            pending.clone(),
            incoming_tx,
        ));
        (
            AdapterIo {
                tx,
                pending,
                id: Arc::new(AtomicU64::new(1)),
                read_timeout,
                write_timeout,
            },
            incoming_rx,
        )
    }

    pub fn read_timeout(&self) -> Duration {
        self.read_timeout
    }

    pub fn write_timeout(&self) -> Duration {
        self.write_timeout
    }

    pub async fn notify(&self, msg: AdapterMsg) -> Result<()> {
        self.tx
            .send(QueuedFrame {
                id: None,
                reply_to: None,
                msg,
            })
            .await
            .map_err(|_| AdapterError::SendDropped)
    }

    pub async fn respond(&self, reply_to: u64, body: ResBody) -> Result<()> {
        self.tx
            .send(QueuedFrame {
                id: None,
                reply_to: Some(reply_to),
                msg: AdapterMsg::Res(body),
            })
            .await
            .map_err(|_| AdapterError::SendDropped)
    }

    pub async fn request(&self, msg: AdapterMsg) -> Result<ResBody> {
        let id = self.id.fetch_add(1, Ordering::SeqCst);
        let (response_tx, response_rx) = oneshot::channel();
        self.pending.lock().await.insert(id, response_tx);
        if self
            .tx
            .send(QueuedFrame {
                id: Some(id),
                reply_to: None,
                msg,
            })
            .await
            .is_err()
        {
            self.pending.lock().await.remove(&id);
            return Err(AdapterError::SendDropped);
        }
        match timeout(self.read_timeout, response_rx).await {
            Ok(Ok(body)) => Ok(body),
            Ok(Err(_)) => Err(AdapterError::RecvDropped),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(AdapterError::Timeout("response"))
            }
        }
    }

    pub async fn request_ok(&self, msg: AdapterMsg) -> Result<Value> {
        let body = self.request(msg).await?;
        if body.ok {
            Ok(body.data.unwrap_or(Value::Null))
        } else {
            Err(AdapterError::Protocol(body))
        }
    }
}

async fn writer_loop<W>(
    mut writer: WriteHalf<W>,
    mut rx: mpsc::Receiver<QueuedFrame>,
    write_timeout: Duration,
) where
    W: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    while let Some(outbound) = rx.recv().await {
        let message = match outbound.reply_to {
            Some(reply_to) => match outbound.msg {
                AdapterMsg::Res(body) => WireMessage::response(reply_to, body),
                msg => WireMessage {
                    id: outbound.id,
                    reply_to: Some(reply_to),
                    msg,
                },
            },
            None => WireMessage::request(outbound.id, outbound.msg),
        };
        if timeout(write_timeout, write_frame(&mut writer, &message))
            .await
            .is_err()
        {
            break;
        }
    }
}

async fn reader_loop<R>(
    mut reader: ReadHalf<R>,
    read_timeout: Duration,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<ResBody>>>>,
    incoming: mpsc::Sender<IncomingFrame>,
) where
    R: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    loop {
        let read = timeout(read_timeout, read_frame::<_, WireMessage>(&mut reader)).await;
        let wire = match read {
            Ok(Ok(Some(wire))) => wire,
            Ok(Ok(None)) | Ok(Err(_)) | Err(_) => break,
        };
        let id = wire.id;
        let reply_to = wire.reply_to;
        match wire.msg {
            AdapterMsg::Res(body) => {
                if let Some(reply_id) = reply_to {
                    if let Some(waiter) = pending.lock().await.remove(&reply_id) {
                        let _ = waiter.send(body);
                        continue;
                    }
                }
                if incoming
                    .send(IncomingFrame {
                        id,
                        reply_to,
                        msg: AdapterMsg::Res(body),
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            msg => {
                if incoming
                    .send(IncomingFrame { id, reply_to, msg })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

pub struct AdapterClient;

impl AdapterClient {
    pub async fn connect_unix(path: impl AsRef<Path>) -> Result<AgentHandle> {
        let stream = UnixStream::connect(path).await?;
        Ok(Self::connect(stream))
    }

    pub fn connect<S>(stream: S) -> AgentHandle
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        Self::connect_with_timeouts(stream, DEFAULT_READ_TIMEOUT, DEFAULT_WRITE_TIMEOUT)
    }

    pub fn connect_with_timeouts<S>(
        stream: S,
        read_timeout: Duration,
        write_timeout: Duration,
    ) -> AgentHandle
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (io, inbound) = AdapterIo::new_with_inbound(stream, read_timeout, write_timeout);
        AgentHandle::new(io, inbound)
    }

    pub fn gateway<S>(stream: S) -> GatewayHandle
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        Self::gateway_with_timeouts(stream, DEFAULT_READ_TIMEOUT, DEFAULT_WRITE_TIMEOUT)
    }

    pub fn gateway_with_timeouts<S>(
        stream: S,
        read_timeout: Duration,
        write_timeout: Duration,
    ) -> GatewayHandle
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (io, inbound) = AdapterIo::new_with_inbound(stream, read_timeout, write_timeout);
        GatewayHandle::new(io, inbound)
    }
}

pub struct AdapterServer;

impl AdapterServer {
    pub async fn accept_unix<F>(stream: UnixStream, welcome: F) -> Result<ServerConnection>
    where
        F: FnOnce(&HelloArgs) -> std::result::Result<HelloAck, (ErrorCode, String)> + Send,
    {
        Self::accept(stream, welcome).await
    }

    pub async fn accept<S, F>(stream: S, welcome: F) -> Result<ServerConnection>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: FnOnce(&HelloArgs) -> std::result::Result<HelloAck, (ErrorCode, String)> + Send,
    {
        Self::accept_with_timeouts(
            stream,
            DEFAULT_READ_TIMEOUT,
            DEFAULT_WRITE_TIMEOUT,
            None,
            welcome,
        )
        .await
    }

    pub async fn accept_with_timeouts<S, F>(
        mut stream: S,
        read_timeout: Duration,
        write_timeout: Duration,
        peer_pid: Option<u32>,
        welcome: F,
    ) -> Result<ServerConnection>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: FnOnce(&HelloArgs) -> std::result::Result<HelloAck, (ErrorCode, String)> + Send,
    {
        let first = match timeout(HELLO_TIMEOUT, read_frame::<_, WireMessage>(&mut stream)).await {
            Ok(Ok(Some(wire))) => wire,
            Ok(Ok(None)) => return Err(AdapterError::Closed),
            Ok(Err(err)) => return Err(AdapterError::Io(err)),
            Err(_) => {
                if let Some(pid) = peer_pid {
                    warn!(peer_pid = pid, "adapter hello timeout elapsed");
                } else {
                    warn!("adapter hello timeout elapsed");
                }
                return Err(AdapterError::Timeout("hello"));
            }
        };
        let hello = match first.msg {
            AdapterMsg::Plugin(PluginOp::Hello(args)) => args,
            _ => {
                let body = ResBody::err(
                    ErrorCode::Invalid,
                    HELLO_REQUIRED_MESSAGE,
                    Some("op".to_string()),
                );
                let reply_to = first.id.unwrap_or_default();
                let message = WireMessage::response(reply_to, body);
                let _ = timeout(write_timeout, write_frame(&mut stream, &message)).await;
                return Err(AdapterError::wire(
                    ErrorCode::Invalid,
                    HELLO_REQUIRED_MESSAGE,
                    Some("op"),
                ));
            }
        };
        let ack = match welcome(&hello) {
            Ok(ack) => ack,
            Err((code, message)) => {
                let body = ResBody::err(code, message, None);
                let reply_to = first.id.unwrap_or_default();
                let wire = WireMessage::response(reply_to, body.clone());
                let _ = timeout(write_timeout, write_frame(&mut stream, &wire)).await;
                return Err(AdapterError::Protocol(body));
            }
        };
        let data = serde_json::to_value(HostOp::Welcome(ack.clone()))?;
        let wire = WireMessage::response(first.id.unwrap_or_default(), ResBody::ok(data));
        timeout(write_timeout, write_frame(&mut stream, &wire))
            .await
            .map_err(|_| AdapterError::Timeout("write"))??;
        let (io, inbound) = AdapterIo::new_with_inbound(stream, read_timeout, write_timeout);
        Ok(ServerConnection {
            hello,
            ack,
            io,
            inbound,
        })
    }

    pub async fn accept_async<S, F, Fut>(mut stream: S, welcome: F) -> Result<ServerConnection>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: FnOnce(HelloArgs) -> Fut + Send,
        Fut: Future<Output = std::result::Result<HelloAck, (ErrorCode, String)>> + Send,
    {
        let first = match timeout(HELLO_TIMEOUT, read_frame::<_, WireMessage>(&mut stream)).await {
            Ok(Ok(Some(wire))) => wire,
            Ok(Ok(None)) => return Err(AdapterError::Closed),
            Ok(Err(err)) => return Err(AdapterError::Io(err)),
            Err(_) => {
                warn!("adapter hello timeout elapsed");
                return Err(AdapterError::Timeout("hello"));
            }
        };
        Self::accept_from_first(stream, first, welcome).await
    }

    /// Finish a handshake whose first frame the caller already read.
    ///
    /// A socket that serves two vocabularies reads the opening frame to tell
    /// them apart, then hands the adapter frame here (plan §7 line 293).
    pub async fn accept_from_first<S, F, Fut>(
        mut stream: S,
        first: WireMessage,
        welcome: F,
    ) -> Result<ServerConnection>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: FnOnce(HelloArgs) -> Fut + Send,
        Fut: Future<Output = std::result::Result<HelloAck, (ErrorCode, String)>> + Send,
    {
        let hello = match first.msg {
            AdapterMsg::Plugin(PluginOp::Hello(args)) => args,
            _ => {
                let body = ResBody::err(
                    ErrorCode::Invalid,
                    HELLO_REQUIRED_MESSAGE,
                    Some("op".to_string()),
                );
                let wire = WireMessage::response(first.id.unwrap_or_default(), body);
                let _ = timeout(DEFAULT_WRITE_TIMEOUT, write_frame(&mut stream, &wire)).await;
                return Err(AdapterError::wire(
                    ErrorCode::Invalid,
                    HELLO_REQUIRED_MESSAGE,
                    Some("op"),
                ));
            }
        };
        let ack = match welcome(hello.clone()).await {
            Ok(ack) => ack,
            Err((code, message)) => {
                let body = ResBody::err(code, message, None);
                let wire = WireMessage::response(first.id.unwrap_or_default(), body.clone());
                let _ = timeout(DEFAULT_WRITE_TIMEOUT, write_frame(&mut stream, &wire)).await;
                return Err(AdapterError::Protocol(body));
            }
        };
        let data = serde_json::to_value(HostOp::Welcome(ack.clone()))?;
        let wire = WireMessage::response(first.id.unwrap_or_default(), ResBody::ok(data));
        timeout(DEFAULT_WRITE_TIMEOUT, write_frame(&mut stream, &wire))
            .await
            .map_err(|_| AdapterError::Timeout("write"))??;
        let (io, inbound) =
            AdapterIo::new_with_inbound(stream, DEFAULT_READ_TIMEOUT, DEFAULT_WRITE_TIMEOUT);
        Ok(ServerConnection {
            hello,
            ack,
            io,
            inbound,
        })
    }
}

pub struct ServerConnection {
    pub hello: HelloArgs,
    pub ack: HelloAck,
    pub io: AdapterIo,
    pub inbound: mpsc::Receiver<IncomingFrame>,
}

pub async fn reject_late_hello<S>(mut stream: S, peer_pid: Option<u32>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::sleep(HELLO_TIMEOUT).await;
    if let Ok(Ok(Some(_))) = timeout(
        Duration::from_millis(1),
        read_frame::<_, WireMessage>(&mut stream),
    )
    .await
    {
        if let Some(pid) = peer_pid {
            warn!(peer_pid = pid, "adapter hello timeout elapsed");
        } else {
            warn!("adapter hello timeout elapsed");
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilitySet {
    caps: Vec<Capability>,
}

impl CapabilitySet {
    pub fn new(caps: impl Into<Vec<Capability>>) -> Self {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for cap in caps.into() {
            if seen.insert(cap) {
                out.push(cap);
            }
        }
        CapabilitySet { caps: out }
    }

    pub fn as_slice(&self) -> &[Capability] {
        &self.caps
    }

    pub fn has(&self, capability: Capability) -> bool {
        self.caps.contains(&capability)
    }

    pub fn missing(&self, expected: &[Capability]) -> Vec<Capability> {
        expected
            .iter()
            .copied()
            .filter(|cap| !self.has(*cap))
            .collect()
    }

    pub fn require(&self, expected: &[Capability]) -> std::result::Result<(), (ErrorCode, String)> {
        let missing = self.missing(expected);
        if missing.is_empty() {
            Ok(())
        } else {
            let names = missing
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            Err((ErrorCode::Forbidden, format!("missing capability: {names}")))
        }
    }
}

impl AdapterClient {
    pub async fn connect_gateway_unix(path: impl AsRef<Path>) -> Result<GatewayHandle> {
        let stream = UnixStream::connect(path).await?;
        Ok(Self::gateway(stream))
    }
}

impl From<Vec<Capability>> for CapabilitySet {
    fn from(caps: Vec<Capability>) -> Self {
        CapabilitySet::new(caps)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostGap {
    pub capability: Capability,
    pub action: &'static str,
}

pub fn degrade_for(missing: &[Capability]) -> Vec<HostGap> {
    missing
        .iter()
        .filter_map(|capability| match capability {
            Capability::Recycle => Some(HostGap {
                capability: *capability,
                action: "recycle missing means the host judges resource loss through probe",
            }),
            Capability::Report => Some(HostGap {
                capability: *capability,
                action: "report missing means the affected session moves to idle_fault with a recorded fault",
            }),
            Capability::Inject => Some(HostGap {
                capability: *capability,
                action: "assign missing means the payload travels through process stdin or argv and the terminal state comes from the exit code plus the last output line",
            }),
            _ => None,
        })
        .collect()
}

#[derive(Clone)]
pub struct ReportSender {
    io: AdapterIo,
    generation: Arc<AtomicU64>,
    seq: Arc<AtomicU64>,
    session_id: Arc<Mutex<Option<String>>>,
}

impl ReportSender {
    pub fn new(io: AdapterIo, generation: u64) -> Self {
        ReportSender {
            io,
            generation: Arc::new(AtomicU64::new(generation)),
            seq: Arc::new(AtomicU64::new(0)),
            session_id: Arc::new(Mutex::new(None)),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn bump_generation(&self) -> u64 {
        self.seq.store(0, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub async fn bind_session(&self, session_id: impl Into<String>) {
        *self.session_id.lock().await = Some(session_id.into());
    }

    pub async fn ready(
        &self,
        task_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<Report> {
        let report = Report::Ready {
            task_id: task_id.into(),
            session_id: session_id.into(),
            generation: self.generation(),
            seq: self.next_seq(),
            // A local report has no origin cluster; a connection speaking for a
            // sub-cluster carries that name in `hello`'s `Mount::Cluster`.
            cluster_ref: None,
        };
        self.send(report.clone()).await?;
        Ok(report)
    }

    pub async fn heartbeat(&self, task_id: impl Into<String>, observed: Value) -> Result<Report> {
        let report = Report::Heartbeat {
            task_id: task_id.into(),
            generation: self.generation(),
            seq: self.next_seq(),
            observed,
            cluster_ref: None,
        };
        self.send(report.clone()).await?;
        Ok(report)
    }

    pub async fn complete(
        &self,
        task_id: impl Into<String>,
        outcome: Outcome,
        head: Option<String>,
    ) -> Result<Report> {
        let report = Report::Complete {
            task_id: task_id.into(),
            outcome,
            head,
            reply_to: None,
            cluster_ref: None,
        };
        self.send(report.clone()).await?;
        Ok(report)
    }

    pub async fn fault(
        &self,
        task_id: Option<String>,
        kind: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<Report> {
        let report = Report::Fault {
            task_id,
            session_id: self.session_id.lock().await.clone(),
            generation: Some(self.generation()),
            seq: Some(self.next_seq()),
            kind: kind.into(),
            reason: reason.into(),
            desired: None,
            observed: None,
        };
        self.send(report.clone()).await?;
        Ok(report)
    }

    pub async fn send(&self, report: Report) -> Result<()> {
        self.io
            .request_ok(AdapterMsg::Plugin(PluginOp::Report(report)))
            .await
            .map(|_| ())
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::SeqCst) + 1
    }
}

pub fn accept_report_generation(watermark: (u64, u64), incoming: (u64, u64)) -> bool {
    incoming.0 > watermark.0 || (incoming.0 == watermark.0 && incoming.1 > watermark.1)
}

#[async_trait]
pub trait Host: Send + Sync {
    async fn hello(&self, args: &HelloArgs) -> std::result::Result<HelloAck, (ErrorCode, String)>;

    async fn report(&self, _report: &Report) -> std::result::Result<(), (ErrorCode, String)> {
        Err((ErrorCode::UnknownOp, "report is unsupported".to_string()))
    }

    async fn session_register(
        &self,
        _args: &SessionRegisterArgs,
    ) -> std::result::Result<(), (ErrorCode, String)> {
        Err((
            ErrorCode::UnknownOp,
            "session_register is unsupported".to_string(),
        ))
    }

    async fn assign_ack(
        &self,
        _ack: &AssignAckArgs,
    ) -> std::result::Result<(), (ErrorCode, String)> {
        Err((
            ErrorCode::UnknownOp,
            "assign_ack is unsupported".to_string(),
        ))
    }

    async fn send(
        &self,
        _envelope: &Envelope,
    ) -> std::result::Result<Receipt, (ErrorCode, String)> {
        Err((ErrorCode::UnknownOp, "send is unsupported".to_string()))
    }

    async fn deliver(&self, _delivery: &Delivery) -> std::result::Result<(), (ErrorCode, String)> {
        Err((ErrorCode::UnknownOp, "deliver is unsupported".to_string()))
    }

    async fn register_channel(
        &self,
        _args: &RegisterChannelArgs,
    ) -> std::result::Result<(), (ErrorCode, String)> {
        Err((
            ErrorCode::UnknownOp,
            "register_channel is unsupported".to_string(),
        ))
    }

    async fn health(&self, _args: &HealthArgs) -> std::result::Result<(), (ErrorCode, String)> {
        Err((ErrorCode::UnknownOp, "health is unsupported".to_string()))
    }

    async fn typing(&self, _args: &TypingArgs) -> std::result::Result<(), (ErrorCode, String)> {
        Err((ErrorCode::UnknownOp, "typing is unsupported".to_string()))
    }

    async fn detach(&self, _args: &DetachArgs) -> std::result::Result<(), (ErrorCode, String)> {
        Err((ErrorCode::UnknownOp, "detach is unsupported".to_string()))
    }
}

pub struct HostDispatcher<H> {
    mount: MountKind,
    host: Arc<H>,
}

impl<H> HostDispatcher<H>
where
    H: Host,
{
    pub fn new(mount: MountKind, host: Arc<H>) -> Self {
        HostDispatcher { mount, host }
    }

    pub async fn dispatch(&self, op: PluginOp) -> ResBody {
        if let Err(message) = self.enforce(&op) {
            return ResBody::err(ErrorCode::Forbidden, message, Some("op".to_string()));
        }
        result_to_body(match op {
            PluginOp::Hello(args) => self.host.hello(&args).await.map(|ack| json!(ack)),
            PluginOp::Report(report) => self.host.report(&report).await.map(|_| Value::Null),
            PluginOp::SessionRegister(args) => {
                self.host.session_register(&args).await.map(|_| Value::Null)
            }
            PluginOp::AssignAck(ack) => self.host.assign_ack(&ack).await.map(|_| Value::Null),
            PluginOp::Send(envelope) => match envelope.validate() {
                Ok(()) => self
                    .host
                    .send(&envelope)
                    .await
                    .map(|receipt| json!(receipt)),
                Err(err) => {
                    return ResBody::err(
                        ErrorCode::Invalid,
                        err.message().to_string(),
                        Some(err.field().to_string()),
                    );
                }
            },
            PluginOp::Deliver(delivery) => self.host.deliver(&delivery).await.map(|_| Value::Null),
            PluginOp::RegisterChannel(args) => {
                self.host.register_channel(&args).await.map(|_| Value::Null)
            }
            PluginOp::Health(args) => self.host.health(&args).await.map(|_| Value::Null),
            PluginOp::Typing(args) => self.host.typing(&args).await.map(|_| Value::Null),
            PluginOp::Detach(args) => self.host.detach(&args).await.map(|_| Value::Null),
        })
    }

    pub async fn serve(
        &self,
        io: AdapterIo,
        mut inbound: mpsc::Receiver<IncomingFrame>,
    ) -> Result<()> {
        while let Some(frame) = inbound.recv().await {
            if let AdapterMsg::Plugin(op) = frame.msg {
                let body = self.dispatch(op).await;
                if let Some(id) = frame.id {
                    io.respond(id, body).await?;
                } else {
                    io.notify(AdapterMsg::Res(body)).await?;
                }
            }
        }
        Ok(())
    }

    fn enforce(&self, op: &PluginOp) -> std::result::Result<(), String> {
        let allowed = match self.mount {
            MountKind::Agent => matches!(
                op,
                PluginOp::Report(_)
                    | PluginOp::SessionRegister(_)
                    | PluginOp::AssignAck(_)
                    | PluginOp::Send(_)
                    | PluginOp::Detach(_)
            ),
            MountKind::Gateway => matches!(
                op,
                PluginOp::Deliver(_)
                    | PluginOp::RegisterChannel(_)
                    | PluginOp::Health(_)
                    | PluginOp::Typing(_)
                    | PluginOp::Detach(_)
            ),
            MountKind::Admin => false,
        };
        if allowed {
            Ok(())
        } else {
            Err(format!(
                "op {} forbidden on {:?} mount",
                op.name(),
                self.mount
            ))
        }
    }
}

fn result_to_body<T>(result: std::result::Result<T, (ErrorCode, String)>) -> ResBody
where
    T: serde::Serialize,
{
    match result {
        Ok(value) => ResBody::ok(serde_json::to_value(value).unwrap_or(Value::Null)),
        Err((code, message)) => ResBody::err(code, message, None),
    }
}

pub struct AgentHandle {
    io: AdapterIo,
    inbound: Mutex<mpsc::Receiver<IncomingFrame>>,
    reports: ReportSender,
    welcome: Mutex<Option<HelloAck>>,
}

impl AgentHandle {
    fn new(io: AdapterIo, inbound: mpsc::Receiver<IncomingFrame>) -> Self {
        AgentHandle {
            reports: ReportSender::new(io.clone(), 1),
            io,
            inbound: Mutex::new(inbound),
            welcome: Mutex::new(None),
        }
    }

    pub async fn hello(&self, mut args: HelloArgs) -> Result<HelloAck> {
        args.kind = MountKind::Agent;
        let value = self
            .io
            .request_ok(AdapterMsg::Plugin(PluginOp::Hello(args)))
            .await?;
        let ack = decode_welcome(value)?;
        *self.welcome.lock().await = Some(ack.clone());
        Ok(ack)
    }

    pub async fn hello_role(
        &self,
        role: impl Into<String>,
        plugin: impl Into<String>,
        capabilities: Vec<Capability>,
    ) -> Result<HelloAck> {
        self.hello(HelloArgs {
            protocol: PROTOCOL_VERSION,
            plugin: plugin.into(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            kind: MountKind::Agent,
            capabilities,
            mount: Some(Mount::Agent(AgentMount {
                role: role.into(),
                session: None,
                task_id: None,
                pid: Some(std::process::id()),
            })),
        })
        .await
    }

    pub async fn wait_welcome(&self) -> Result<HelloAck> {
        if let Some(ack) = self.welcome.lock().await.clone() {
            return Ok(ack);
        }
        loop {
            match self.next_host_op().await? {
                HostOp::Welcome(ack) => {
                    *self.welcome.lock().await = Some(ack.clone());
                    return Ok(ack);
                }
                HostOp::Bye(bye) => return Err(AdapterError::Unexpected(bye.reason)),
                _ => {}
            }
        }
    }

    pub async fn next_host_op(&self) -> Result<HostOp> {
        let mut inbound = self.inbound.lock().await;
        match inbound.recv().await {
            Some(frame) => match frame.msg {
                AdapterMsg::Host(op) => Ok(op),
                AdapterMsg::Res(body) => Err(AdapterError::Protocol(body)),
                AdapterMsg::Plugin(_) => Err(AdapterError::Unexpected(
                    "plugin op on client inbound".to_string(),
                )),
            },
            None => Err(AdapterError::Closed),
        }
    }

    pub fn assign_stream(&self) -> Pin<Box<dyn Stream<Item = AssignArgs> + Send + '_>> {
        Box::pin(async_stream::stream! {
            while let Ok(op) = self.next_host_op().await {
                if let HostOp::Assign(assign) = op {
                    yield assign;
                }
            }
        })
    }

    pub async fn wait_assign(&self) -> Result<AssignArgs> {
        loop {
            match self.next_host_op().await? {
                HostOp::Assign(assign) => return Ok(assign),
                HostOp::Bye(bye) => return Err(AdapterError::Unexpected(bye.reason)),
                _ => {}
            }
        }
    }

    pub async fn probe_reply(&self, observed: Value) -> Result<()> {
        let task_id = observed
            .get("task_id")
            .and_then(Value::as_str)
            .unwrap_or("probe")
            .to_string();
        self.reports.heartbeat(task_id, observed).await.map(|_| ())
    }

    pub async fn recycle_ack(
        &self,
        task_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<()> {
        self.reports
            .fault(Some(task_id.into()), "recycle_ack", reason.into())
            .await
            .map(|_| ())
    }

    pub async fn config_get(&self, key: impl Into<String>) -> Result<Value> {
        self.io
            .request_ok(AdapterMsg::Host(HostOp::ConfigGet(ConfigGetArgs {
                key: key.into(),
            })))
            .await
    }

    pub async fn report_ready(
        &self,
        task_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<Report> {
        self.reports.ready(task_id, session_id).await
    }

    pub async fn report_heartbeat(
        &self,
        task_id: impl Into<String>,
        observed: Value,
    ) -> Result<Report> {
        self.reports.heartbeat(task_id, observed).await
    }

    pub async fn report_complete(
        &self,
        task_id: impl Into<String>,
        outcome: Outcome,
        head: Option<String>,
    ) -> Result<Report> {
        self.reports.complete(task_id, outcome, head).await
    }

    pub async fn report_fault(
        &self,
        task_id: Option<String>,
        kind: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<Report> {
        self.reports.fault(task_id, kind, reason).await
    }

    pub async fn send(&self, envelope: Envelope) -> Result<Receipt> {
        let value = self
            .io
            .request_ok(AdapterMsg::Plugin(PluginOp::Send(Box::new(envelope))))
            .await?;
        serde_json::from_value(value).map_err(AdapterError::Serde)
    }

    pub fn report_sender(&self) -> ReportSender {
        self.reports.clone()
    }

    pub async fn exit(&self, reason: impl Into<String>) -> Result<()> {
        self.io
            .request_ok(AdapterMsg::Plugin(PluginOp::Detach(DetachArgs {
                reason: reason.into(),
            })))
            .await
            .map(|_| ())
    }
}

fn decode_welcome(value: Value) -> Result<HelloAck> {
    if value.get("op").and_then(Value::as_str) == Some("welcome") {
        let op: HostOp = serde_json::from_value(value.clone())?;
        if let HostOp::Welcome(ack) = op {
            return Ok(ack);
        }
    }
    serde_json::from_value(value).map_err(AdapterError::Serde)
}

pub struct GatewayHandle {
    io: AdapterIo,
    inbound: Mutex<mpsc::Receiver<IncomingFrame>>,
}

impl GatewayHandle {
    fn new(io: AdapterIo, inbound: mpsc::Receiver<IncomingFrame>) -> Self {
        GatewayHandle {
            io,
            inbound: Mutex::new(inbound),
        }
    }

    pub async fn hello(&self, mut args: HelloArgs) -> Result<HelloAck> {
        args.kind = MountKind::Gateway;
        let value = self
            .io
            .request_ok(AdapterMsg::Plugin(PluginOp::Hello(args)))
            .await?;
        decode_welcome(value)
    }

    pub async fn hello_gateway(
        &self,
        gateway: impl Into<String>,
        platform: impl Into<String>,
        capabilities: Vec<Capability>,
    ) -> Result<HelloAck> {
        let gateway = gateway.into();
        let platform = platform.into();
        self.hello(HelloArgs {
            protocol: PROTOCOL_VERSION,
            plugin: format!("onlyne-gateway-{platform}"),
            version: env!("CARGO_PKG_VERSION").to_string(),
            kind: MountKind::Gateway,
            capabilities,
            mount: Some(Mount::Gateway(GatewayMount { gateway, platform })),
        })
        .await
    }

    pub async fn deliver_inbound(&self, delivery: Delivery) -> Result<()> {
        self.io
            .request_ok(AdapterMsg::Plugin(PluginOp::Deliver(delivery)))
            .await
            .map(|_| ())
    }

    pub async fn next_host_op(&self) -> Result<HostOp> {
        let mut inbound = self.inbound.lock().await;
        match inbound.recv().await {
            Some(frame) => match frame.msg {
                AdapterMsg::Host(op) => Ok(op),
                AdapterMsg::Res(body) => Err(AdapterError::Protocol(body)),
                AdapterMsg::Plugin(_) => Err(AdapterError::Unexpected(
                    "plugin op on gateway inbound".to_string(),
                )),
            },
            None => Err(AdapterError::Closed),
        }
    }

    pub fn render_send_stream(&self) -> Pin<Box<dyn Stream<Item = RenderSendArgs> + Send + '_>> {
        Box::pin(async_stream::stream! {
            while let Ok(op) = self.next_host_op().await {
                if let HostOp::RenderSend(args) = op {
                    yield args;
                }
            }
        })
    }

    pub async fn wait_render_send(&self) -> Result<RenderSendArgs> {
        loop {
            match self.next_host_op().await? {
                HostOp::RenderSend(args) => return Ok(args),
                HostOp::Bye(bye) => return Err(AdapterError::Unexpected(bye.reason)),
                _ => {}
            }
        }
    }

    pub async fn health(
        &self,
        state: GatewayHealth,
        detail: Option<String>,
        uptime_s: u64,
    ) -> Result<()> {
        let mut args = HealthArgs::from(state);
        args.detail = detail;
        args.uptime_s = uptime_s;
        self.io
            .request_ok(AdapterMsg::Plugin(PluginOp::Health(args)))
            .await
            .map(|_| ())
    }

    /// Report the platform typing state for one conversation.
    ///
    /// The indicator travels as a state, so a caller that only turns it on
    /// leaves a stuck indicator on the platform.
    pub async fn typing(&self, conversation: impl Into<String>, on: bool) -> Result<()> {
        self.io
            .request_ok(AdapterMsg::Plugin(PluginOp::Typing(TypingArgs {
                conversation: conversation.into(),
                on,
            })))
            .await
            .map(|_| ())
    }

    pub async fn register_channel(&self, args: RegisterChannelArgs) -> Result<()> {
        self.io
            .request_ok(AdapterMsg::Plugin(PluginOp::RegisterChannel(args)))
            .await
            .map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceGap {
    Unsupported(&'static str),
    Failed {
        member: &'static str,
        reason: String,
    },
}

impl fmt::Display for SurfaceGap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SurfaceGap::Unsupported(member) => write!(f, "unsupported surface member: {member}"),
            SurfaceGap::Failed { member, reason } => {
                write!(f, "surface member {member} failed: {reason}")
            }
        }
    }
}

impl std::error::Error for SurfaceGap {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeUser {
    pub text: String,
    pub deliver_as: Option<String>,
}

#[async_trait]
pub trait AgentSurface: Send + Sync {
    async fn config_path(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("config_path"))
    }

    async fn register_tool(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("register_tool"))
    }

    async fn register_command(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("register_command"))
    }

    async fn wake_user(&self, _wake: WakeUser) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("wake_user"))
    }

    async fn send_custom_entry(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("send_custom_entry"))
    }

    async fn on_turn_lifecycle(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("on_turn_lifecycle"))
    }

    async fn exit(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("exit"))
    }

    async fn wrap_result(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("wrap_result"))
    }

    async fn set_active_tools(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("set_active_tools"))
    }

    async fn set_status(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("set_status"))
    }

    async fn set_title(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("set_title"))
    }

    async fn set_model(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("set_model"))
    }

    async fn set_thinking_level(&self) -> std::result::Result<(), SurfaceGap> {
        Err(SurfaceGap::Unsupported("set_thinking_level"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SurfaceGaps {
    unsupported: Vec<&'static str>,
}

impl SurfaceGaps {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, gap: SurfaceGap) {
        if let SurfaceGap::Unsupported(member) = gap {
            if !self.unsupported.contains(&member) {
                self.unsupported.push(member);
            }
        }
    }

    pub fn unsupported(&self) -> &[&'static str] {
        &self.unsupported
    }

    pub fn report(&self) -> Vec<String> {
        self.unsupported
            .iter()
            .map(|member| format!("agent surface gap: {member}"))
            .collect()
    }
}

pub fn text_envelope(
    kind: onlyne_proto::MsgKind,
    from: Principal,
    to: Principal,
    text: impl Into<String>,
    causality: Option<onlyne_proto::Causality>,
) -> std::result::Result<Envelope, onlyne_proto::Error> {
    onlyne_proto::new_envelope(kind, from, to, Body::text(text), causality)
}

pub fn image_envelope(
    kind: onlyne_proto::MsgKind,
    from: Principal,
    to: Principal,
    data_base64: String,
    mime: impl Into<String>,
    causality: Option<onlyne_proto::Causality>,
) -> std::result::Result<Envelope, onlyne_proto::Error> {
    onlyne_proto::new_envelope(
        kind,
        from,
        to,
        Body {
            text: None,
            image: Some(ImagePart {
                data_base64,
                mime: mime.into(),
                name: None,
            }),
        },
        causality,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use onlyne_proto::{LedgerState, MsgKind, ServerInfo, new_envelope};

    fn ack() -> HelloAck {
        HelloAck {
            protocol: PROTOCOL_VERSION,
            role: "planner".to_string(),
            session_id: Some("s1".to_string()),
            generation: 1,
            prose: "Read the task".to_string(),
            server: ServerInfo {
                connected: true,
                cluster: "local".to_string(),
                name: "srv".to_string(),
            },
            host_capabilities: vec![Capability::Inject],
        }
    }

    fn envelope(text: &str) -> Envelope {
        new_envelope(
            MsgKind::Note,
            Principal::role("planner"),
            Principal::role("builder"),
            Body::text(text),
            None,
        )
        .expect("valid envelope")
    }

    #[test]
    fn degrade_covers_required_gaps() {
        assert!(degrade_for(&[]).is_empty());
        assert_eq!(
            degrade_for(&[Capability::Recycle])[0].action,
            "recycle missing means the host judges resource loss through probe"
        );
        assert_eq!(
            degrade_for(&[Capability::Report])[0].action,
            "report missing means the affected session moves to idle_fault with a recorded fault"
        );
        assert_eq!(
            degrade_for(&[Capability::Inject])[0].action,
            "assign missing means the payload travels through process stdin or argv and the terminal state comes from the exit code plus the last output line"
        );
    }

    #[test]
    fn report_generation_is_monotonic() {
        assert!(!accept_report_generation((2, 8), (1, 99)));
        assert!(!accept_report_generation((2, 8), (2, 8)));
        assert!(accept_report_generation((2, 8), (2, 9)));
        assert!(accept_report_generation((2, 8), (3, 1)));
    }

    #[test]
    fn capability_require_names_missing() {
        let set = CapabilitySet::new(vec![Capability::Report]);
        let err = set
            .require(&[Capability::Report, Capability::Recycle])
            .unwrap_err();
        assert_eq!(err.0, ErrorCode::Forbidden);
        assert!(err.1.contains("recycle"));
    }

    #[tokio::test]
    async fn pre_hello_frame_is_rejected() {
        let (client, server) = tokio::io::duplex(4096);
        let server_task =
            tokio::spawn(async move { AdapterServer::accept(server, |_| Ok(ack())).await });
        let io = AdapterIo::new(client, Duration::from_secs(1), Duration::from_secs(1));
        let body = io
            .request(AdapterMsg::Plugin(PluginOp::Send(Box::new(envelope(
                "early",
            )))))
            .await
            .expect("pre hello response");
        assert!(!body.ok);
        assert_eq!(
            body.error.as_ref().map(|e| e.code),
            Some(ErrorCode::Invalid)
        );
        assert_eq!(
            body.error.as_ref().map(|e| e.message.as_str()),
            Some(HELLO_REQUIRED_MESSAGE)
        );
        let server_err = match server_task.await.expect("server task") {
            Ok(_) => panic!("server accepted pre-hello frame"),
            Err(err) => err,
        };
        assert_eq!(server_err.code(), Some(ErrorCode::Invalid));
    }

    struct SendHost;

    #[async_trait]
    impl Host for SendHost {
        async fn hello(
            &self,
            _args: &HelloArgs,
        ) -> std::result::Result<HelloAck, (ErrorCode, String)> {
            Ok(ack())
        }

        async fn send(
            &self,
            envelope: &Envelope,
        ) -> std::result::Result<Receipt, (ErrorCode, String)> {
            Ok(Receipt {
                msg_id: envelope.id.clone(),
                op_id: envelope.op_id.clone(),
                kind: envelope.kind,
                task: envelope.task_id().map(str::to_string),
                state: LedgerState::Acked,
                enqueued_at: chrono::Utc::now(),
            })
        }
    }

    #[tokio::test]
    async fn dispatcher_rejects_gateway_op_on_agent_mount() {
        let dispatcher = HostDispatcher::new(MountKind::Agent, Arc::new(SendHost));
        let body = dispatcher
            .dispatch(PluginOp::Health(HealthArgs::default()))
            .await;
        assert!(!body.ok);
        let error = body.error.expect("error");
        assert_eq!(error.code, ErrorCode::Forbidden);
        assert!(error.message.contains("health"));
    }
}
