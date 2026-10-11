//! IM gateway backend protocol: standard frames and payload types between
//! the IM gateway core and platform backends.
//!
//! The backend is a separate process that speaks this protocol on stdio
//! (or a Unix socket) with the IM gateway core. The core spawns it.
//!
//! Frames are length-prefixed JSON (same codec as `onlyne-wire`), with a
//! `op` field selecting the variant.

pub mod backend;
pub mod frame;
pub mod payload;
pub mod serve;

pub use backend::{Backend, BackendCapabilities};
pub use frame::{BackendOp, CoreOp, HealthArgs, HelloArgs, SendArgs, StopArgs};
pub use payload::{
    ImAction, ImAttachment, ImConversationId, ImEvent, ImEventKind, ImFormat, ImGatewayId,
    ImPayload, ImPlatform, ImSender,
};
pub use serve::{serve, serve_with};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_roundtrip() {
        let p = ImPayload {
            text: "hello".into(),
            format: ImFormat::Plain,
            attachments: vec![],
            actions: vec![],
            reply_to: None,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ImPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }
}
