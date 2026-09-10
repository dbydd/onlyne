//! One request frame out, one answer frame in, each bounded by `--timeout`.

use onlyne_proto::{AdminOp, ClientOp, ErrorCode, Frame, ResBody};
use serde::{Deserialize, Serialize};
use std::io::{ErrorKind, Result as IoResult};
use std::path::Path;
use tokio::net::UnixStream;
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

/// Request frame for an op the proto vocabulary does not declare yet.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "f")]
pub enum ExtensionFrame {
    Req {
        id: String,
        op: &'static str,
        args: serde_json::Value,
    },
}

/// One request frame, in whichever vocabulary the surface needs.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Outbound {
    Client(Frame),
    Admin(AdminFrame),
    Extension(ExtensionFrame),
}

impl Outbound {
    pub fn client(id: String, op: ClientOp) -> Self {
        Outbound::Client(Frame::req(id, op))
    }

    pub fn admin(id: String, op: AdminOp) -> Self {
        Outbound::Admin(AdminFrame::Req { id, op })
    }

    pub fn extension(id: String, op: &'static str, args: serde_json::Value) -> Self {
        Outbound::Extension(ExtensionFrame::Req { id, op, args })
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

/// Connect to a unix socket, bounded by `--timeout`.
pub async fn connect(path: &Path, timeout_ms: u64) -> IoResult<UnixStream> {
    match timeout(Duration::from_millis(timeout_ms), UnixStream::connect(path)).await {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::new(
            ErrorKind::TimedOut,
            format!("socket timeout after {timeout_ms}ms"),
        )),
    }
}

/// Write one frame, bounded by `--timeout`.
pub async fn send_frame<T: Serialize + ?Sized>(
    stream: &mut UnixStream,
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
pub async fn recv_frame(stream: &mut UnixStream, timeout_ms: u64) -> Result<Frame, ExchangeError> {
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
    stream: &mut UnixStream,
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
