//! Exit-code table, the socket-resolution entry point, and socket error mapping.

use onlyne_proto::{ErrorCode, ResBody};
use std::future::Future;
use std::io::ErrorKind;
use tokio::net::UnixStream;

use crate::flags::GlobalFlags;
use crate::render;
use crate::socket::{resolve_socket, NoSocket, SocketTarget};
use crate::wire::{self, ExchangeError};

pub const EXIT_OK: i32 = 0;
pub const EXIT_ANSWER_FAILED: i32 = 1;
pub const EXIT_VALIDATION: i32 = 2;
pub const EXIT_NO_SOCKET: i32 = 3;
pub const EXIT_NO_SIBLING: i32 = 127;

/// Run one verb's future on a fresh runtime.
pub fn block_on<F: Future<Output = i32>>(future: F) -> i32 {
    match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime.block_on(future),
        Err(error) => {
            eprintln!("onlyne: cannot start event loop: {error}");
            EXIT_VALIDATION
        }
    }
}

/// Resolve the socket, printing the byte-exact message when nothing is found.
pub fn target(flags: &GlobalFlags) -> Option<SocketTarget> {
    match resolve_socket(flags) {
        Ok(target) => Some(target),
        Err(NoSocket) => {
            eprintln!("{}", NoSocket::MESSAGE);
            None
        }
    }
}

/// A local validation failure: printed to stderr, exit 2.
pub fn usage_error(message: impl Into<String>) -> i32 {
    eprintln!("{}", message.into());
    EXIT_VALIDATION
}

/// The `--request` failure shape, carrying the serde message.
pub fn request_error(message: String) -> i32 {
    usage_error(format!("onlyne: --request: {message}"))
}

/// Open the socket for one verb, printing the local JSON answer on failure.
pub async fn open(flags: &GlobalFlags, target: &SocketTarget) -> Result<UnixStream, i32> {
    match wire::connect(&target.path, flags.timeout_ms).await {
        Ok(stream) => Ok(stream),
        Err(error) => Err(connect_error(&error, flags.timeout_ms)),
    }
}

/// A socket that refused, disappeared, or never answered in time.
pub fn connect_error(error: &std::io::Error, timeout_ms: u64) -> i32 {
    let json = if error.kind() == ErrorKind::TimedOut {
        render::timeout_json(timeout_ms)
    } else {
        render::local_error_json(ErrorCode::Internal, error.to_string(), None)
    };
    println!("{json}");
    EXIT_ANSWER_FAILED
}

/// An exchange failure, printed as JSON so a script always sees JSON.
pub fn exchange_error(error: &ExchangeError, timeout_ms: u64) -> i32 {
    let (code, message) = match error {
        ExchangeError::Timeout => {
            (ErrorCode::Internal, format!("socket timeout after {timeout_ms}ms"))
        }
        ExchangeError::Closed => {
            (ErrorCode::Internal, "socket closed before an answer arrived".to_string())
        }
        ExchangeError::Wire(code, message) => (*code, message.clone()),
    };
    println!("{}", render::local_error_json(code, message, None));
    EXIT_ANSWER_FAILED
}

/// Print one answer body and return its exit code.
pub fn finish(body: &ResBody, flags: &GlobalFlags) -> i32 {
    println!("{}", render::render_body(body, flags));
    if body.ok {
        EXIT_OK
    } else {
        EXIT_ANSWER_FAILED
    }
}
