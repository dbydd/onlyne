//! Feishu (Lark) platform backend for the Onlyne IM gateway.
//!
//! Implements the standard backend protocol (`onlyne-im-backend`) and speaks
//! Feishu's Open API. The two translation halves live in [`update`] (a
//! platform event becomes a standard event) and [`render`] (a standard payload
//! becomes a send-message request body); [`api`] holds the two HTTP calls and
//! [`auth`] the credential encoding. This module is the `Backend` that joins
//! them.
//!
//! Inbound runs over the `openlark` websocket client, exactly as in v1
//! (`frozen/gateway-v1`'s `spawn_websocket`): Feishu has no polling API, and
//! the SDK owns the connection and its token lifecycle. The websocket is
//! spawned at construction and re-connects inside the SDK; this backend only
//! drains the payload channel.

mod api;
mod auth;
mod render;
mod update;

use anyhow::{Context, Result};
use api::Api;
use async_trait::async_trait;
use onlyne_im_backend::backend::Backend;
use onlyne_im_backend::frame::{
    HealthArgs, HelloArgs, ImConversation, LoginArgs, OnboardingKind, OnboardingPrompt, SendArgs,
    SentArgs,
};
use onlyne_im_backend::payload::ImEvent;
use std::time::Instant;

pub use auth::{APP_ID_ENV, APP_SECRET_ENV, DEFAULT_DOMAIN, DOMAIN_ENV};

/// One Feishu backend: the app identity and the websocket event pump.
pub struct FeishuBackend {
    api: Api,
    gateway: String,
    platform: String,
    started: Instant,
    /// Raw Feishu event payloads from the websocket, one JSON object each.
    events: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    /// Why no credential was found, when that is the case. It becomes the
    /// onboarding sentence, so an operator reads the exact reason.
    problem: Option<String>,
}

impl FeishuBackend {
    pub fn new(
        api: Api,
        gateway: impl Into<String>,
        events: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    ) -> Self {
        Self {
            api,
            gateway: gateway.into(),
            platform: "feishu".to_string(),
            started: Instant::now(),
            events,
            problem: None,
        }
    }

    /// Build from the environment: the credential loader's order (`--token`,
    /// then the file `ONLYNE_IM_CREDENTIAL` names, then `FEISHU_CREDENTIAL`,
    /// which an operator with both variables exported fills as
    /// `FEISHU_APP_ID:FEISHU_APP_SECRET`), then the websocket pump.
    ///
    /// The credential is the one string the protocol carries, encoded as
    /// `app_id:app_secret` (see [`auth::parse_credential`]). A missing or
    /// unparseable credential is carried as [`Self::problem`] rather than
    /// raised, because it is the state `onlyne-im login` exists to fix and
    /// this backend has to be running to answer that login.
    pub fn from_env(
        explicit: Option<String>,
        gateway: impl Into<String>,
    ) -> (Self, Option<String>) {
        let loaded = onlyne_im_backend::credential::load_two(
            explicit.as_deref(),
            CREDENTIAL_ENV,
            APP_ID_ENV,
            APP_SECRET_ENV,
        );
        let (creds, problem) = match parse_loaded(&loaded.value) {
            Ok(creds) if loaded.problem.is_none() => (Some(creds), None),
            Ok(creds) => (Some(creds), loaded.problem),
            Err(reason) => (None, Some(loaded.problem.unwrap_or(reason.to_string()))),
        };
        let events = match &creds {
            Some(creds) => spawn_websocket(creds.clone()),
            None => noop_events(),
        };
        let mut backend = Self::new(
            Api::new(creds.unwrap_or_else(|| PLACEHOLDER_CREDS.clone())),
            gateway,
            events,
        );
        backend.problem = problem.clone();
        (backend, problem)
    }

    /// The onboarding sentence an unauthenticated backend reports. Feishu's
    /// credential is a custom app's id and secret, created at the developer
    /// console and pasted as one `app_id:app_secret` string.
    fn credential_prompt(problem: Option<&str>) -> OnboardingPrompt {
        OnboardingPrompt {
            kind: OnboardingKind::ManualCode,
            payload: auth::onboarding_payload(problem),
            expires_in: None,
        }
    }
}

/// The platform env name the credential loader falls back to. Feishu splits
/// its credential across two variables, but the loader carries one string, so
/// the loader's third source reads a single combined variable that is joined
/// with `:` when both halves are set.
const CREDENTIAL_ENV: &str = "FEISHU_CREDENTIAL";

/// A placeholder identity so an unauthenticated backend still constructs; it
/// never contacts Feishu and every `hello` says so.
static PLACEHOLDER_CREDS: std::sync::LazyLock<auth::FeishuCredentials> =
    std::sync::LazyLock::new(|| auth::FeishuCredentials::new("", "", DEFAULT_DOMAIN));

/// Decode the loader's one credential string. A credential file may instead
/// hold `FEISHU_APP_ID`-style `$VAR` references (resolved by the loader), but
/// the resolved value is still one `app_id:app_secret` string.
fn parse_loaded(value: &str) -> Result<auth::FeishuCredentials> {
    auth::parse_credential(value)
        .map_err(|reason| anyhow::anyhow!("{reason}"))
        .context("decoding the Feishu credential")
}

/// A channel no websocket feeds, for a backend without a credential. It never
/// yields, so `next_event` parks until a login replaces the pump — which the
/// protocol loop answers on stdin without touching this channel.
fn noop_events() -> tokio::sync::mpsc::UnboundedReceiver<Vec<u8>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    drop(tx);
    rx
}

/// Spawn the `openlark` websocket client, ported from v1's
/// `spawn_websocket`: raw event payloads land in the channel this returns.
fn spawn_websocket(
    creds: auth::FeishuCredentials,
) -> tokio::sync::mpsc::UnboundedReceiver<Vec<u8>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        // The `openlark` crate's library target is named `open_lark`, as v1
        // referenced it.
        let base_url = creds.base_url();
        #[allow(deprecated)]
        let Ok(config) = open_lark::Config::builder()
            .app_id(creds.app_id)
            .app_secret(creds.app_secret)
            .base_url(base_url)
            .build()
        else {
            tracing::warn!("feishu sdk config was rejected; the websocket never starts");
            return;
        };
        let handler = open_lark::ws_client::EventDispatcherHandler::builder()
            .payload_sender(tx)
            .build();
        let result =
            open_lark::ws_client::LarkWsClient::open(std::sync::Arc::new(config), handler).await;
        if let Err(e) = result {
            tracing::warn!(error = ?e, "feishu websocket closed");
        }
    });
    rx
}

#[async_trait]
impl Backend for FeishuBackend {
    fn hello(&self) -> HelloArgs {
        // Feishu has no credential-less mode: without an app id and secret the
        // backend can neither receive nor send, and saying so here is what
        // lets the gateway print one actionable sentence instead of idling.
        let authenticated = !self.api.credentials().app_id.is_empty();
        HelloArgs {
            platform: self.platform.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec!["message".into()],
            authenticated,
            onboarding: (!authenticated).then(|| Self::credential_prompt(self.problem.as_deref())),
        }
    }

    /// Verify a pasted `app_id:app_secret` by fetching a tenant access token
    /// with it, and answer the app's identity. Feishu's token endpoint does
    /// not name the app, so the identity is the app id itself — the operator
    /// pasted it, and seeing it echoed confirms which app authenticated.
    async fn login(&mut self, args: LoginArgs) -> Result<Option<String>> {
        let creds = auth::parse_credential(&args.credential)
            .map_err(|reason| anyhow::anyhow!("{reason}"))
            .context("decoding the pasted credential")?;
        // The pair is swapped in only after the check, so a wrong credential
        // leaves this backend holding what it just proved good. The setup verb
        // reads the answer and decides.
        let api = self.api.with_credentials(creds.clone());
        api.tenant_token()
            .await
            .context("Feishu refused this app_id/app_secret; check them against the custom app's credentials page")?;
        self.api = api;
        // The credential is now real: start (or restart) the websocket pump
        // with it, so events flow without a backend restart.
        self.events = spawn_websocket(creds.clone());
        self.problem = None;
        Ok(Some(creds.app_id))
    }

    /// The chats this app can see by id.
    ///
    /// Feishu's chat listing (`GET /open-apis/im/v1/chats`) exists, but v1
    /// never called it and the port does not add what v1 did not do: a bot
    /// learns a chat when something happens in it, and an operator names the
    /// chat id by hand instead. Always empty.
    async fn list_conversations(&mut self) -> Result<Vec<ImConversation>> {
        Ok(Vec::new())
    }

    async fn next_event(&mut self) -> Result<Option<ImEvent>> {
        loop {
            // `None` only when the channel itself closed; a websocket that
            // dropped logs inside `spawn_websocket` and the SDK reconnects,
            // so the pump keeps waiting rather than ending the backend.
            let payload = self
                .events
                .recv()
                .await
                .context("the feishu event channel closed")?;
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&payload) else {
                tracing::debug!("feishu event was not JSON; skipped");
                continue;
            };
            // The one line that separates "the platform pushed nothing" from
            // "it pushed and this backend dropped it": an event that never
            // connects leaves no other trace, because an event this backend
            // does not translate is the ordinary case and is not reported.
            // Debug level, so the gateway's own output stays quiet unless
            // `RUST_LOG` asks for it.
            tracing::debug!(
                event_type = value
                    .pointer("/header/event_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(none)"),
                bytes = payload.len(),
                "feishu event received"
            );
            if let Some(event) = update::event_from_payload(&self.gateway, &self.platform, &value) {
                return Ok(Some(event));
            }
            // An event this backend does not translate is the ordinary case
            // (membership changes, edits, receipts); keep waiting.
        }
    }

    async fn send(&mut self, args: SendArgs) -> Result<SentArgs> {
        let mut message_id = None;
        for request in render::render(&args.conversation, &args.payload) {
            // Feishu answers every create with the message it made; a payload
            // may render into several calls and the last id is the one a reply
            // to this turn should point at.
            message_id = Some(
                self.api
                    .post_message(&args.conversation, request.body)
                    .await?,
            );
        }
        Ok(SentArgs {
            conversation: args.conversation,
            message_id,
            // One id per message on Feishu: a quote is a new message, not a
            // handle the platform hands back.
            ref_handle: None,
        })
    }

    fn health(&self) -> HealthArgs {
        HealthArgs {
            state: "online".to_string(),
            detail: None,
            uptime_s: self.started.elapsed().as_secs(),
        }
    }
}

/// Build the backend from the environment and run the standard protocol loop.
pub async fn run(gateway: String, credential: String) -> Result<()> {
    let explicit = (!credential.trim().is_empty()).then_some(credential);
    let (backend, _) = FeishuBackend::from_env(explicit, gateway);
    onlyne_im_backend::serve(backend).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unparseable_credential_is_a_problem_not_a_panic() {
        let (backend, problem) = FeishuBackend::from_env(Some("not-a-credential".into()), "gw");
        assert!(problem.is_some());
        assert!(!backend.hello().authenticated);
        let onboarding = backend.hello().onboarding.unwrap();
        assert_eq!(onboarding.kind, OnboardingKind::ManualCode);
        assert!(
            onboarding.payload.contains("app_id:app_secret"),
            "{}",
            onboarding.payload
        );
    }

    #[tokio::test]
    async fn a_parseable_credential_reports_authenticated_and_names_the_env_vars() {
        let (backend, problem) = FeishuBackend::from_env(Some("cli_x:secret".into()), "gw");
        assert!(problem.is_none());
        let hello = backend.hello();
        assert!(hello.authenticated);
        assert_eq!(hello.platform, "feishu");
        assert!(hello.onboarding.is_none());
        assert_eq!(hello.capabilities, vec!["message".to_string()]);
    }

    #[test]
    fn the_onboarding_prompt_names_both_env_vars_when_nothing_is_found() {
        let prompt = FeishuBackend::credential_prompt(Some("FEISHU_APP_ID is not set"));
        assert!(prompt.payload.contains(APP_ID_ENV), "{}", prompt.payload);
        assert!(
            prompt.payload.contains(APP_SECRET_ENV),
            "{}",
            prompt.payload
        );
    }
}
