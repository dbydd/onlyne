//! Feishu (Lark) platform backend for the Onlyne IM gateway.
//!
//! Implements the standard backend protocol (`onlyne-im-backend`) and speaks
//! Feishu's Open API through the `openlark` SDK. The two translation halves
//! live in [`update`] (a platform event becomes a standard event) and
//! [`render`] (a standard payload becomes a send-message request body);
//! [`api`] holds the SDK requests (chat list, send, reply, token check) and
//! [`auth`] the credential encoding. This module is the `Backend` that joins
//! them.
//!
//! Inbound runs over the `openlark` websocket client, exactly as in v1
//! (`frozen/gateway-v1`'s `spawn_websocket`): Feishu has no polling API, and
//! the SDK owns the connection and its token lifecycle. The websocket is
//! spawned at construction, re-connects inside the SDK, and is reopened by
//! [`spawn_websocket`] whenever the SDK's session returns; this backend only
//! drains the payload channel.
//!
//! The app must subscribe to three events in the Feishu developer console:
//! `im.message.receive_v1`, `im.chat.member.bot.added_v1` (so a group the bot
//! has just joined is adopted immediately), and `card.action.trigger` (button
//! presses on interactive cards). The bot must also be granted
//! `im:message`, `im:chat:readonly` and `im:resource`.
//!
//! A card callback is delivered as an inbound action, but it is **not**
//! answered with an in-place card update: the websocket ACK must leave within
//! Feishu's three-second deadline, and the agent's answer is not known then.
//! The callback is acknowledged with a static info toast
//! ([`CARD_ACK_TOAST`]) while the action itself reaches the gateway through
//! the payload channel, so the trigger can answer with a normal message that
//! the action event's `reply` coordinate threads under the card.

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
use std::time::{Duration, Instant};

pub use auth::{APP_ID_ENV, APP_SECRET_ENV, DEFAULT_DOMAIN, DOMAIN_ENV};

/// What a card press is told immediately: the callback must answer within
/// three seconds, before the agent has decided anything. The answer itself
/// arrives as the follow-up message the trigger sends.
pub const CARD_ACK_TOAST: &str = "Received; the agent is working on it";

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
///
/// `open` runs one session and returns when it ends, so the pump reopens it
/// for as long as the receiver is alive. A session that lasted at least
/// [`NORMAL_SESSION`] was working, and its return is treated as a restart
/// rather than a failure: it retries at once and the failure count resets. A
/// session that returned sooner failed, so each consecutive failure doubles
/// the delay ([`reconnect_delay`]) instead of hammering a platform that is
/// down.
fn spawn_websocket(
    creds: auth::FeishuCredentials,
) -> tokio::sync::mpsc::UnboundedReceiver<Vec<u8>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        let base_url = creds.base_url();
        let mut consecutive_failures: u32 = 0;
        loop {
            // The gateway dropped the receiver: no reason to hold a connection.
            if tx.is_closed() {
                break;
            }
            let config = open_lark::Config::builder()
                .app_id(creds.app_id.as_str())
                .app_secret(creds.app_secret.as_str())
                .base_url(base_url.as_str())
                .build();
            let Ok(handler) = dispatcher(tx.clone()) else {
                tracing::warn!(
                    "feishu sdk rejected the card callback handler; the websocket never starts"
                );
                return;
            };
            let started = Instant::now();
            let result =
                open_lark::ws_client::LarkWsClient::open(std::sync::Arc::new(config), handler)
                    .await;
            if tx.is_closed() {
                break;
            }
            if started.elapsed() >= NORMAL_SESSION {
                consecutive_failures = 0;
                tracing::debug!(
                    error = ?result.err(),
                    "feishu websocket session closed; reconnecting"
                );
                tokio::time::sleep(reconnect_delay(0)).await;
            } else {
                consecutive_failures = consecutive_failures.saturating_add(1);
                tracing::warn!(
                    error = ?result.err(),
                    retry_in_ms = reconnect_delay(consecutive_failures).as_millis() as u64,
                    "feishu websocket session closed immediately; reconnecting"
                );
                tokio::time::sleep(reconnect_delay(consecutive_failures)).await;
            }
        }
    });
    rx
}

/// How long a websocket session must have lasted to count as working rather
/// than failing: a session that had been up this long is reopened without
/// backoff.
const NORMAL_SESSION: Duration = Duration::from_secs(30);

/// The delay before the next connection attempt: a fifth of a second after a
/// normal close, then doubling per consecutive fast failure up to half a
/// minute, so a platform that stays down is retried steadily instead of in a
/// tight loop.
fn reconnect_delay(consecutive_failures: u32) -> Duration {
    match consecutive_failures {
        0 => Duration::from_millis(200),
        1 => Duration::from_secs(1),
        2 => Duration::from_secs(2),
        3 => Duration::from_secs(4),
        4 => Duration::from_secs(8),
        5 => Duration::from_secs(16),
        _ => Duration::from_secs(30),
    }
}

/// The SDK dispatcher: every payload goes to the channel the backend drains,
/// and a card press additionally gets an immediate toast in the ACK.
///
/// `openlark` 0.21's `dispatch_with_response` forwards to `payload_sender`
/// before it invokes a callback handler, so registering the card callback does
/// not stop the action reaching the gateway — it only adds the response body.
fn dispatcher(
    payload_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
) -> Result<open_lark::ws_client::EventDispatcherHandler, String> {
    Ok(open_lark::ws_client::EventDispatcherHandler::builder()
        .payload_sender(payload_tx)
        .register_card_action_trigger(CardAcknowledged)?
        .build())
}

/// Answers a card callback with a toast while the action is handled.
struct CardAcknowledged;

impl open_lark::ws_client::CardActionTriggerHandler for CardAcknowledged {
    fn handle(
        &self,
        _event: open_lark::ws_client::CardActionTrigger,
    ) -> Result<
        Option<open_lark::ws_client::CardActionTriggerResponse>,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        Ok(Some(open_lark::ws_client::CardActionTriggerResponse {
            toast: Some(open_lark::ws_client::CardToast {
                r#type: "info".to_string(),
                title: None,
                content: Some(CARD_ACK_TOAST.to_string()),
            }),
            // An in-place card update would have to know the agent's answer
            // now; it does not exist yet, so the follow-up message carries it.
            card: None,
        }))
    }
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
            // `action` is real now: a button press on an interactive card
            // arrives as `card.action.trigger` and is reported as an `action`
            // event.
            capabilities: vec!["message".into(), "action".into()],
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

    /// The chats this bot is in, from Feishu's own listing
    /// (`GET /open-apis/im/v1/chats`, walked page by page).
    ///
    /// This is the reach answer rather than a convenience: a chat the bot was
    /// just added to is here even before an event arrives in it, and an
    /// external group the bot was invited into is a row like any other.
    async fn list_conversations(&mut self) -> Result<Vec<ImConversation>> {
        self.api.list_chats().await
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
            if let Some(mut event) =
                update::event_from_payload(&self.gateway, &self.platform, &value)
            {
                self.materialize_inbound_attachments(&mut event).await;
                return Ok(Some(event));
            }
            // An event this backend does not translate is the ordinary case
            // (membership changes, edits, receipts); keep waiting.
        }
    }

    async fn send(&mut self, args: SendArgs) -> Result<SentArgs> {
        let mut message_id = None;
        let reply_to = args.payload.reply_to.as_deref();
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
        for attachment in &args.payload.attachments {
            let body = if attachment.kind == "image" {
                let image_key = if let Some(key) = attachment.path.strip_prefix("feishu:image_key:")
                {
                    key.to_string()
                } else {
                    self.api.upload_image(&attachment.path).await?
                };
                render::image_request(&args.conversation, &image_key, reply_to)
            } else {
                let file_key = if let Some(key) = attachment.path.strip_prefix("feishu:file_key:") {
                    key.to_string()
                } else {
                    self.api.upload_file(&attachment.path).await?
                };
                render::file_request(&args.conversation, &file_key, reply_to)
            };
            message_id = Some(self.api.post_message(&args.conversation, body).await?);
        }
        Ok(SentArgs {
            conversation: args.conversation,
            message_id,
            // One id per message on Feishu: a quote is a new message, not a
            // handle the platform hands back.
            ref_handle: None,
            error: None,
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

impl FeishuBackend {
    /// Download inbound `feishu:image_key:<key>` and `feishu:file_key:<key>`
    /// resources to disk so the agent and trigger script receive a readable
    /// local path alongside the original key in `alt`.
    async fn materialize_inbound_attachments(&self, event: &mut ImEvent) {
        if event.attachments.is_empty() || self.api.credentials().app_id.is_empty() {
            return;
        }
        let media_dir = std::env::var("FEISHU_MEDIA_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("onlyne-feishu-media"));
        for att in &mut event.attachments {
            let (res_type, key) = if let Some(k) = att.path.strip_prefix("feishu:image_key:") {
                ("image", k.to_string())
            } else if let Some(k) = att.path.strip_prefix("feishu:file_key:") {
                ("file", k.to_string())
            } else {
                continue;
            };
            match self.api.download_resource(&event.id, &key, res_type).await {
                Ok(bytes) => {
                    if std::fs::create_dir_all(&media_dir).is_ok() {
                        let safe_key: String = key
                            .chars()
                            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                            .collect();
                        let ext = att
                            .alt
                            .as_deref()
                            .and_then(|n| std::path::Path::new(n).extension())
                            .and_then(|e| e.to_str())
                            .unwrap_or(if res_type == "image" { "png" } else { "bin" });
                        let file_path = media_dir.join(format!("{}-{safe_key}.{ext}", event.id));
                        if std::fs::write(&file_path, &bytes).is_ok() {
                            let prev_alt = att.alt.take();
                            att.alt = Some(match prev_alt {
                                Some(name) => format!("{name} (feishu:{res_type}_key:{key})"),
                                None => format!("feishu:{res_type}_key:{key}"),
                            });
                            att.path = file_path.to_string_lossy().into_owned();
                        }
                    }
                }
                Err(error) => {
                    tracing::debug!(
                        message_id = %event.id,
                        key = %key,
                        error = %format!("{error:#}"),
                        "feishu inbound resource download skipped; keeping handle in path"
                    );
                }
            }
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
    use serde_json::Value;

    const CARD_CALLBACK: &[u8] = br#"{"schema":"2.0","header":{"event_id":"evt-card","event_type":"card.action.trigger"},"event":{"operator":{"open_id":"ou_presser"},"action":{"tag":"button","value":{"action":"approve"}},"context":{"open_chat_id":"oc_group","open_message_id":"om_card"}}}"#;

    /// A card press must both answer within the deadline and reach the
    /// gateway: `dispatch_with_response` forwards to the channel before it
    /// invokes the callback, and the callback's value becomes the ACK body.
    #[test]
    fn a_card_callback_answers_a_toast_and_still_reaches_the_channel() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handler = dispatcher(tx).expect("the dispatcher registers the callback");
        let ack = handler
            .dispatch_with_response(CARD_CALLBACK)
            .expect("the callback is dispatched")
            .expect("a card callback carries a response body");
        let response: Value = serde_json::from_slice(&ack).expect("the ACK body is JSON");
        assert_eq!(response["toast"]["type"], "info");
        assert_eq!(response["toast"]["content"], CARD_ACK_TOAST);
        assert!(
            response.get("card").is_none(),
            "no in-place card update: the answer is not known yet"
        );
        assert_eq!(
            rx.try_recv().expect("the action reaches the gateway"),
            CARD_CALLBACK
        );
    }

    /// Any other event still reaches the channel, with no callback body.
    #[test]
    fn an_ordinary_event_has_no_callback_body_but_reaches_the_channel() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handler = dispatcher(tx).expect("the dispatcher builds");
        let payload = br#"{"header":{"event_type":"im.message.receive_v1"},"event":{}}"#;
        assert!(
            handler
                .dispatch_with_response(payload)
                .expect("dispatched")
                .is_none()
        );
        assert_eq!(
            rx.try_recv().expect("the event reaches the gateway"),
            payload
        );
    }

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

    /// The backoff ladder: a fifth of a second after a normal close, doubling
    /// per consecutive fast failure to a half-minute cap.
    #[test]
    fn the_reconnect_delay_doubles_from_a_fifth_of_a_second_to_half_a_minute() {
        assert_eq!(reconnect_delay(0), Duration::from_millis(200));
        assert_eq!(reconnect_delay(1), Duration::from_secs(1));
        assert_eq!(reconnect_delay(2), Duration::from_secs(2));
        assert_eq!(reconnect_delay(3), Duration::from_secs(4));
        assert_eq!(reconnect_delay(4), Duration::from_secs(8));
        assert_eq!(reconnect_delay(5), Duration::from_secs(16));
        // Past the cap it holds, so a platform that stays down is not hammered.
        for failures in [6, 7, 30, u32::MAX] {
            assert_eq!(reconnect_delay(failures), Duration::from_secs(30));
        }
    }

    #[tokio::test]
    async fn a_parseable_credential_reports_authenticated_and_names_the_env_vars() {
        let (backend, problem) = FeishuBackend::from_env(Some("cli_x:secret".into()), "gw");
        assert!(problem.is_none());
        let hello = backend.hello();
        assert!(hello.authenticated);
        assert_eq!(hello.platform, "feishu");
        assert!(hello.onboarding.is_none());
        assert_eq!(
            hello.capabilities,
            vec!["message".to_string(), "action".to_string()]
        );
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
