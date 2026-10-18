//! Adapter SDK for the Onlyne local socket.
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

pub mod dispatch;
pub use dispatch::{
    AgentSurface, CapabilitySet, Host, HostDispatcher, HostGap, SurfaceGap, SurfaceGaps, WakeUser,
    degrade_for,
};

pub mod plugin;
pub use plugin::{
    AdapterHealth, GatewayHost, GatewayPlugin, OnboardingKind, OnboardingPrompt, Outbound,
    SendReceipt,
};

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::Stream;
use onlyne_proto::adapter::HandoffArgs;
use onlyne_proto::{
    AdapterMsg, AgentMount, AssignArgs, Body, ByeNotice, ConfigGetArgs, Delivery, DetachArgs,
    Envelope, ErrorCode, GatewayHealth, GatewayMount, HELLO_REQUIRED_MESSAGE, HealthArgs, HostOp,
    ImagePart, Outcome, PluginOp, Principal, Receipt, RegisterChannelArgs, RenderSendArgs, Report,
    ResBody, TypingArgs,
};
pub use onlyne_proto::{
    Capability, HelloAck, HelloArgs, Mount, MountKind, PROTOCOL_VERSION, ToolsMount,
};
use onlyne_wire::{FrameReader, read_frame, write_frame};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf, split};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::{sleep, timeout};
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

/// Frames one connection buffers for its host before the reader sheds.
///
/// The ceiling is what keeps a host that stopped consuming — a `Host` future hung
/// on a model call, a dispatcher awaiting a lock — from turning the socket into an
/// unbounded queue: every buffered frame can carry an inline image, so an
/// unbuffered read path costs memory the plugin's stall should not bill here. A
/// host that drains at the rate the wire delivers never sees the number.
pub const INBOUND_CAPACITY: usize = 256;

/// How long the reader waits between two tries against a full inbound queue.
pub const INBOUND_PRESSURE_PAUSE: Duration = Duration::from_millis(20);

/// Tries the reader spends on a full queue before it sheds the frame.
///
/// Paired with [`INBOUND_PRESSURE_PAUSE`] this is the backpressure window: a
/// consumer that finds room within ~200 ms loses nothing, and one that does not
/// answers its plugin instead of stalling the reader forever.
pub const INBOUND_PRESSURE_RETRIES: u32 = 10;

#[derive(Debug)]
pub enum AdapterError {
    Io(std::io::Error),
    Serde(serde_json::Error),
    Closed,
    Timeout(&'static str),
    Protocol(ResBody),
    Code { code: ErrorCode, message: String },
    Unexpected(String),
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

/// The requests of one connection that still wait for their answer.
///
/// `closed` is set once, when the reader ends: no answer can arrive after that,
/// so every waiter is dropped at that moment and a later request is refused
/// before it registers, rather than sitting out its timeout.
#[derive(Default)]
struct Pending {
    closed: bool,
    waiters: HashMap<u64, oneshot::Sender<ResBody>>,
}

#[derive(Clone)]
pub struct AdapterIo {
    tx: mpsc::Sender<QueuedFrame>,
    pending: Arc<Mutex<Pending>>,
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

    /// Drive one connection.
    ///
    /// `read_timeout` bounds how long one request waits for its answer. It is
    /// not an idle deadline: the protocol has no keepalive frame, so a quiet
    /// connection is a healthy one. The connection ends when the peer closes,
    /// when a write fails, or once every handle is dropped.
    pub fn new_with_inbound<S>(
        stream: S,
        read_timeout: Duration,
        write_timeout: Duration,
    ) -> (Self, mpsc::Receiver<IncomingFrame>)
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (reader, writer) = split(stream);
        let (tx, rx) = mpsc::channel(INBOUND_CAPACITY);
        let (incoming_tx, incoming_rx) = mpsc::channel(INBOUND_CAPACITY);
        let (writer_done_tx, writer_done_rx) = oneshot::channel();
        let pending = Arc::new(Mutex::new(Pending::default()));
        tokio::spawn(writer_loop(writer, rx, write_timeout, writer_done_tx));
        tokio::spawn(reader_loop(
            reader,
            pending.clone(),
            incoming_tx,
            tx.downgrade(),
            writer_done_rx,
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

    /// Whether two handles drive one connection.
    ///
    /// A handle is cheap to clone, and every clone writes to the same channel. A
    /// host that bound a session to a connection needs this test to drop exactly
    /// those bindings when the connection ends, leaving a later connection's
    /// bindings in place.
    pub fn same_connection(&self, other: &Self) -> bool {
        self.tx.same_channel(&other.tx)
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

    /// Send one request and wait up to `read_timeout` for its answer.
    ///
    /// A connection whose reader has ended answers [`AdapterError::Closed`] at
    /// once, and so does a request in flight when it ends.
    pub async fn request(&self, msg: AdapterMsg) -> Result<ResBody> {
        let id = self.id.fetch_add(1, Ordering::SeqCst);
        let (response_tx, response_rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if pending.closed {
                return Err(AdapterError::Closed);
            }
            pending.waiters.insert(id, response_tx);
        }
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
            self.pending.lock().await.waiters.remove(&id);
            return Err(AdapterError::SendDropped);
        }
        match timeout(self.read_timeout, response_rx).await {
            Ok(Ok(body)) => Ok(body),
            // The reader dropped the waiter: the connection ended unanswered.
            Ok(Err(_)) => Err(AdapterError::Closed),
            Err(_) => {
                self.pending.lock().await.waiters.remove(&id);
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

/// Write queued frames until every handle is gone or the socket refuses one.
///
/// The write half is shut down on the way out for streams that carry a half
/// close. `done` is dropped last and tells the reader to stop: a local socket
/// closes only when both halves are gone, so the reader must let go of its own.
async fn writer_loop<W>(
    mut writer: WriteHalf<W>,
    mut rx: mpsc::Receiver<QueuedFrame>,
    write_timeout: Duration,
    done: oneshot::Sender<()>,
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
        match timeout(write_timeout, write_frame(&mut writer, &message)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                warn!(error = %error, "adapter write failed; closing the connection");
                break;
            }
            Err(_) => {
                warn!("adapter write timed out; closing the connection");
                break;
            }
        }
    }
    let _ = timeout(write_timeout, writer.shutdown()).await;
    drop(done);
}

/// Read frames until the peer closes, the stream fails, or the writer ends.
///
/// Waiting for the next frame has no deadline: an idle connection is healthy.
/// Once the writer has ended — every handle is gone, or the socket refused a
/// write — nothing on this side can answer the peer again, so the reader stops
/// too even if the peer never closes, and the socket is released. On every exit
/// the pending requests are failed at once.
async fn reader_loop<R>(
    mut reader: ReadHalf<R>,
    pending: Arc<Mutex<Pending>>,
    incoming: mpsc::Sender<IncomingFrame>,
    outbound: mpsc::WeakSender<QueuedFrame>,
    mut writer_done: oneshot::Receiver<()>,
) where
    R: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut frames = FrameReader::new();
    loop {
        let wire = tokio::select! {
            read = frames.next::<_, WireMessage>(&mut reader) => match read {
                Ok(Some(wire)) => wire,
                Ok(None) | Err(_) => break,
            },
            _ = &mut writer_done => break,
        };
        let id = wire.id;
        let reply_to = wire.reply_to;
        let frame = match wire.msg {
            AdapterMsg::Res(body) => {
                if let Some(reply_id) = reply_to {
                    if let Some(waiter) = pending.lock().await.waiters.remove(&reply_id) {
                        let _ = waiter.send(body);
                        continue;
                    }
                }
                IncomingFrame {
                    id,
                    reply_to,
                    msg: AdapterMsg::Res(body),
                }
            }
            msg => IncomingFrame { id, reply_to, msg },
        };
        if !forward_incoming(&incoming, &outbound, frame).await {
            break;
        }
    }
    let mut pending = pending.lock().await;
    pending.closed = true;
    pending.waiters.clear();
}

/// Hand one decoded frame to the host's consumer without growing the buffer.
///
/// The answer is `false` only when the consumer is gone and the reader must
/// stop. A full queue is waited on, not widened: the window is what lets a host
/// that merely paused keep every frame, and [`shed`] is what stops a host that
/// never returns from stopping the reader with it.
async fn forward_incoming(
    incoming: &mpsc::Sender<IncomingFrame>,
    outbound: &mpsc::WeakSender<QueuedFrame>,
    frame: IncomingFrame,
) -> bool {
    let mut waiting = frame;
    for _ in 0..INBOUND_PRESSURE_RETRIES {
        match incoming.try_send(waiting) {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Full(rejected)) => {
                waiting = rejected;
                sleep(INBOUND_PRESSURE_PAUSE).await;
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
        }
    }
    shed(outbound, waiting);
    true
}

/// Drop one frame the host is not reading, and signal the pressure back.
///
/// Only an unanswered request has someone to signal: the error rides its own id,
/// which is the one reply its plugin can act on, and says `internal` — the
/// retryable code — because a host that drains again takes the frame next time.
/// A frame that already answers something else has no such reader waiting, so it
/// is logged and dropped, and a notification carries no id at all. The reader
/// holds the write queue weakly, so that it never keeps a connection alive on
/// its own; once every handle is gone, the shed request has nobody to answer it.
fn shed(outbound: &mpsc::WeakSender<QueuedFrame>, frame: IncomingFrame) {
    let IncomingFrame { id, reply_to, msg } = frame;
    warn!(
        op = msg.op_name().unwrap_or("res"),
        reply_to = ?reply_to,
        capacity = INBOUND_CAPACITY,
        "adapter inbound queue is full; shedding the frame",
    );
    let (Some(id), None) = (id, reply_to) else {
        return;
    };
    let Some(outbound) = outbound.upgrade() else {
        warn!(
            id,
            "adapter writer is gone; the shed request goes unanswered"
        );
        return;
    };
    let body = ResBody::err(
        ErrorCode::Internal,
        format!("host inbound queue is full at {INBOUND_CAPACITY} frames; retry"),
        Some("queue".to_string()),
    );
    if outbound
        .try_send(QueuedFrame {
            id: None,
            reply_to: Some(id),
            msg: AdapterMsg::Res(body),
        })
        .is_err()
    {
        warn!(
            id,
            "adapter write queue is full too; the shed request goes unanswered"
        );
    }
}

pub struct AdapterClient;

impl AdapterClient {
    pub async fn connect_local(path: impl AsRef<Path>) -> Result<AgentHandle> {
        let stream = onlyne_wire::socket::connect_local(path.as_ref()).await?;
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
        if hello.protocol != PROTOCOL_VERSION {
            let reply_to = first.id.unwrap_or_default();
            return Err(Self::refuse_revision(
                &mut stream,
                write_timeout,
                &hello,
                reply_to,
                peer_pid,
            )
            .await);
        }
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
        let reply_to = first.id.unwrap_or_default();
        let hello = match first.msg {
            AdapterMsg::Plugin(PluginOp::Hello(args)) => args,
            _ => {
                let body = ResBody::err(
                    ErrorCode::Invalid,
                    HELLO_REQUIRED_MESSAGE,
                    Some("op".to_string()),
                );
                let wire = WireMessage::response(reply_to, body);
                let _ = timeout(DEFAULT_WRITE_TIMEOUT, write_frame(&mut stream, &wire)).await;
                return Err(AdapterError::wire(
                    ErrorCode::Invalid,
                    HELLO_REQUIRED_MESSAGE,
                    Some("op"),
                ));
            }
        };
        if hello.protocol != PROTOCOL_VERSION {
            return Err(Self::refuse_revision(
                &mut stream,
                DEFAULT_WRITE_TIMEOUT,
                &hello,
                reply_to,
                None,
            )
            .await);
        }
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

    /// Refuse a `hello` whose revision this SDK does not speak.
    ///
    /// A plugin on another revision has to be told twice, in the two ways it can
    /// hear: the error answers its own frame — that is the reply its request is
    /// already waiting on — and the `bye` says the connection is over, so it does
    /// not sit holding a half-open socket for a `welcome` that never comes. The
    /// word goes to the log as well, since a revision mismatch is a deployment
    /// fault the host operator, not the plugin, reads.
    async fn refuse_revision<S>(
        stream: &mut S,
        write_timeout: Duration,
        hello: &HelloArgs,
        reply_to: u64,
        peer_pid: Option<u32>,
    ) -> AdapterError
    where
        S: AsyncWrite + Unpin,
    {
        let message = format!(
            "protocol {} unsupported, expected {PROTOCOL_VERSION}",
            hello.protocol
        );
        warn!(
            plugin = %hello.plugin,
            protocol = hello.protocol,
            expected = PROTOCOL_VERSION,
            peer_pid = ?peer_pid,
            "adapter hello refused: unsupported protocol revision",
        );
        let body = ResBody::err(
            ErrorCode::ProtocolVersion,
            message.clone(),
            Some("protocol".to_string()),
        );
        let answer = WireMessage::response(reply_to, body.clone());
        let _ = timeout(write_timeout, write_frame(stream, &answer)).await;
        let bye = WireMessage::request(
            None,
            AdapterMsg::Host(HostOp::Bye(ByeNotice { reason: message })),
        );
        let _ = timeout(write_timeout, write_frame(stream, &bye)).await;
        AdapterError::Protocol(body)
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

impl AdapterClient {
    pub async fn connect_gateway_local(path: impl AsRef<Path>) -> Result<GatewayHandle> {
        let stream = onlyne_wire::socket::connect_local(path.as_ref()).await?;
        Ok(Self::gateway(stream))
    }
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
            // A sender bound to one session names it; an unbound sender's beat
            // reaches its host with no session and no projection, and the
            // client reduces the observation into the row it keeps itself.
            session_id: self.session_id.lock().await.clone().unwrap_or_default(),
            generation: self.generation(),
            seq: self.next_seq(),
            observed,
            projection: None,
            // A local report has no origin cluster; a connection speaking for a
            // sub-cluster carries that name in `hello`'s `Mount::Cluster`.
            cluster_ref: None,
        };
        self.send(report.clone()).await?;
        Ok(report)
    }

    /// File this session's verdict for a task.
    ///
    /// `head` is the one-line preview the ledger keeps for the operator;
    /// `details` is the full result, which the client delivers verbatim to the
    /// next hop and the originator, and `files` names the files it refers to.
    /// A caller with only a head passes `None` and an empty list.
    pub async fn complete(
        &self,
        task_id: impl Into<String>,
        outcome: Outcome,
        head: Option<String>,
        details: Option<String>,
        files: Vec<String>,
    ) -> Result<Report> {
        self.complete_answered(task_id, outcome, head, details, files)
            .await
            .map(|(report, _)| report)
    }

    /// File this session's verdict and answer with the client's body.
    ///
    /// The body is `null` for a report the task was waiting for, and carries the
    /// verdict the task already had when the client had settled it before
    /// (`{"already_settled": {state, settled_at, head}}`,
    /// `docs/v2-CONTRACT.md` §3c). A drive that answers a model reads the second
    /// form so a completion behind a standing verdict does not read as one this
    /// call caused.
    pub async fn complete_answered(
        &self,
        task_id: impl Into<String>,
        outcome: Outcome,
        head: Option<String>,
        details: Option<String>,
        files: Vec<String>,
    ) -> Result<(Report, serde_json::Value)> {
        let report = Report::Complete {
            task_id: task_id.into(),
            outcome,
            head,
            details,
            files,
            reply_to: None,
            cluster_ref: None,
        };
        let answer = self.send_answered(report.clone()).await?;
        Ok((report, answer))
    }

    /// File one report and answer with the client's body verbatim.
    pub async fn send_answered(&self, report: Report) -> Result<serde_json::Value> {
        self.io
            .request_ok(AdapterMsg::Plugin(PluginOp::Report(report)))
            .await
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
        self.send_answered(report).await.map(|_| ())
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::SeqCst) + 1
    }
}

pub fn accept_report_generation(watermark: (u64, u64), incoming: (u64, u64)) -> bool {
    incoming.0 > watermark.0 || (incoming.0 == watermark.0 && incoming.1 > watermark.1)
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
                runtime: None,
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
        self.next_host_frame().await.map(|(_, op)| op)
    }

    /// The next host op, with the id of the frame it arrived in.
    ///
    /// A notification carries no id and a request does, and a runtime that owns
    /// its sessions has to answer the requests: a host that asked for a session
    /// waits, and a dropped frame is a delivery that never runs. Reading the op
    /// alone loses the id, so a caller that may have to answer asks for this.
    pub async fn next_host_frame(&self) -> Result<(Option<u64>, HostOp)> {
        let mut inbound = self.inbound.lock().await;
        match inbound.recv().await {
            Some(frame) => match frame.msg {
                AdapterMsg::Host(op) => Ok((frame.id, op)),
                AdapterMsg::Res(body) => Err(AdapterError::Protocol(body)),
                AdapterMsg::Plugin(_) => Err(AdapterError::Unexpected(
                    "plugin op on client inbound".to_string(),
                )),
            },
            None => Err(AdapterError::Closed),
        }
    }

    /// Answer a host request, naming the conversation the runtime opened and a
    /// handle it will accept on a later `open`.
    ///
    /// The handle is the runtime's own word for where its conversation is. The
    /// client stores it without reading it and hands it back on the next ask,
    /// which is what lets one family keep one conversation across a client
    /// restart — a runtime that cannot resume answers without one, and the client
    /// does not paper over the difference with a summary of its own.
    pub async fn answer_open(&self, reply_to: u64, session_id: &str, handle: &str) -> Result<()> {
        self.io
            .respond(
                reply_to,
                ResBody::ok(json!({
                    "session_id": session_id,
                    "conversation": handle,
                    "resume_handle": handle,
                })),
            )
            .await
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
        details: Option<String>,
        files: Vec<String>,
    ) -> Result<Report> {
        self.reports
            .complete(task_id, outcome, head, details, files)
            .await
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

    /// Hand this session's task on to another role.
    ///
    /// The host mints the child — it knows the parent's causality, the family
    /// root, and the hop budget — so the answer is the child's own identity:
    /// `{ "task_id": …, "hop": …, "queued": …, "op_id": … }`. A refused handoff
    /// arrives as an error carrying the host's own word for the refusal.
    pub async fn handoff(&self, args: HandoffArgs) -> Result<Value> {
        self.io
            .request_ok(AdapterMsg::Plugin(PluginOp::Handoff(args)))
            .await
    }

    pub fn report_sender(&self) -> ReportSender {
        self.reports.clone()
    }

    /// Build an independent [`ReportSender`] for one hosted session, with its
    /// own `(generation, seq)` counter and bound `session_id`.
    pub fn report_sender_for(
        &self,
        session_id: impl Into<String>,
        generation: u64,
    ) -> ReportSender {
        let sender = ReportSender::new(self.io.clone(), generation);
        // The sender is freshly built, so nothing else holds the lock yet and
        // the binding cannot wait for an await inside this sync constructor.
        if let Ok(mut bound) = sender.session_id.try_lock() {
            *bound = Some(session_id.into());
        }
        sender
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
            head: None,
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
    use onlyne_proto::{Capability, Mount, MountKind};

    fn beat(report: Report) -> (String, u64) {
        let Report::Heartbeat {
            session_id, seq, ..
        } = report
        else {
            panic!("expected a heartbeat");
        };
        (session_id, seq)
    }

    #[tokio::test]
    async fn agent_mount_runtime_roundtrips_and_defaults() {
        let json = serde_json::json!({"role": "reviewer", "runtime": "pi"});
        let mount: AgentMount = serde_json::from_value(json).unwrap();
        assert_eq!(mount.runtime.as_deref(), Some("pi"));
        assert_eq!(mount.session, None);
        assert_eq!(mount.task_id, None);
        assert_eq!(mount.pid, None);

        let bare: AgentMount = serde_json::from_value(serde_json::json!({"role": "r"})).unwrap();
        assert_eq!(bare.runtime, None);

        let wire = serde_json::to_value(Mount::Agent(mount)).unwrap();
        assert_eq!(wire["runtime"], "pi");
        assert!(Mount::decode(MountKind::Agent, wire).is_ok());
    }

    #[tokio::test]
    async fn is_hosting_requires_open() {
        assert!(Capability::is_hosting(&[Capability::Open]));
        assert!(Capability::is_hosting(&[
            Capability::Report,
            Capability::Open
        ]));
        assert!(!Capability::is_hosting(&[
            Capability::Suspend,
            Capability::Close
        ]));
        assert!(!Capability::is_hosting(&[]));
    }

    #[tokio::test]
    async fn per_session_senders_count_and_bind_independently() {
        let (client_side, server_side) = tokio::net::UnixStream::pair().unwrap();
        let t = Duration::from_secs(1);
        let (server_io, mut inbound) = AdapterIo::new_with_inbound(server_side, t, t);
        tokio::spawn(async move {
            while let Some(frame) = inbound.recv().await {
                if let Some(id) = frame.id {
                    let _ = server_io.respond(id, ResBody::ok(Value::Null)).await;
                }
            }
        });
        let handle = AdapterClient::connect(client_side);

        let first = handle.report_sender_for("session-a", 2);
        let second = handle.report_sender_for("session-b", 3);

        assert_eq!(first.generation(), 2);
        assert_eq!(second.generation(), 3);

        let a1 = first.heartbeat("task-1", json!({"beat": 1})).await.unwrap();
        let a2 = first.heartbeat("task-1", json!({"beat": 2})).await.unwrap();
        let b1 = second
            .heartbeat("task-1", json!({"beat": 1}))
            .await
            .unwrap();

        let (a1_id, a1_seq) = beat(a1);
        assert_eq!(a1_id, "session-a");
        assert_eq!(a1_seq, 1);
        let (a2_id, a2_seq) = beat(a2);
        assert_eq!(a2_id, "session-a");
        assert_eq!(a2_seq, 2);
        let (b1_id, b1_seq) = beat(b1);
        assert_eq!(b1_id, "session-b");
        assert_eq!(b1_seq, 1);

        // The shared handle sender is untouched by the per-session ones.
        let shared = handle
            .report_heartbeat("task-1", json!({"beat": 1}))
            .await
            .unwrap();
        let (shared_id, shared_seq) = beat(shared);
        assert_eq!(shared_id, "");
        assert_eq!(shared_seq, 1);
    }
}
