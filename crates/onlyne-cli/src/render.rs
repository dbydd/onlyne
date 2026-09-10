//! Answer rendering: compact by default, two-space indented with `--pretty`.

use onlyne_proto::{ErrorCode, ErrorPayload, Frame, ResBody};
use serde::Serialize;
use std::path::PathBuf;

use crate::flags::GlobalFlags;

/// The `data` object of a `pong` answer, in wire field order.
#[derive(Debug, Serialize)]
struct PongData {
    t: i64,
    server_seq: u64,
}

/// The `pong` answer wrapped in a response body.
#[derive(Debug, Serialize)]
struct PongBody {
    ok: bool,
    data: PongData,
}

fn encode<T: Serialize>(pretty: bool, value: &T) -> String {
    if pretty {
        serde_json::to_string_pretty(value).expect("serialisable value")
    } else {
        serde_json::to_string(value).expect("serialisable value")
    }
}

/// Render an answer body. `--quiet` keeps only the payload a script wants:
/// `data` on success, `error` on failure.
pub fn render_body(body: &ResBody, flags: &GlobalFlags) -> String {
    if !flags.quiet {
        return encode(flags.pretty, body);
    }
    let payload = if body.ok {
        body.data.clone().unwrap_or(serde_json::Value::Null)
    } else if let Some(error) = &body.error {
        serde_json::to_value(error).expect("serialisable value")
    } else {
        serde_json::Value::Null
    };
    encode(false, &payload)
}

/// Render a `pong` frame as the response body scripts expect.
pub fn render_pong(frame: &Frame::Pong, flags: &GlobalFlags) -> String {
    let body = PongBody {
        ok: true,
        data: PongData {
            t: frame.t,
            server_seq: frame.server_seq,
        },
    };
    if flags.quiet {
        return encode(false, &body.data);
    }
    encode(flags.pretty, &body)
}

/// Render an answer a script sees locally, without a daemon in the loop.
pub fn local_error_json(
    code: ErrorCode,
    message: impl Into<String>,
    field: Option<String>,
) -> String {
    let body = ResBody {
        ok: false,
        data: None,
        error: Some(ErrorPayload {
            code,
            message: message.into(),
            field,
        }),
    };
    encode(false, &body)
}

/// The socket-timeout answer, printed so a script always sees JSON.
pub fn timeout_json(timeout_ms: u64) -> String {
    local_error_json(
        ErrorCode::Internal,
        format!("socket timeout after {timeout_ms}ms"),
        None,
    )
}

/// The `onlyne version` report, probing each sibling for a path.
pub fn version_json() -> String {
    encode(
        false,
        &VersionReport {
            onlyne_cli: env!("CARGO_PKG_VERSION"),
            protocol: onlyne_proto::PROTOCOL_VERSION,
            binaries: Siblings {
                onlyne_server: sibling("onlyne-server"),
                onlyne_client: sibling("onlyne-client"),
                onlyne_gateway: sibling("onlyne-gateway"),
            },
        },
    )
}

fn sibling(name: &str) -> Option<PathBuf> {
    crate::forward::resolve_sibling(name)
}

/// Field order matches the documented report shape.
#[derive(Debug, Serialize)]
struct VersionReport<'a> {
    #[serde(rename = "onlyne-cli")]
    onlyne_cli: &'a str,
    protocol: u16,
    binaries: Siblings,
}

#[derive(Debug, Serialize)]
struct Siblings {
    #[serde(rename = "onlyne-server")]
    onlyne_server: Option<PathBuf>,
    #[serde(rename = "onlyne-client")]
    onlyne_client: Option<PathBuf>,
    #[serde(rename = "onlyne-gateway")]
    onlyne_gateway: Option<PathBuf>,
}
