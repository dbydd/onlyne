use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    PinMismatch {
        expected: String,
        got: String,
    },
    ProtocolVersion {
        peer: u16,
        expected: u16,
    },
    Unauthorized(String),
    Rejected {
        code: String,
        message: String,
    },
    /// The transport died. The supervisor dials again.
    Disconnected(String),
    /// The handle sits between connections and accepts no frame.
    NotReady,
    /// The caller's own deadline elapsed while the request was in flight.
    RequestTimeout,
    HandshakeTimeout,
    FrameTooLarge,
    BadFrame,
    Io(String),
    Crypto(String),
    MalformedKey(String),
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PinMismatch { expected, got } => write!(
                f,
                "certificate pin mismatch: expected {expected}, got {got}"
            ),
            Self::ProtocolVersion { peer, expected } => write!(
                f,
                "protocol version mismatch: peer {peer}, expected {expected}"
            ),
            Self::Unauthorized(detail) => write!(f, "unauthorized: {detail}"),
            Self::Rejected { code, message } => write!(f, "rejected ({code}): {message}"),
            Self::Disconnected(detail) => write!(f, "disconnected: {detail}"),
            Self::NotReady => f.write_str("the connection is not ready"),
            Self::RequestTimeout => f.write_str("request timed out"),
            Self::HandshakeTimeout => f.write_str("handshake timed out"),
            Self::FrameTooLarge => f.write_str("frame too large"),
            Self::BadFrame => f.write_str("bad frame"),
            Self::Io(detail) => write!(f, "I/O error: {detail}"),
            Self::Crypto(detail) => write!(f, "cryptographic error: {detail}"),
            Self::MalformedKey(detail) => write!(f, "malformed key: {detail}"),
        }
    }
}

impl std::error::Error for NetError {}

impl From<std::io::Error> for NetError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<serde_json::Error> for NetError {
    fn from(_error: serde_json::Error) -> Self {
        Self::BadFrame
    }
}

impl From<ed25519_dalek::SignatureError> for NetError {
    fn from(error: ed25519_dalek::SignatureError) -> Self {
        Self::Crypto(error.to_string())
    }
}
