//! One request frame out, one answer frame in, each bounded by `--timeout`.

use onlyne_layout::{LocalStream, connect_local};
use onlyne_proto::{AdminOp, ClientOp, ErrorCode, Frame, ResBody};
use serde::{Deserialize, Serialize};
use std::io::{ErrorKind, Result as IoResult};
use std::path::Path;
use tokio::time::{Duration, timeout};

/// Request frame on the local admin socket. [`onlyne_proto::Frame::Req`]
/// carries a `ClientOp`, so the admin vocabulary travels through this mirror
/// with the same `f`, `id`, `op`, and `args` JSON shape.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "f")]
pub enum AdminFrame {
    Req {
        id: String,
        #[serde(flatten)]
        op: AdminOp,
    },
}

/// One request frame, in whichever vocabulary the surface needs.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Outbound {
    Client(Frame),
    Admin(AdminFrame),
}

impl Outbound {
    pub fn client(id: String, op: ClientOp) -> Self {
        Outbound::Client(Frame::req(id, op))
    }

    pub fn admin(id: String, op: AdminOp) -> Self {
        Outbound::Admin(AdminFrame::Req { id, op })
    }
}

/// A socket operation failed.
#[derive(Debug)]
pub enum ExchangeError {
    /// The `--timeout` bound elapsed.
    Timeout,
    /// The stream ended cleanly before any answer arrived.
    Closed,
    /// A frame-level or transport failure, mapped onto a wire error code.
    Wire(ErrorCode, String),
}

fn wire_error(error: &std::io::Error) -> ExchangeError {
    let code = match error.kind() {
        ErrorKind::InvalidInput => ErrorCode::FrameTooLarge,
        ErrorKind::InvalidData => ErrorCode::BadFrame,
        _ => ErrorCode::Internal,
    };
    ExchangeError::Wire(code, error.to_string())
}

/// Connect to the local socket, bounded by `--timeout`.
///
/// Windows `ERROR_PIPE_BUSY` is remapped to `WouldBlock` inside
/// [`connect_local`]; this loop retries every 20ms until the bound elapses so
/// the operator-facing timeout text stays the same.
pub async fn connect(path: &Path, timeout_ms: u64) -> IoResult<LocalStream> {
    match timeout(
        Duration::from_millis(timeout_ms),
        connect_with_busy_retry(path),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::new(
            ErrorKind::TimedOut,
            format!("socket timeout after {timeout_ms}ms"),
        )),
    }
}

async fn connect_with_busy_retry(path: &Path) -> IoResult<LocalStream> {
    loop {
        match connect_local(path).await {
            Ok(stream) => return Ok(stream),
            Err(error) if connect_is_busy(&error) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn connect_is_busy(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::WouldBlock | ErrorKind::ResourceBusy
    ) || error.raw_os_error() == Some(231)
}

/// Write one frame, bounded by `--timeout`.
pub async fn send_frame<T: Serialize + ?Sized>(
    stream: &mut LocalStream,
    frame: &T,
    timeout_ms: u64,
) -> Result<(), ExchangeError> {
    match timeout(
        Duration::from_millis(timeout_ms),
        onlyne_frame::write_frame(stream, frame),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(wire_error(&error)),
        Err(_) => Err(ExchangeError::Timeout),
    }
}

/// Read one answer frame, bounded by `--timeout`.
pub async fn recv_frame(stream: &mut LocalStream, timeout_ms: u64) -> Result<Frame, ExchangeError> {
    match timeout(
        Duration::from_millis(timeout_ms),
        onlyne_frame::read_frame::<_, Frame>(stream),
    )
    .await
    {
        Ok(Ok(Some(frame))) => Ok(frame),
        Ok(Ok(None)) => Err(ExchangeError::Closed),
        Ok(Err(error)) => Err(wire_error(&error)),
        Err(_) => Err(ExchangeError::Timeout),
    }
}

/// Send one request and read the `res` frame that answers it.
pub async fn request_res<T: Serialize + ?Sized>(
    stream: &mut LocalStream,
    request: &T,
    timeout_ms: u64,
) -> Result<ResBody, ExchangeError> {
    send_frame(stream, request, timeout_ms).await?;
    match recv_frame(stream, timeout_ms).await? {
        Frame::Res { body, .. } => Ok(body),
        other => Err(ExchangeError::Wire(
            ErrorCode::BadFrame,
            format!("expected a res frame, got a {} frame", frame_name(&other)),
        )),
    }
}

pub fn frame_name(frame: &Frame) -> &'static str {
    match frame {
        Frame::Req { .. } => "req",
        Frame::Res { .. } => "res",
        Frame::Ev { .. } => "ev",
        Frame::Ack { .. } => "ack",
        Frame::Ping { .. } => "ping",
        Frame::Pong { .. } => "pong",
        Frame::Bye { .. } => "bye",
    }
}
