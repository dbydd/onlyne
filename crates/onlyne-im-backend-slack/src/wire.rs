//! The connector the Web API client and the Socket Mode listener share.
//!
//! Slack Morphism hard-codes `https://slack.com/api` in
//! `create_method_uri_path`, and every call this backend makes — the Web API's
//! and the listener's own `apps.connections.open` — is addressed through that
//! method. It is a constructor setting on the SDK's connector, so a local fake
//! or an operator's own host can stand in for Slack without touching a request
//! shape.

use slack_morphism::prelude::*;

/// The SDK's hyper connector: TLS, connection pooling, its per-method rate
/// limits, and nothing of this crate's.
pub type SlackConnector = SlackClientHyperHttpsConnector;

/// The connector every call goes through, on `base` when one is named.
///
/// This is the SDK's own constructor, with one difference: it is built to
/// reach `http` as well as `https`. Slack is `https` only, and a fake answers
/// plain `http` on a loopback port; the SDK's constructor refuses such a root
/// before a request is ever made.
///
/// The root is a setting of the connector, not of the process: a client holds
/// the connector it was built with, so two clients on two roots never share
/// one.
///
/// The rustls provider is named here, before any connector exists. The SDK's
/// hyper connector is built on `hyper-rustls`, and with more than one provider
/// compiled into a binary (this workspace's `ring` and another dependency's
/// `aws-lc-rs`) rustls refuses to guess and panics on the first handshake. A
/// backend process installs the provider in `main`; a caller that builds a
/// client without one — a test, an embedding — would otherwise reach that
/// panic, so the install belongs where the connector is made. Installing twice
/// is the same provider and not an error.
pub fn connector(base: Option<&str>) -> std::io::Result<SlackConnector> {
    onlyne_im_backend::install_crypto_provider();
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()?
        .https_or_http()
        .enable_http1()
        .build();
    let connector = SlackClientHyperConnector::with_connector(https);
    Ok(match base.filter(|base| !base.is_empty()) {
        Some(base) => connector.with_slack_api_url(base),
        None => connector,
    })
}
