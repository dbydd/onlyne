//! QQ Open Platform backend for the Onlyne IM gateway.
//!
//! Implements the standard backend protocol (`onlyne-im-backend`) and speaks
//! to QQ's Open Platform: an app access token over HTTP, message sends per
//! scene, and the websocket event gateway. The translation halves live in
//! [`update`] (a platform dispatch becomes a standard event), [`render`]
//! (a standard payload becomes a QQ message body), and [`ws`] (the gateway
//! protocol); [`api`] is the HTTP client and [`credential`] the credential
//! encoding. This module is the [`Backend`] that joins them.
//!
//! # What QQ will not do
//!
//! **A bot cannot react to a message in a group or a c2c chat.** QQ's reaction
//! API is `PUT /channels/{channel_id}/messages/{message_id}/reactions/{type}/{id}`
//! and its own page says so in one line: *「目前表情表态仅支持在频道内使用」*
//! (`wiki/develop/api-v2/server-inter/message/trans/emoji.html`, read
//! 2026-10-15). There is no group or c2c form of it, and no group event that
//! reports a user's reaction either — the `MESSAGE_REACTION_ADD` event is a
//! guild event. The emoji vocabulary exists for the platform as a whole
//! (`{id: string, type: uint32}`, type 1 = system, 2 = emoji), which is what
//! makes the channel-only rule easy to misread as a general one.
//!
//! So a delivery asking this backend to react has nothing to call. A group
//! reaction is therefore a platform fact and not a missing code path: an
//! operator who wants one gets it on a platform that carries reactions
//! (Feishu does, on the message itself), or they read it as the platform's
//! limitation rather than this backend's.

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
use ws::{Session, reconnect_delay};

/// The conventional credential variables, for an operator who exports them
/// instead of running `onlyne-im login`.
pub const APP_ID_ENV: &str = credential::APP_ID_ENV;
pub const APP_SECRET_ENV: &str = credential::APP_SECRET_ENV;
/// The single combined variable the credential loader reads first.
pub const CREDENTIAL_ENV: &str = "QQBOT_CREDENTIAL";
/// An override for the event-gateway address, for a test that needs a
/// connection to *fail*.
///
/// QQ's gateway accepts anything a valid credential asks of it, so the
/// reconnect path cannot be reached by pointing the backend at the real
/// platform: a refused connection needs an address that refuses. Set only for
/// that; unset, the backend dials the endpoint's own URL.
pub const WS_URL_ENV: &str = "ONLYNE_QQBOT_WS_URL";

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
    /// Wait for one platform event, reconnecting under backoff.
    ///
    /// A websocket that dropped, a session QQ invalidated (`op 9, d=false`),
    /// an `IDENTIFY` the gateway refused while it still held the old
    /// connection: all of these are the same class of fact — the platform is
    /// not talking *now* — and the answer to all of them is to reconnect. This
    /// used to return on the first failure, which ended the backend process
    /// (`serve_backend` reads a stream that ends as a backend that is done), so
    /// one invalidated QQ session left the gateway running for the rest of its
    /// life with every QQ conversation silently unserved. Measured live
    /// 2026-10-15: `IDENTIFY` answered `op 9, d=false` once and the backend was
    /// gone until the gateway was restarted.
    ///
    /// The delay doubles and is jittered from the clock, so a QQ gateway that
    /// drops every bot at once does not get them all back in the same second.
    /// Nothing here gives up: the platform is the one that decides when it will
    /// answer again, and every attempt says so in the log.
    async fn next_event(&mut self) -> Result<Option<ImEvent>> {
        if !self.configured() {
            anyhow::bail!(
                "no usable credential: {}",
                self.problem.as_deref().unwrap_or("unknown reason")
            );
        }
        let url = std::env::var(WS_URL_ENV)
            .ok()
            .filter(|url| !url.trim().is_empty())
            .unwrap_or_else(|| self.api.endpoint().ws_url().to_string());
        let mut attempt = 0u32;
        loop {
            let identify = match self.resume_payload().await {
                Ok(identify) => identify,
                Err(error) => {
                    tracing::warn!(
                        error = %format!("{error:#}"),
                        retry_in_ms = reconnect_delay(attempt).as_millis() as u64,
                        "no credential for the qqbot event gateway; retrying"
                    );
                    tokio::time::sleep(reconnect_delay(attempt)).await;
                    attempt += 1;
                    continue;
                }
            };
            let prior = self.session.clone();
            let connect = ws::Gateway::connect(&url, "", identify, prior)
                .await
                .context("the qqbot gateway stream failed");
            let mut gateway = match connect {
                Ok(gateway) => gateway,
                Err(error) => {
                    if error.to_string().contains("resumable=false") {
                        self.session = None;
                    }
                    tracing::warn!(
                        error = %format!("{error:#}"),
                        retry_in_ms = reconnect_delay(attempt).as_millis() as u64,
                        "the qqbot event gateway refused a connection; retrying"
                    );
                    tokio::time::sleep(reconnect_delay(attempt)).await;
                    attempt += 1;
                    continue;
                }
            };
            // The connection is up: the next failure is a fresh one and earns
            // the first rung of the ladder rather than the one this loop was
            // already sitting on.
            attempt = 0;
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
                    // `Reconnect` (the platform is retiring this connection) or
                    // an invalidated session: the inner loop ends and the outer
                    // one dials again, with `RESUME` while the session stands
                    // and a fresh `IDENTIFY` once it does not.
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(
                            error = %format!("{error:#}"),
                            "the qqbot event stream dropped; reconnecting"
                        );
                        break;
                    }
                }
            }
            // `op 9` can invalidate the session inside `next_dispatch`; the
            // clone carries that cleared value, so the next connection sends
            // IDENTIFY rather than replaying the rejected id.
            self.session = Some(gateway.session.clone());
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
            error: None,
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
    ///
    /// A failure is a failure. Reading it as an empty token — the shape this
    /// had — put `"QQBot "` on the wire, which the platform answers with
    /// `op 9`, so a token endpoint that was down looked like a refused
    /// session and sent the reconnect path hunting the wrong problem.
    async fn app_token_for_gateway(&mut self) -> Result<String> {
        let token = self.api.access_token().await.context(
            "QQ's Open Platform would not issue an app access token for this credential",
        )?;
        if token.trim().is_empty() {
            anyhow::bail!("QQ's Open Platform answered the token request with no token");
        }
        Ok(token)
    }

    /// The `RESUME` payload for the reconnect path, or a fresh `IDENTIFY`
    /// when no session ever existed.
    async fn resume_payload(&mut self) -> Result<serde_json::Value> {
        match self.session.as_ref().and_then(|s| s.session_id.clone()) {
            Some(_) => {
                let token = self.app_token_for_gateway().await?;
                let session = self.session.clone();
                Ok(session
                    .as_ref()
                    .expect("session checked above")
                    .resume(&token))
            }
            None => {
                let token = self.app_token_for_gateway().await?;
                Ok(self.api.identify_payload(&token, None, None))
            }
        }
    }
}

/// Build the backend from the environment and run the standard protocol loop.
///
/// The event-gateway address is the one thing an override is offered for
/// ([`WS_URL_ENV`]), because a refused connection is what the reconnect path
/// is tested against. The **API** root has no override: pointing it somewhere
/// other than QQ — which the endpoint enum's `--sandbox` bracket already
/// spans, with no gap in between — is what an `--api_base` flag would be for,
/// and QQ has no second API root to use it with. The Telegram backend carries
/// one because a local fake answers there; nothing does here.
pub async fn run(gateway: String, token: Option<String>, endpoint: Endpoint) -> Result<()> {
    let backend = QqBotBackend::from_env(token, gateway, endpoint);
    onlyne_im_backend::serve(backend).await
}
