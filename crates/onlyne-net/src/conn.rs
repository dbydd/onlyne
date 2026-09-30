//! The shared authenticated connection handle used by the client and gateway.
//!
//! A [`ConnHandle`] owns one pinned TLS connection to the server, the ed25519
//! admission handshake, the request/response multiplex, the observation stream,
//! the heartbeat, and the reconnect loop. Callers hold clones of the handle and
//! the task behind it stays shared.
//!
//! Outcomes a caller can tell apart:
//!
//! - `Ok(`[`ResBody`]`)` carries every `res` frame, refusals included. The typed
//!   `ErrorPayload::code` names the refusal, and `ResBody::data` keeps the
//!   earlier receipt that a duplicate `op_id` answer attaches.
//! - [`NetError::Unauthorized`] and [`NetError::ProtocolVersion`] arrive from
//!   admission and mean re-key or upgrade. [`retry_of`] answers `AfterHuman` for
//!   both, which stops the retry loop and says a person is the reason.
//! - [`NetError::Disconnected`] means the pipe died. The supervisor dials again
//!   under [`Backoff`], and the fresh connection serves the same handle.
//! - [`NetError::NotReady`] means the handle sits between connections.
//! - [`NetError::RequestTimeout`] means the caller's own deadline elapsed.

use onlyne_proto::{
    ClientOp, ErrorCode, Event, FaultEvent, Frame, GatewayOp, PROTOCOL_VERSION, ResBody, Retry,
    new_id,
};
use onlyne_wire::{FrameReader, is_bad_frame, is_too_large, read_frame, write_frame};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, Error as TlsError, ServerConfig};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::error::Error as StdError;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::NetError;
use crate::backoff::Backoff;
use crate::handshake::{DEFAULT_HANDSHAKE_TIMEOUT, connect_with_timeout};
use crate::identity::KeyPair;
use crate::tls::{PinMismatchMarker, client_config};

/// Handle over a client-vocabulary connection.
pub type ClientConn = ConnHandle<ClientOp>;

/// Handle over a gateway-vocabulary connection.
pub type GatewayConn = ConnHandle<GatewayOp>;

/// Local event queue depth, the `resync_lag` of §4 in the plan.
pub const DEFAULT_RESYNC_LAG: usize = 256;

/// Frames a caller may queue before `request` and `send` apply backpressure.
pub const OUTBOUND_QUEUE_DEPTH: usize = 256;

/// `FaultEvent::kind` of the synthetic frame that reports local event loss.
pub const RESYNC_LAG_KIND: &str = "resync_lag";

/// Reason carried by the `bye` frame that [`ConnHandle::close`] writes.
pub const CLOSE_REASON: &str = "close";

const STATE_READY: u8 = 0;
const STATE_RECONNECTING: u8 = 1;
const STATE_CLOSED: u8 = 2;

/// The client half of a pinned TLS connection.
pub type ClientTls = tokio_rustls::client::TlsStream<TcpStream>;

#[derive(Debug)]
pub enum TlsConn {
    Client(ClientTls),
    Server(tokio_rustls::server::TlsStream<TcpStream>),
}

impl TlsConn {
    pub async fn connect(endpoint: &str, pin: &str) -> Result<Self, NetError> {
        Ok(Self::Client(connect_client(endpoint, pin).await?))
    }

    pub async fn connect_with_config(
        endpoint: &str,
        config: ClientConfig,
    ) -> Result<Self, NetError> {
        Ok(Self::Client(
            connect_client_with_config(endpoint, config).await?,
        ))
    }

    pub async fn send_frame<T: Serialize>(&mut self, value: &T) -> Result<(), NetError> {
        match self {
            Self::Client(stream) => write_frame(stream, value).await,
            Self::Server(stream) => write_frame(stream, value).await,
        }
        .map_err(map_frame_error)
    }

    pub async fn recv_frame<T: DeserializeOwned>(&mut self) -> Result<Option<T>, NetError> {
        match self {
            Self::Client(stream) => read_frame(stream).await,
            Self::Server(stream) => read_frame(stream).await,
        }
        .map_err(map_frame_error)
    }

    /// Read one frame through `reader`, which keeps a partial frame across a
    /// dropped call; a `select!` loop reads through this.
    pub async fn recv_frame_with<T: DeserializeOwned>(
        &mut self,
        reader: &mut FrameReader,
    ) -> Result<Option<T>, NetError> {
        reader.next(self).await.map_err(map_frame_error)
    }
}

/// The wrapper delegates the byte traits, so `accept`, `read_frame`, and
/// `write_frame` take a `TlsConn` from either side without an inner accessor.
impl AsyncRead for TlsConn {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            Self::Client(stream) => std::pin::Pin::new(stream).poll_read(cx, buf),
            Self::Server(stream) => std::pin::Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for TlsConn {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match &mut *self {
            Self::Client(stream) => std::pin::Pin::new(stream).poll_write(cx, buf),
            Self::Server(stream) => std::pin::Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            Self::Client(stream) => std::pin::Pin::new(stream).poll_flush(cx),
            Self::Server(stream) => std::pin::Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            Self::Client(stream) => std::pin::Pin::new(stream).poll_shutdown(cx),
            Self::Server(stream) => std::pin::Pin::new(stream).poll_shutdown(cx),
        }
    }
}

#[derive(Debug)]
pub struct TcpListen {
    listener: TcpListener,
}

impl TcpListen {
    pub async fn bind(listen: &str) -> Result<Self, NetError> {
        let listener = TcpListener::bind(listen).await?;
        Ok(Self { listener })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        Ok(self.listener.local_addr()?)
    }

    /// Accept one TCP connection without running the TLS handshake.
    ///
    /// Keeping the two steps apart lets a caller serve each handshake on its
    /// own task, so a peer that fails or stalls one connection leaves the
    /// listener accepting (plan §4 line 198).
    pub async fn accept(&mut self) -> Result<TcpStream, NetError> {
        Ok(self.listener.accept().await?.0)
    }

    pub async fn accept_next(&mut self, config: &ServerConfig) -> Result<TlsConn, NetError> {
        let stream = self.accept().await?;
        accept_tls(stream, config).await
    }
}

/// Complete the TLS handshake for one accepted connection.
pub async fn accept_tls(stream: TcpStream, config: &ServerConfig) -> Result<TlsConn, NetError> {
    let acceptor = TlsAcceptor::from(Arc::new(config.clone()));
    acceptor
        .accept(stream)
        .await
        .map(TlsConn::Server)
        .map_err(map_tls_io_error)
}

/// Open a pinned TLS connection and hand back the client stream.
async fn connect_client(endpoint: &str, pin: &str) -> Result<ClientTls, NetError> {
    connect_client_with_config(endpoint, client_config(pin)?).await
}

async fn connect_client_with_config(
    endpoint: &str,
    config: ClientConfig,
) -> Result<ClientTls, NetError> {
    let (host, stream) = connect_tcp(endpoint).await?;
    let server_name = ServerName::try_from(host.clone())
        .map_err(|_| NetError::Io(format!("invalid TLS server name {host}")))?;
    let connector = TlsConnector::from(Arc::new(config));
    connector
        .connect(server_name, stream)
        .await
        .map_err(map_tls_io_error)
}

async fn connect_tcp(endpoint: &str) -> Result<(String, TcpStream), NetError> {
    let (host, _) = if let Ok(address) = endpoint.parse::<SocketAddr>() {
        (address.ip().to_string(), address.port())
    } else {
        let (host, port) = endpoint
            .rsplit_once(':')
            .ok_or_else(|| NetError::Io(format!("invalid endpoint {endpoint}")))?;
        (
            host.trim_matches(['[', ']']).to_string(),
            port.parse::<u16>()
                .map_err(|_| NetError::Io(format!("invalid endpoint {endpoint}")))?,
        )
    };
    let mut addresses = tokio::net::lookup_host(endpoint).await?;
    let address = addresses
        .next()
        .ok_or_else(|| NetError::Io(format!("endpoint {endpoint} has no addresses")))?;
    Ok((host, TcpStream::connect(address).await?))
}

fn map_frame_error(error: std::io::Error) -> NetError {
    if is_too_large(&error) {
        NetError::FrameTooLarge
    } else if is_bad_frame(&error) {
        NetError::BadFrame
    } else {
        NetError::Io(error.to_string())
    }
}

fn map_tls_io_error(error: std::io::Error) -> NetError {
    if let Some(source) = error
        .get_ref()
        .and_then(|source| source.downcast_ref::<TlsError>())
    {
        return map_rustls_error(source);
    }
    NetError::Io(error.to_string())
}

fn map_rustls_error(error: &TlsError) -> NetError {
    if let TlsError::Other(other) = error {
        if let Some(marker) = other
            .source()
            .and_then(|source| source.downcast_ref::<PinMismatchMarker>())
        {
            return NetError::PinMismatch {
                expected: marker.expected.clone(),
                got: marker.got.clone(),
            };
        }
    }
    NetError::Io(error.to_string())
}

/// Connection liveness gate shared by client and gateway callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnReadiness {
    /// An authenticated connection is live and accepts frames.
    Ready,
    /// The supervisor is dialing again after a death.
    Reconnecting,
    /// The connection ended for good; [`ConnHandle::failure`] carries the reason.
    Closed,
}

/// Everything one connection needs beyond its endpoint and identity.
#[derive(Debug, Clone)]
pub struct ConnSettings {
    /// Protocol revision sent in the `hello` challenge response.
    pub protocol: u16,
    /// Software name recorded for fault triage.
    pub agent: String,
    pub version: String,
    /// True when this connection serves an aggregate role.
    pub aggregate: bool,
    /// Interval between `ping` frames.
    pub heartbeat: Duration,
    /// Local event queue depth. Values below two are raised to two.
    pub resync_lag: usize,
    /// Delay schedule for redialing after a death.
    pub backoff: Backoff,
    /// Bound on the admission handshake.
    pub handshake_timeout: Duration,
}

impl Default for ConnSettings {
    fn default() -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            agent: "onlyne-net".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            aggregate: false,
            heartbeat: Duration::from_secs(10),
            resync_lag: DEFAULT_RESYNC_LAG,
            backoff: Backoff::new(),
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
        }
    }
}

impl ConnSettings {
    /// Settings for a given protocol revision and every other default.
    pub fn new(protocol: u16) -> Self {
        Self {
            protocol,
            ..Self::default()
        }
    }

    /// A ping waits this long for its `pong` before the connection counts dead.
    pub fn pong_deadline(&self) -> Duration {
        self.heartbeat.saturating_mul(2)
    }
}

/// Shared handle over one authenticated, reconnecting connection.
///
/// The observation stream is a `broadcast` channel, so the op payload carries
/// `Clone` beside the serde bounds. `ClientOp`, `GatewayOp`, and `AdminOp` all
/// implement it, and each subscriber clones one frame per delivery.
pub struct ConnHandle<Op: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> {
    inner: Arc<ConnInner<Op>>,
}

impl<Op: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> Clone for ConnHandle<Op> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<Op: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> std::fmt::Debug
    for ConnHandle<Op>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnHandle")
            .field("role", &self.inner.role)
            .field("readiness", &self.inner.readiness())
            .finish()
    }
}

impl<Op: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> ConnHandle<Op> {
    /// Adopt an already-connected stream and serve it with the shared loop.
    ///
    /// The caller owns admission: run `handshake::accept` or `handshake::connect`
    /// on the stream first when the surface is keyed. This is the unix-socket and
    /// in-process shape, where `dial`'s TLS and `cert_pin` do not apply. An
    /// attached stream carries no redial plan, so its first death stops the
    /// handle as `Closed` with the reason in `failure`.
    pub fn attach<S>(stream: S, role: &str, settings: ConnSettings) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let settings = ConnSettings {
            resync_lag: settings.resync_lag.max(2),
            ..settings
        };
        spawn_handle(
            stream,
            role,
            &settings,
            AdoptedPlan {
                marker: std::marker::PhantomData,
            },
        )
    }

    /// Role name this connection authenticated as.
    pub fn role(&self) -> &str {
        &self.inner.role
    }

    /// Current liveness of the connection behind this handle.
    pub fn readiness(&self) -> ConnReadiness {
        self.inner.readiness()
    }

    /// Terminal failure, once the supervisor stopped for good.
    pub async fn failure(&self) -> Option<NetError> {
        self.inner.failure.lock().await.clone()
    }

    /// Subscribe to the observation stream. Every consumer holds its own receiver
    /// over the one bounded queue inside this handle.
    pub fn events(&self) -> broadcast::Receiver<Frame<Op>> {
        self.inner.events.subscribe()
    }

    /// Write one request, register a waiter by its `id`, then await the answer.
    ///
    /// The frame keeps a caller-supplied id and receives a generated one when the
    /// id is empty. `BadFrame` answers any frame that opens no request.
    pub async fn request(&self, frame: Frame<Op>, timeout: Duration) -> Result<ResBody, NetError> {
        let frame = assign_id(frame)?;
        let id = frame.id().ok_or(NetError::BadFrame)?.to_string();
        let (waiter, answer) = oneshot::channel();
        {
            let mut pending = self.inner.pending.lock().await;
            pending.insert(id.clone(), waiter);
        }
        if let Err(error) = self.write(frame).await {
            self.inner.pending.lock().await.remove(&id);
            return Err(error);
        }
        match tokio::time::timeout(timeout, answer).await {
            Ok(Ok(Ok(body))) => Ok(body),
            Ok(_) => Err(NetError::Disconnected(
                "the connection died while this request was in flight".to_string(),
            )),
            Err(_) => {
                self.inner.pending.lock().await.remove(&id);
                Err(NetError::RequestTimeout)
            }
        }
    }

    /// Write one frame without registering a waiter.
    pub async fn send(&self, frame: Frame<Op>) -> Result<(), NetError> {
        self.write(assign_id(frame)?).await
    }

    /// Stop the handle for good, complete every in-flight waiter as a failure,
    /// and say `bye` on the live connection.
    ///
    /// `Ok` means the `bye` was queued on a connection that was live when the
    /// handle closed. A handle that was reconnecting or already closed has no
    /// such connection, so the handle still closes but the call answers
    /// `Disconnected`.
    pub async fn close(&self) -> Result<(), NetError> {
        let was = self
            .inner
            .stop(Some(NetError::Disconnected(
                "the handle closed the connection".to_string(),
            )))
            .await;
        if was != STATE_READY {
            return Err(NetError::Disconnected(
                "no live connection carried the bye".to_string(),
            ));
        }
        let bye = Frame::Bye {
            reason: CLOSE_REASON.to_string(),
        };
        if self.inner.outbound.send(bye).await.is_ok() {
            Ok(())
        } else {
            Err(NetError::Disconnected(
                "the connection task stopped".to_string(),
            ))
        }
    }

    /// Gate one frame on readiness, then queue it for the connection task.
    async fn write(&self, frame: Frame<Op>) -> Result<(), NetError> {
        match self.inner.readiness() {
            ConnReadiness::Ready => {}
            ConnReadiness::Reconnecting => return Err(NetError::NotReady),
            ConnReadiness::Closed => {
                return Err(self
                    .inner
                    .failure
                    .lock()
                    .await
                    .clone()
                    .unwrap_or(NetError::NotReady));
            }
        }
        self.inner
            .outbound
            .send(frame)
            .await
            .map_err(|_| NetError::Disconnected("the connection task stopped".to_string()))
    }
}

/// Give a request frame an `id`, so the server can match its answer.
///
/// Any other frame without an id passes through untouched.
fn assign_id<Op>(frame: Frame<Op>) -> Result<Frame<Op>, NetError> {
    match frame {
        Frame::Req { id, op } if id.is_empty() => Ok(Frame::Req { id: new_id(), op }),
        other => Ok(other),
    }
}

struct ConnInner<Op> {
    role: String,
    outbound: mpsc::Sender<Frame<Op>>,
    events: broadcast::Sender<Frame<Op>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<ResBody, NetError>>>>,
    failure: Mutex<Option<NetError>>,
    state: AtomicU8,
    /// Newest `ev` sequence seen, echoed as `pong.server_seq`.
    cursor: AtomicU64,
    /// Events evicted from the local queue since the last report.
    dropped: AtomicU64,
    resync_lag: usize,
}

impl<Op> ConnInner<Op> {
    fn readiness(&self) -> ConnReadiness {
        match self.state.load(Ordering::SeqCst) {
            STATE_READY => ConnReadiness::Ready,
            STATE_RECONNECTING => ConnReadiness::Reconnecting,
            _ => ConnReadiness::Closed,
        }
    }

    /// Move from one live state to another. Refused once the handle is closed,
    /// so nothing can bring a closed handle back.
    fn advance(&self, from: u8, to: u8) -> bool {
        self.state
            .compare_exchange(from, to, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    fn is_stopped(&self) -> bool {
        self.state.load(Ordering::SeqCst) == STATE_CLOSED
    }

    /// Stop for good, recording the reason callers read from `failure`, and
    /// answer the state the handle left.
    async fn stop(&self, error: Option<NetError>) -> u8 {
        let error = error.unwrap_or(NetError::Disconnected("the connection closed".to_string()));
        *self.failure.lock().await = Some(error.clone());
        let was = self.state.swap(STATE_CLOSED, Ordering::SeqCst);
        fail_pending(self, error).await;
        was
    }
}

/// How one handle opens each connection after the first.
///
/// `dial` redials its endpoint and runs the `hello` exchange again. A stream the
/// caller adopted carries no redial plan, so its first death stops the handle.
trait Reopen {
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    /// Whether a death can be followed by another connection.
    fn can_redial(&self) -> bool;

    /// Open the next connection.
    fn reopen(&mut self) -> impl Future<Output = Result<Self::Stream, NetError>> + Send;
}

/// One dial plan, reused by every reconnect attempt.
struct DialPlan {
    endpoint: String,
    pin: String,
    role: String,
    keys: KeyPair,
    settings: ConnSettings,
}

impl Reopen for DialPlan {
    type Stream = ClientTls;

    fn can_redial(&self) -> bool {
        true
    }

    async fn reopen(&mut self) -> Result<ClientTls, NetError> {
        establish(self).await
    }
}

/// An already-connected stream, adopted by its caller.
struct AdoptedPlan<S> {
    marker: std::marker::PhantomData<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> Reopen for AdoptedPlan<S> {
    type Stream = S;

    fn can_redial(&self) -> bool {
        false
    }

    async fn reopen(&mut self) -> Result<S, NetError> {
        Err(NetError::Disconnected(
            "the attached stream has no redial".to_string(),
        ))
    }
}

/// Dial an endpoint, verify its pin, finish the `hello` exchange, then serve one
/// shared connection through the returned handle.
///
/// The handle comes back ready. A later death starts the reconnect loop inside
/// the supervisor task, and `request` answers `NetError::NotReady` while that
/// loop dials.
pub async fn dial<Op>(
    endpoint: &str,
    keypair: &KeyPair,
    expected_pin: &str,
    role: &str,
    settings: ConnSettings,
) -> Result<ConnHandle<Op>, NetError>
where
    Op: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    let settings = ConnSettings {
        resync_lag: settings.resync_lag.max(2),
        ..settings
    };
    let plan = DialPlan {
        endpoint: endpoint.to_string(),
        pin: expected_pin.to_string(),
        role: role.to_string(),
        keys: keypair.clone(),
        settings,
    };
    let stream = establish(&plan).await?;
    let settings = plan.settings.clone();
    Ok(spawn_handle(stream, role, &settings, plan))
}

/// Build the shared channel state, then start the connection task.
fn spawn_handle<P, Op>(
    stream: P::Stream,
    role: &str,
    settings: &ConnSettings,
    plan: P,
) -> ConnHandle<Op>
where
    P: Reopen + Send + 'static,
    Op: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    let (outbound, outbound_rx) = mpsc::channel(OUTBOUND_QUEUE_DEPTH);
    let (events, _) = broadcast::channel(settings.resync_lag);
    let inner = Arc::new(ConnInner {
        role: role.to_string(),
        outbound,
        events,
        pending: Mutex::new(HashMap::new()),
        failure: Mutex::new(None),
        state: AtomicU8::new(STATE_READY),
        cursor: AtomicU64::new(0),
        dropped: AtomicU64::new(0),
        resync_lag: settings.resync_lag,
    });
    tokio::spawn(run_supervisor(
        stream,
        Arc::clone(&inner),
        outbound_rx,
        settings.clone(),
        plan,
    ));
    ConnHandle { inner }
}

/// Open one authenticated connection: pinned TLS, then the `hello` challenge.
async fn establish(plan: &DialPlan) -> Result<ClientTls, NetError> {
    let mut stream = connect_client(&plan.endpoint, &plan.pin).await?;
    let ack = connect_with_timeout(
        &mut stream,
        &plan.role,
        &plan.keys,
        plan.settings.protocol,
        &plan.settings.agent,
        &plan.settings.version,
        plan.settings.aggregate,
        plan.settings.handshake_timeout,
    )
    .await?;
    if !ack.ok {
        return Err(NetError::Rejected {
            code: "rejected".to_string(),
            message: "the server refused the hello".to_string(),
        });
    }
    if ack.role != plan.role {
        return Err(NetError::Unauthorized(format!(
            "the server bound this connection to role {}, not {}",
            ack.role, plan.role
        )));
    }
    Ok(stream)
}

/// What a retry loop should do about a connection failure.
///
/// The same [`Retry`] vocabulary `ErrorCode::retry` answers with, because these
/// are the two places a failure is read: at the socket, and on the wire. They
/// were two functions of the same name giving opposite answers for
/// `unauthorized`, and a reader had to know which layer they were looking at
/// before they could act.
///
/// An identity or version failure is `AfterHuman` rather than `Never` on
/// purpose: both end the run, and saying which one it is turns "the client
/// stopped" into "re-key this role" or "upgrade one side".
pub fn retry_of(error: &NetError) -> Retry {
    match error {
        NetError::ProtocolVersion { .. }
        | NetError::Unauthorized(_)
        | NetError::PinMismatch { .. }
        | NetError::MalformedKey(_)
        | NetError::Crypto(_) => Retry::AfterHuman,
        NetError::Rejected { code, .. } => {
            if code == ErrorCode::Unauthorized.as_str()
                || code == ErrorCode::ProtocolVersion.as_str()
                || code == "rejected"
            {
                Retry::AfterHuman
            } else {
                Retry::Never
            }
        }
        NetError::HandshakeTimeout
        | NetError::RequestTimeout
        | NetError::NotReady
        | NetError::Disconnected(_)
        | NetError::Io(_) => Retry::UnderBackoff,
        // A frame this build could not parse is a protocol violation between
        // two builds of this program, not a hiccup in the pipe.
        NetError::FrameTooLarge | NetError::BadFrame => Retry::Never,
    }
}

/// Read the evicted-event count carried by a synthetic resync frame.
///
/// The count sits in the frame's `seq` field, and matches `FaultEvent::seq`.
pub fn resync_lag_of<Op>(frame: &Frame<Op>) -> Option<u64> {
    match frame {
        Frame::Ev { seq, event } => match event.as_ref() {
            Event::Fault(fault) if fault.kind == RESYNC_LAG_KIND => Some(*seq),
            _ => None,
        },
        _ => None,
    }
}

/// How one session ended.
enum SessionEnd {
    /// Local `close` or a peer `bye`: the connection ended on purpose.
    Closed(String),
    /// The transport died; the supervisor dials again.
    Dead(NetError),
}

/// Serve one live session, then report how it ended.
async fn run_session<S, Op>(
    stream: &mut S,
    inner: &Arc<ConnInner<Op>>,
    outbound: &mut mpsc::Receiver<Frame<Op>>,
    settings: &ConnSettings,
) -> SessionEnd
where
    S: AsyncRead + AsyncWrite + Unpin,
    Op: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + settings.heartbeat,
        settings.heartbeat,
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ping_sent: Option<tokio::time::Instant> = None;
    // The read branch is dropped whenever another branch wins; the reader keeps
    // the bytes it had already pulled, so frame boundaries survive that.
    let mut reader = FrameReader::new();
    loop {
        tokio::select! {
            outgoing = outbound.recv() => {
                let Some(frame) = outgoing else {
                    return SessionEnd::Closed("the handle dropped".to_string());
                };
                let bye = matches!(frame, Frame::Bye { .. });
                if let Err(error) = write_frame(stream, &frame).await.map_err(map_frame_error) {
                    return SessionEnd::Dead(error);
                }
                if bye {
                    return SessionEnd::Closed("the handle closed the connection".to_string());
                }
            }
            incoming = reader.next::<_, Frame<Op>>(stream) => {
                let frame = match incoming {
                    Ok(Some(frame)) => frame,
                    Ok(None) => {
                        return SessionEnd::Dead(NetError::Disconnected(
                            "the peer closed the connection".to_string(),
                        ));
                    }
                    Err(error) => return SessionEnd::Dead(map_frame_error(error)),
                };
                match frame {
                    Frame::Res { id, body } => settle(inner, &id, body).await,
                    Frame::Ev { seq, event } => {
                        inner.cursor.store(seq, Ordering::SeqCst);
                        publish(inner, Frame::Ev { seq, event });
                    }
                    // The observation cursor travels on `pong`, so subscribers
                    // see it and can ask for a resync from the last good `seq`.
                    Frame::Pong { t, server_seq } => {
                        ping_sent = None;
                        publish(inner, Frame::Pong { t, server_seq });
                    }
                    Frame::Ack { seq } => publish(inner, Frame::Ack { seq }),
                    Frame::Ping { t } => {
                        let pong: Frame<Op> = Frame::Pong {
                            t,
                            server_seq: inner.cursor.load(Ordering::SeqCst),
                        };
                        if let Err(error) = write_frame(stream, &pong).await.map_err(map_frame_error) {
                            return SessionEnd::Dead(error);
                        }
                    }
                    Frame::Bye { reason } => {
                        return SessionEnd::Closed(format!("the peer said bye: {reason}"));
                    }
                    Frame::Req { .. } => {}
                }
            }
            _ = heartbeat.tick() => {
                match ping_sent {
                    Some(sent) if sent.elapsed() >= settings.pong_deadline() => {
                        return SessionEnd::Dead(NetError::Disconnected(format!(
                            "no pong within {:?}",
                            settings.pong_deadline()
                        )));
                    }
                    Some(_) => {}
                    None => {
                        ping_sent = Some(tokio::time::Instant::now());
                        let ping: Frame<Op> = Frame::Ping { t: now_ms() };
                        if let Err(error) = write_frame(stream, &ping).await.map_err(map_frame_error) {
                            return SessionEnd::Dead(error);
                        }
                    }
                }
                report_lag(inner);
            }
        }
    }
}

/// Keep one connection alive, redialing under `Backoff` after every death.
async fn run_supervisor<S, P, Op>(
    mut stream: S,
    inner: Arc<ConnInner<Op>>,
    mut outbound: mpsc::Receiver<Frame<Op>>,
    settings: ConnSettings,
    mut plan: P,
) where
    S: AsyncRead + AsyncWrite + Unpin,
    P: Reopen<Stream = S>,
    Op: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
{
    let mut backoff = settings.backoff.clone();
    loop {
        match run_session(&mut stream, &inner, &mut outbound, &settings).await {
            SessionEnd::Closed(reason) => {
                inner.stop(Some(NetError::Disconnected(reason))).await;
                return;
            }
            SessionEnd::Dead(error) => {
                // A `close` that raced the death already failed every waiter.
                if !inner.advance(STATE_READY, STATE_RECONNECTING) {
                    return;
                }
                fail_pending(&inner, error.clone()).await;
                drain(&mut outbound);
                if !plan.can_redial() {
                    inner
                        .stop(Some(NetError::Disconnected(format!(
                            "{error}; the attached stream has no redial"
                        ))))
                        .await;
                    return;
                }
                loop {
                    if inner.is_stopped() {
                        return;
                    }
                    tokio::time::sleep(backoff.next()).await;
                    if inner.is_stopped() {
                        return;
                    }
                    match plan.reopen().await {
                        Ok(mut fresh) => {
                            // A `close` can land while `reopen` runs; the fresh
                            // connection then only carries the `bye`. Its write
                            // error has no caller left to reach, since `close`
                            // already recorded the handle's failure.
                            if !inner.advance(STATE_RECONNECTING, STATE_READY) {
                                let bye: Frame<Op> = Frame::Bye {
                                    reason: CLOSE_REASON.to_string(),
                                };
                                let _ = write_frame(&mut fresh, &bye).await;
                                return;
                            }
                            backoff.reset();
                            stream = fresh;
                            break;
                        }
                        Err(error) => {
                            if retry_of(&error).ends_the_run() {
                                inner.stop(Some(error)).await;
                                return;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Drop the frames that belonged to the dead connection.
fn drain<Op>(outbound: &mut mpsc::Receiver<Frame<Op>>) {
    while outbound.try_recv().is_ok() {}
}

/// Hand one `res` body to its waiter.
async fn settle<Op>(inner: &ConnInner<Op>, id: &str, body: ResBody) {
    let waiter = inner.pending.lock().await.remove(id);
    if let Some(waiter) = waiter {
        let _ = waiter.send(Ok(body));
    }
}

async fn fail_pending<Op>(inner: &ConnInner<Op>, error: NetError) {
    let mut pending = inner.pending.lock().await;
    for (_, waiter) in pending.drain() {
        let _ = waiter.send(Err(error.clone()));
    }
}

/// Push one observation frame into the bounded local queue.
///
/// The queue holds `resync_lag` frames for the slowest subscriber. Writing into
/// a full queue evicts that subscriber's oldest frame, so the eviction is
/// counted and the next frame with room reports it once.
fn publish<Op: Clone>(inner: &ConnInner<Op>, frame: Frame<Op>) {
    if inner.events.len() >= inner.resync_lag {
        inner.dropped.fetch_add(1, Ordering::SeqCst);
    }
    report_lag(inner);
    let _ = inner.events.send(frame);
}

/// Report accumulated evictions as one synthetic `resync_lag` frame.
///
/// Two free queue slots are required, so the report and the frame behind it both
/// survive the write.
fn report_lag<Op: Clone>(inner: &ConnInner<Op>) {
    if inner.dropped.load(Ordering::SeqCst) == 0 || inner.events.len() + 2 > inner.resync_lag {
        return;
    }
    let lag = inner.dropped.swap(0, Ordering::SeqCst);
    let _ = inner.events.send(resync_frame(lag));
}

/// One synthetic frame carrying the number of events a subscriber lost.
fn resync_frame<Op>(dropped: u64) -> Frame<Op> {
    Frame::Ev {
        seq: dropped,
        event: Box::new(Event::Fault(FaultEvent {
            kind: RESYNC_LAG_KIND.to_string(),
            reason: format!("{dropped} events fell out of the local queue"),
            seq: Some(dropped),
            ..FaultEvent::default()
        })),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}
