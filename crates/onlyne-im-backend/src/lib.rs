//! IM gateway backend protocol: standard frames and payload types between
//! the IM gateway core and platform backends.
//!
//! The backend is a separate process that speaks this protocol on stdio
//! (or a Unix socket) with the IM gateway core. The core spawns it.
//!
//! Frames are length-prefixed JSON (same codec as `onlyne-wire`), with a
//! `op` field selecting the variant.

pub mod backend;
pub mod credential;
pub mod frame;
pub mod payload;
pub mod serve;

/// Pick the process-wide rustls `CryptoProvider`, once, before any TLS handshake.
///
/// A platform backend links rustls through more than one dependency, and those
/// dependencies do not agree on a provider: `openlark`'s `reqwest 0.13` enables
/// rustls' default (`aws-lc-rs`) while this workspace's own `reqwest` builds
/// against `ring`, which is what `onlyne-net` uses everywhere. With both
/// providers compiled into one image, rustls refuses to guess and panics on the
/// first handshake — measured on a live Feishu run, where the backend died the
/// moment its websocket connected.
///
/// So every backend process names the provider instead: `ring`, the one the
/// rest of this workspace already carries. A process that installs it twice
/// (two entry points, a test harness) is not an error — the first install wins
/// and the second is the same provider.
pub fn install_crypto_provider() {
    // The error is the benign "already installed" case; a real failure would be
    // a rustls built without the `ring` feature, which this crate's own
    // dependency on it makes impossible.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Install the crypto provider and initialize `tracing` for a backend process.
///
/// Pass no arguments for the `"info"` default, or a filter expression to use
/// when `RUST_LOG` is unset.
#[macro_export]
macro_rules! init_backend_runtime {
    () => {
        $crate::init_backend_runtime!("info")
    };
    ($default_filter:expr) => {{
        $crate::install_crypto_provider();
        use tracing_subscriber::EnvFilter;
        let filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new($default_filter));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init();
    }};
}

/// Await the backend's future and exit the process with its result's code,
/// printing `Err` payloads to stderr.
pub async fn exit_on_result(bin_name: &str, fut: impl Future<Output = anyhow::Result<()>>) -> ! {
    match fut.await {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            eprintln!("{bin_name}: {error:#}");
            std::process::exit(1);
        }
    }
}

pub use backend::Backend;
pub use credential::{CREDENTIAL_ENV, GATEWAY_ENV, credential_path};
pub use frame::{
    BackendOp, CoreOp, HealthArgs, HelloArgs, ImConversation, ImConversations,
    ListConversationsArgs, LoginArgs, LoginResult, OnboardingKind, OnboardingPrompt, SendArgs,
    StopArgs,
};
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
            reply_reference: None,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ImPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }
}
