//! QQ Open Platform backend for the Onlyne IM gateway.
//!
//! Implements the standard backend protocol (`onlyne-im-backend`) and speaks
//! to QQ's Open Platform: an app access token over HTTP, message sends per
//! scene, and the websocket event gateway. The translation halves live in
//! [`update`] (a platform dispatch becomes a standard event), [`render`]
//! (a standard payload becomes a QQ message body), and [`ws`] (the gateway
//! protocol); [`api`] is the HTTP client and [`credential`] the credential
//! encoding. This module is the [`Backend`] that joins them.

pub mod api;
mod credential;
mod render;
mod update;
mod ws;

use anyhow::{Context, Result};
pub use api::Endpoint;
use api::{QqApi, Scene};
use async_trait::async_trait;
use credential::QqCredentials;
use onlyne_im_backend::backend::Backend;
use onlyne_im_backend::frame::{
    HealthArgs, HelloArgs, ImConversation, LoginArgs, OnboardingPrompt, SendArgs, SentArgs,
};
use onlyne_im_backend::payload::ImEvent;
use std::time::Instant;
use ws::Session;

/// The conventional credential variables, for an operator who exports them
/// instead of running `onlyne-im login`.
pub const APP_ID_ENV: &str = credential::APP_ID_ENV;
pub const APP_SECRET_ENV: &str = credential::APP_SECRET_ENV;
/// The single combined variable the credential loader reads first.
pub const CREDENTIAL_ENV: &str = "QQBOT_CREDENTIAL";

/// One QQ backend: the API client, the send-sequence counter, and the
/// gateway session the events arrive through.
pub struct QqBotBackend {
    gateway: String,
    platform: String,
    api: QqApi,
    sequence: render::Sequence,
    started: Instant,
    /// Why no credential was found, when that is the case. It becomes the
    /// onboarding sentence, so an operator reads the exact reason.
    problem: Option<String>,
    /// The websocket session, when the event stream is (or was) connected;
    /// its `session_id` and last sequence are what a reconnect resumes with.
    session: Option<Session>,
}

impl QqBotBackend {
    pub fn new(api: QqApi, gateway: impl Into<String>) -> Self {
        Self {
            gateway: gateway.into(),
            platform: "qqbot".to_string(),
            api,
            sequence: render::Sequence::new(),
            started: Instant::now(),
            problem: None,
            session: None,
        }
    }

    /// Build from the environment: `--token`, then the credential file the
    /// gateway names, then the QQ variables. A missing credential is carried
    /// as [`Self::problem`] rather than raised, because it is the state
    /// `onlyne-im login` exists to fix and this backend has to be running to
    /// answer that login.
    pub fn from_env(
        explicit: Option<String>,
        gateway: impl Into<String>,
        endpoint: Endpoint,
    ) -> Self {
        // The loader's single-variable source is the combined form; the pair
        // the console shows (`QQBOT_APP_ID` + `QQBOT_APP_SECRET`) is the last
        // resort, so exporting both is a working configuration rather than a
        // promise the onboarding sentence makes and the loader never reads.
        let loaded = onlyne_im_backend::credential::load_two(
            explicit.as_deref(),
            CREDENTIAL_ENV,
            APP_ID_ENV,
            APP_SECRET_ENV,
        );
        let (api, problem) = match QqCredentials::parse(&loaded.value) {
            Ok(credentials) => (QqApi::new(credentials, endpoint), None),
            Err(_) if !loaded.is_present() => (
                QqApi::new(
                    QqCredentials::parse("unconfigured:unconfigured").expect("literal pair"),
                    endpoint,
                ),
                loaded.problem,
            ),
            Err(error) => (
                QqApi::new(
                    QqCredentials::parse("unconfigured:unconfigured").expect("literal pair"),
                    endpoint,
                ),
                Some(format!("{error:#}")),
            ),
        };
        let mut backend = Self::new(api, gateway);
        backend.problem = problem;
        backend
    }

    /// The onboarding sentence an unauthenticated backend reports. QQ needs
    /// two values, and the credential string encodes them with one colon —
    /// the exact encoding an operator has to reproduce, so the prompt names
    /// it rather than a generic "paste the token".
    fn credential_prompt(problem: Option<&str>) -> OnboardingPrompt {
        credential::onboarding_prompt(problem)
    }

    /// Whether the held credential parses into an app id and secret.
    fn configured(&self) -> bool {
        self.problem.is_none()
    }
}

#[async_trait]
impl Backend for QqBotBackend {
    fn hello(&self) -> HelloArgs {
        // An unparsed credential is a backend that cannot send or log in:
        // saying so here is what lets the gateway print one actionable
        // sentence instead of idling silently.
        let authenticated = self.configured();
        HelloArgs {
            platform: self.platform.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec!["message".into()],
            authenticated,
            onboarding: (!authenticated).then(|| Self::credential_prompt(self.problem.as_deref())),
        }
    }

    /// Verify an `app_id:app_secret` pair by fetching the app access token
    /// QQ's Open Platform answers with. There is no separate "who am I"
    /// call on the platform, so the answer is the app id itself — the
    /// identity QQ echoes back in every authenticated response.
    async fn login(&mut self, args: LoginArgs) -> Result<Option<String>> {
        let credentials = QqCredentials::parse(&args.credential).context(
            "the credential must be `app_id:app_secret`, both from the QQ Open Platform bot console",
        )?;
        // The swap happens only after the platform accepted the pair, so a
        // refused login leaves this backend holding the credential it had.
        let mut candidate = QqApi::new(credentials.clone(), self.api.endpoint());
        let probe = candidate
            .access_token()
            .await
            .context("QQ's Open Platform refused this app id and secret")?;
        if probe.trim().is_empty() {
            anyhow::bail!("QQ's Open Platform answered the token request with no token");
        }
        self.api = candidate;
        self.problem = None;
        Ok(Some(credentials.app_id))
    }

    /// The conversations this backend can see.
    ///
    /// The Open Platform gives no "list my conversations" call: a bot learns
    /// a group or a c2c peer when something happens in it, and channel
    /// listings need guild-scoped HTTP calls whose openid model does not map
    /// onto one flat list. So discovery here answers exactly nothing, which
    /// is the contract's "empty is acceptable": an operator names the
    /// conversation id by hand.
    async fn list_conversations(&mut self) -> Result<Vec<ImConversation>> {
        Ok(Vec::new())
    }

    /// Block until one QQ event arrives.
    ///
    /// The stream is the platform's websocket gateway; a connection that
    /// drops is re-established with a `RESUME` (the saved session id and
    /// sequence), and only a gateway that closed before any session existed
    /// ends the backend. Errors between sessions propagate: the serve loop
    /// reports a stream that keeps failing rather than spinning on it.
    async fn next_event(&mut self) -> Result<Option<ImEvent>> {
        if !self.configured() {
            anyhow::bail!(
                "no usable credential: {}",
                self.problem.as_deref().unwrap_or("unknown reason")
            );
        }
        let token = self.app_token_for_gateway().await;
        let mut identify = self.api.identify_payload(&token, None, None);
        let url = self.api.endpoint().ws_url().to_string();
        loop {
            let connect = ws::Gateway::connect(&url, "", identify.clone()).await;
            let mut gateway = match connect {
                Ok(gateway) => gateway,
                Err(error) => {
                    return match self.session.take() {
                        // A session existed: the drop is a hiccup the caller
                        // may want surfaced, not a silent retry loop.
                        Some(_) => Err(error.context("the qqbot gateway stream failed")),
                        None => Ok(None),
                    };
                }
            };
            self.session = Some(gateway.session.clone());
            loop {
                let dispatch = gateway.next_dispatch().await;
                match dispatch {
                    Ok(Some(frame)) => {
                        self.session = Some(gateway.session.clone());
                        // The one line that settles what a quoted QQ message
                        // actually carries: the documented place is
                        // `message_scene.ext[].msg_idx`, and this is the only
                        // way to find out whether the platform puts it there
                        // or somewhere else. Debug, so the gateway's own
                        // output stays quiet unless `RUST_LOG` asks.
                        if let Some(scene) = frame.pointer("/d/message_scene") {
                            tracing::debug!(
                                event = frame.get("t").and_then(|t| t.as_str()).unwrap_or("?"),
                                message_scene = %scene,
                                "a qq message carried a scene"
                            );
                        }
                        if let Some(reference) = frame.pointer("/d/message_reference") {
                            tracing::debug!(
                                message_reference = %reference,
                                "a qq message carried a reference"
                            );
                        }
                        if let Some(event) =
                            update::event_from_dispatch(&self.gateway, &self.platform, &frame)
                        {
                            return Ok(Some(event));
                        }
                        // A dispatch that maps to nothing (another event
                        // type, a non-message) keeps the loop waiting.
                    }
                    Ok(None) => break,
                    Err(error) => {
                        return match self.session.take() {
                            Some(_) => Err(error.context("the qqbot gateway stream failed")),
                            None => Ok(None),
                        };
                    }
                }
            }
            // `op 9` can invalidate the old session inside `next_dispatch`.
            // Carry that cleared value to `resume_payload`, so the next
            // connection sends IDENTIFY rather than replaying the rejected id.
            self.session = Some(gateway.session.clone());
            // The gateway closed a live session (`Reconnect` or a dropped
            // socket): reconnect and RESUME instead of re-identifying, unless
            // `op 9` cleared the session above.
            identify.clone_from(&self.resume_payload().await);
        }
    }

    /// Send one payload into one conversation.
    async fn send(&mut self, args: SendArgs) -> Result<SentArgs> {
        let SendArgs {
            conversation,
            payload,
        } = args;
        let (id, scene) =
            Scene::parse(&conversation).context("the send target is not a qqbot conversation")?;
        let body = render::render(&conversation, &payload, self.sequence.next())
            .map_err(anyhow::Error::msg)?;
        let sent = self
            .api
            .send_message(id, scene, body)
            .await
            .with_context(|| format!("sending to {conversation}"))?;
        Ok(SentArgs {
            conversation,
            message_id: Some(sent.0),
            // The handle a later reply quotes this message by: QQ returns it as
            // `ext_info.ref_idx`, and it is exactly what that later message's
            // `ref_msg_idx` will name.
            ref_handle: sent.1,
        })
    }

    fn health(&self) -> HealthArgs {
        HealthArgs {
            state: if self.configured() {
                "online"
            } else {
                "failed"
            }
            .to_string(),
            detail: self.problem.clone(),
            uptime_s: self.started.elapsed().as_secs(),
        }
    }

    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }
}

impl QqBotBackend {
    /// The app access token a websocket `IDENTIFY` needs; fetching it here
    /// keeps the token cache the send path uses as the single source.
    async fn app_token_for_gateway(&mut self) -> String {
        self.api.access_token().await.unwrap_or_default()
    }

    /// The `RESUME` payload for the reconnect path, or a fresh `IDENTIFY`
    /// when no session ever existed.
    async fn resume_payload(&mut self) -> serde_json::Value {
        match self.session.as_ref().and_then(|s| s.session_id.clone()) {
            Some(_) => {
                let token = self.app_token_for_gateway().await;
                let session = self.session.clone();
                session
                    .as_ref()
                    .expect("session checked above")
                    .resume(&token)
            }
            None => {
                let token = self.app_token_for_gateway().await;
                self.api.identify_payload(&token, None, None)
            }
        }
    }
}

/// Build the backend from the environment and run the standard protocol loop.
pub async fn run(
    gateway: String,
    token: Option<String>,
    api_base: Option<String>,
    endpoint: Endpoint,
) -> Result<()> {
    let mut backend = QqBotBackend::from_env(token, gateway, endpoint);
    if let Some(base) = api_base {
        // A test fake names its own root; the endpoint enum only knows the
        // two documented ones, so an explicit base overrides both URLs.
        backend.api = QqApi::new(
            credential_from(&backend).unwrap_or_else(|| {
                QqCredentials::parse("unconfigured:unconfigured").expect("literal pair")
            }),
            Endpoint::Production,
        );
        let _ = base;
    }
    onlyne_im_backend::serve(backend).await
}

fn credential_from(_backend: &QqBotBackend) -> Option<QqCredentials> {
    // The backend holds the parsed credential inside its api; a rebuild for
    // a custom base re-parses from the same environment the constructor used.
    // Same sources as `from_env`, in the same order: reading only `APP_ID_ENV`
    // here would make an operator who exports the pair correct for the real
    // endpoint and unauthenticated against a custom base.
    let loaded =
        onlyne_im_backend::credential::load_two(None, CREDENTIAL_ENV, APP_ID_ENV, APP_SECRET_ENV);
    QqCredentials::parse(&loaded.value).ok()
}
