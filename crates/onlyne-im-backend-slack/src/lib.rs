//! Slack platform backend for the Onlyne IM gateway.
//!
//! Implements the standard backend protocol (`onlyne-im-backend`) on Slack
//! Morphism's Rust SDK, which supplies both transports: its Socket Mode
//! listener for inbound events, and its Web API client for sends. The
//! translation halves live in [`update`] (an SDK event becomes a standard
//! event), [`render`] (a standard payload becomes calls), and [`wire`] (the
//! SDK connector, addressed with this backend's API root); [`api`] wraps the
//! client and [`ws`] owns the listener. This module is the [`Backend`] that
//! joins them.
//!
//! # Two tokens
//!
//! Socket Mode needs an **app-level token** (`xapp-…`, from the app's Basic
//! Information page) to call `apps.connections.open`; every send needs a **bot
//! token** (`xoxb-…`, from OAuth & Permissions). The backend protocol carries
//! one credential string, so the two share it as `xapp-…:xoxb-…` — first
//! colon separates, the same encoding the QQ and Feishu backends use.

mod api;
mod render;
mod update;
mod wire;
mod ws;

use anyhow::{Context, Result, bail};
use api::SlackApi;
use async_trait::async_trait;
use onlyne_im_backend::backend::Backend;
use onlyne_im_backend::frame::{
    HealthArgs, HelloArgs, ImConversation, LoginArgs, OnboardingKind, OnboardingPrompt, SendArgs,
    SentArgs,
};
use onlyne_im_backend::payload::ImEvent;
use slack_morphism::prelude::ValueStruct;
use std::time::Instant;

/// The combined credential variable, holding `xapp-…:xoxb-…`.
pub const CREDENTIAL_ENV: &str = "SLACK_CREDENTIAL";
/// The app-level token's conventional variable.
pub const APP_TOKEN_ENV: &str = "SLACK_APP_TOKEN";
/// The bot token's conventional variable.
pub const BOT_TOKEN_ENV: &str = "SLACK_BOT_TOKEN";
/// An override for the Socket Mode websocket address, for a test that needs a
/// connection to reach, or fail against, a local server.
///
/// The SDK's listener dials whatever `apps.connections.open` answers and takes
/// no address from its caller, so this variable cannot move the socket on its
/// own. A test points [`API_BASE_ENV`] at a fake that answers
/// `apps.connections.open` with that variable's URL instead; the listener then
/// reaches the local socket through the normal path, mint and dial included.
pub const WS_URL_ENV: &str = "ONLYNE_SLACK_WS_URL";
/// The Web API root override, so a local fake can stand in for Slack in an
/// acceptance run.
pub const API_BASE_ENV: &str = "ONLYNE_SLACK_API_BASE";

/// The log filter this binary installs when `RUST_LOG` is unset.
///
/// Slack Morphism's socket client logs a failed dial at `ERROR` and puts the
/// URL it dialled in the message. That URL is a minted connection ticket —
/// `?ticket=…` is a bearer credential for the socket — so letting the line
/// through at the default level would write a live ticket into every
/// operator's stderr. The module is therefore off by default; a caller who
/// sets `RUST_LOG` explicitly opts back in and gets the line, ticket and all.
pub const DEFAULT_LOG_FILTER: &str =
    "info,slack_morphism::hyper_tokio::socket_mode::tungstenite_wss_client=off";

/// One Slack credential: the app-level token and the bot token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackCredentials {
    pub app_token: String,
    pub bot_token: String,
}

impl SlackCredentials {
    /// Parse the `xapp-…:xoxb-…` credential string.
    ///
    /// Neither Slack token contains a colon, so the first one is the
    /// separator. A missing separator is the shape an operator pastes wrong,
    /// so the error names the encoding instead of reporting a bad credential.
    pub fn parse(value: &str) -> Result<Self> {
        let (app, bot) = value.split_once(':').with_context(|| {
            "the credential must be `xapp-…:xoxb-…`: the app-level token, a colon, \
             then the bot token"
        })?;
        let app_token = app.trim();
        let bot_token = bot.trim();
        if app_token.is_empty() {
            bail!("the app-level token (`xapp-…`) is empty");
        }
        if bot_token.is_empty() {
            bail!("the bot token (`xoxb-…`) is empty");
        }
        Ok(Self {
            app_token: app_token.to_string(),
            bot_token: bot_token.to_string(),
        })
    }

    /// The encoded form, what a credential file holds.
    pub fn encode(&self) -> String {
        format!("{}:{}", self.app_token, self.bot_token)
    }
}

/// One Slack backend: the SDK client, the inbound stream, and the state the
/// protocol loop reports.
pub struct SlackBackend {
    api: SlackApi,
    /// The Web API root this backend answers on, when it is not Slack's own.
    base: Option<String>,
    gateway: String,
    platform: String,
    started: Instant,
    /// The Socket Mode stream, once a credential has been accepted.
    ///
    /// It lives on `self` rather than inside [`Self::next_event`] because the
    /// serve loop polls `next_event` inside a `select!`: a core frame winning
    /// that race drops the event future, and a connection owned by that future
    /// would be reopened on every send. The SDK's reader tasks keep running
    /// across those cancellations, and the event stays queued.
    inbound: Option<ws::Inbound>,
    /// This bot's own user id, from `auth.test`: its echoed posts are not
    /// events to answer.
    self_id: Option<String>,
    /// Why no usable credential was found, when that is the case. It becomes
    /// the onboarding sentence, so an operator reads the exact reason.
    problem: Option<String>,
}

impl SlackBackend {
    pub fn new(api: SlackApi, gateway: impl Into<String>) -> Self {
        Self {
            api,
            base: None,
            gateway: gateway.into(),
            platform: "slack".to_string(),
            started: Instant::now(),
            inbound: None,
            self_id: None,
            problem: None,
        }
    }

    /// Build from the environment: `--token`, then the credential file the
    /// gateway names, then `SLACK_CREDENTIAL`, then the `SLACK_APP_TOKEN` +
    /// `SLACK_BOT_TOKEN` pair.
    ///
    /// A missing or unparseable credential is carried as [`Self::problem`]
    /// rather than raised, because it is the state `onlyne-im login` exists to
    /// fix and this backend has to be running to answer that login.
    pub fn from_env(
        explicit: Option<String>,
        gateway: impl Into<String>,
        base: Option<String>,
    ) -> Self {
        let loaded = onlyne_im_backend::credential::load_two(
            explicit.as_deref(),
            CREDENTIAL_ENV,
            APP_TOKEN_ENV,
            BOT_TOKEN_ENV,
        );
        let (credentials, problem) = match SlackCredentials::parse(&loaded.value) {
            Ok(credentials) => (Some(credentials), loaded.problem),
            Err(_) if !loaded.is_present() => (None, loaded.problem),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        let base = base
            .or_else(|| std::env::var(API_BASE_ENV).ok())
            .filter(|base| !base.trim().is_empty());
        let (app_token, bot_token) = match &credentials {
            Some(credentials) => (credentials.app_token.clone(), credentials.bot_token.clone()),
            None => (String::new(), String::new()),
        };
        let mut build_problem = None;
        let api = match SlackApi::new(app_token, bot_token) {
            Ok(api) => match api.on_base(base.as_deref()) {
                Ok(api) => api,
                Err(error) => {
                    build_problem = Some(format!("{error:#}"));
                    SlackApi::new("", "").expect("a connector over no token")
                }
            },
            Err(error) => {
                build_problem = Some(format!("{error:#}"));
                SlackApi::new("", "").expect("a connector over no token")
            }
        };
        let mut backend = Self::new(api, gateway);
        backend.base = base;
        backend.problem = problem.or(build_problem);
        backend
    }

    /// The onboarding sentence an unauthenticated backend reports.
    ///
    /// Slack needs two tokens from two different pages of the app
    /// configuration, and an operator who pastes one where the other belongs
    /// gets two different failures. The sentence names both.
    fn credential_prompt(problem: Option<&str>) -> OnboardingPrompt {
        let base = "paste `xapp-…:xoxb-…` — the app-level token from Basic \
                    Information → App-level tokens (Socket Mode), a colon, then the bot \
                    token from OAuth & Permissions; or set SLACK_APP_TOKEN and \
                    SLACK_BOT_TOKEN (or SLACK_CREDENTIAL)";
        let payload = match problem {
            Some(problem) => format!("{base} ({problem})"),
            None => base.to_string(),
        };
        OnboardingPrompt {
            kind: OnboardingKind::ManualCode,
            payload,
            expires_in: None,
        }
    }

    /// Whether both halves of a credential are held.
    ///
    /// A login clears the problem that was recorded when the process started
    /// without a credential; what makes the backend configured afterwards is
    /// the pair it now holds.
    fn configured(&self) -> bool {
        !self.api.app_token().trim().is_empty() && !self.api.token().trim().is_empty()
    }
}

#[async_trait]
impl Backend for SlackBackend {
    fn hello(&self) -> HelloArgs {
        // Slack's capabilities are fixed: messages in and out, no button
        // interactions (the render path cannot show them), and no typing
        // indicator (the Web API has none).
        let authenticated = self.configured();
        HelloArgs {
            platform: self.platform.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec!["message".into()],
            authenticated,
            onboarding: (!authenticated).then(|| Self::credential_prompt(self.problem.as_deref())),
        }
    }

    /// Verify a pasted `xapp-…:xoxb-…` credential, both halves.
    ///
    /// `auth.test` checks the bot token and names the bot;
    /// `apps.connections.open` checks the app-level token and needs the
    /// `connections:write` scope. Checking only the bot token would accept a
    /// credential whose socket could never open, and the operator would learn
    /// that from a reconnect log instead of from `login`. The ticket the second
    /// call mints is discarded — the listener mints its own.
    ///
    /// Both checks run on a candidate client, and the swap happens only after
    /// Slack accepted both: a refused login leaves this backend holding what
    /// it had.
    ///
    /// Both probes go through the Web API root, so [`API_BASE_ENV`] mocks both.
    /// [`WS_URL_ENV`] has no bearing on them and never skips a check.
    async fn login(&mut self, args: LoginArgs) -> Result<Option<String>> {
        let credentials = SlackCredentials::parse(&args.credential).context(
            "the credential must be `xapp-…:xoxb-…`, both from the Slack app configuration",
        )?;
        let candidate = self
            .api
            .with_token(credentials.bot_token.clone())
            .with_app_token(credentials.app_token.clone())
            .on_base(self.base.as_deref())?;
        let who = candidate
            .auth_test()
            .await
            .context("Slack refused this bot token; check it against OAuth & Permissions")?;
        candidate.open_socket().await.context(
            "Slack refused this app-level token; check it against Basic Information → \
             App-level tokens, and that Socket Mode is on",
        )?;

        // A socket registered under the old app token belongs to the old app;
        // the next one must be minted with the token just accepted.
        if let Some(inbound) = self.inbound.take() {
            if inbound.app_token() != credentials.app_token {
                let mut inbound = inbound;
                inbound.shutdown().await;
            } else {
                self.inbound = Some(inbound);
            }
        }
        self.api = candidate;
        self.self_id = Some(who.clone());
        self.problem = None;
        Ok(Some(who))
    }

    /// The conversations this bot can see: `conversations.list`, every page.
    async fn list_conversations(&mut self) -> Result<Vec<ImConversation>> {
        let channels = self.api.conversations().await?;
        Ok(channels.iter().filter_map(conversation_row).collect())
    }

    /// Block until one Slack event arrives.
    ///
    /// The SDK's listener owns the connection: it reconnects after a
    /// `disconnect` frame, runs its own ping, and acknowledges envelopes as
    /// this backend queues them. This call waits on the queue, so cancelling
    /// it — which the serve loop does on every core frame — loses nothing.
    async fn next_event(&mut self) -> Result<Option<ImEvent>> {
        if !self.configured() {
            bail!(
                "no usable credential: {}",
                self.problem.as_deref().unwrap_or("unknown reason")
            );
        }
        if self.inbound.is_none() {
            let inbound = ws::Inbound::start(
                &self.api,
                self.gateway.clone(),
                self.platform.clone(),
                self.self_id.clone(),
            )
            .await;
            match inbound {
                Ok(inbound) => {
                    tracing::info!("the slack event stream is open");
                    self.inbound = Some(inbound);
                }
                Err(error) => {
                    // The SDK owns reconnecting; a failed registration is the
                    // one case it cannot retry from, so it is reported.
                    return Err(error.context("opening the slack event stream"));
                }
            }
        }
        let event = self
            .inbound
            .as_mut()
            .expect("opened above")
            .next_event()
            .await;
        Ok(event)
    }

    /// Send one payload into one conversation.
    ///
    /// A payload with neither text nor an attachment is refused rather than
    /// answered with an empty success: the gateway records the answer, and a
    /// message id of `None` for a send that posted nothing is a lie it would
    /// carry into the reply chain.
    ///
    /// A failure after the text posted answers `Ok` with that post's `ts` and
    /// the error: the message exists on Slack, so a later reply can still
    /// thread under it.
    async fn send(&mut self, args: SendArgs) -> Result<SentArgs> {
        let SendArgs {
            conversation,
            payload,
        } = args;
        let calls = render::render(&conversation, &payload);
        if calls.is_empty() {
            bail!("the payload has no text and no attachment to send");
        }
        let mut message_id = None;
        for call in calls {
            match call {
                render::Call::Post {
                    channel,
                    text,
                    thread_ts,
                    mrkdwn,
                } => {
                    let posted = self
                        .api
                        .post_message(&channel, &text, thread_ts.as_deref(), mrkdwn)
                        .await;
                    match posted {
                        // The ts is this message's own id, and the only thing
                        // a later reply can thread under.
                        Ok(ts) => message_id = Some(ts),
                        Err(error) => {
                            return Ok(SentArgs {
                                conversation,
                                message_id,
                                ref_handle: None,
                                error: Some(format!("sending to {channel}: {error:#}")),
                            });
                        }
                    }
                }
                render::Call::Upload {
                    channel,
                    path,
                    thread_ts,
                } => {
                    // `files.completeUploadExternal` answers file objects, not
                    // a message ts, so an upload inherits the anchor post's ts
                    // when the payload was not a reply: the file then lives in
                    // a thread a later reply can address.
                    let thread_ts = thread_ts.or_else(|| message_id.clone());
                    let uploaded = self
                        .api
                        .upload_file(&path, &channel, thread_ts.as_deref())
                        .await;
                    if let Err(error) = uploaded {
                        // The anchor is already live. Answering `Err` would let
                        // the serve loop replace this answer with one carrying
                        // no id, losing the only handle a reply can thread
                        // under.
                        return Ok(SentArgs {
                            conversation,
                            message_id,
                            ref_handle: None,
                            error: Some(format!("uploading {path} to {channel}: {error:#}")),
                        });
                    }
                }
            }
        }
        Ok(SentArgs {
            conversation,
            message_id,
            // One id per message on Slack: the ts is both the message id and
            // the handle a reply uses as `thread_ts`.
            ref_handle: None,
            error: None,
        })
    }

    fn health(&self) -> HealthArgs {
        let state = if !self.configured() {
            "failed"
        } else if self.inbound.is_some() {
            "online"
        } else {
            "reconnecting"
        };
        HealthArgs {
            state: state.to_string(),
            detail: self.problem.clone(),
            uptime_s: self.started.elapsed().as_secs(),
        }
    }

    /// Close the connection politely, so Slack frees the connection slot at
    /// once instead of waiting for its own timeout.
    async fn stop(&mut self) -> Result<()> {
        if let Some(mut inbound) = self.inbound.take() {
            inbound.shutdown().await;
        }
        Ok(())
    }
}

/// One conversation row, from a `conversations.list` channel object.
fn conversation_row(channel: &slack_morphism::prelude::SlackChannelInfo) -> Option<ImConversation> {
    let flag = |value: Option<bool>| value.unwrap_or(false);
    let flags = &channel.flags;
    let kind = if flag(flags.is_im) {
        "im"
    } else if flag(flags.is_mpim) {
        "mpim"
    } else if flag(flags.is_group) || flag(flags.is_private) {
        // Slack's channel object calls a private channel a group.
        "group"
    } else if flag(flags.is_channel) {
        "channel"
    } else {
        return None;
    };
    Some(ImConversation {
        id: channel.id.value().clone(),
        title: channel.name.clone(),
        kind: Some(kind.to_string()),
    })
}

/// Build the backend from the environment and run the standard protocol loop.
pub async fn run(gateway: String, token: Option<String>, base: Option<String>) -> Result<()> {
    let explicit = token.filter(|token| !token.trim().is_empty());
    onlyne_im_backend::serve(SlackBackend::from_env(explicit, gateway, base)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use slack_morphism::prelude::*;

    #[test]
    fn a_pair_splits_on_the_first_colon() {
        let credentials = SlackCredentials::parse("xapp-1-abc:xoxb-2-def").unwrap();
        assert_eq!(credentials.app_token, "xapp-1-abc");
        assert_eq!(credentials.bot_token, "xoxb-2-def");
        assert_eq!(credentials.encode(), "xapp-1-abc:xoxb-2-def");
    }

    #[test]
    fn whitespace_around_the_halves_is_trimmed() {
        let credentials = SlackCredentials::parse("  xapp-a : xoxb-b  ").unwrap();
        assert_eq!(credentials.app_token, "xapp-a");
        assert_eq!(credentials.bot_token, "xoxb-b");
    }

    #[test]
    fn a_missing_separator_names_the_encoding() {
        let error = SlackCredentials::parse("xoxb-only").unwrap_err();
        assert!(format!("{error:#}").contains("xapp-…:xoxb-…"), "{error:#}");
    }

    #[test]
    fn an_empty_half_names_which_half() {
        let error = SlackCredentials::parse(":xoxb-b").unwrap_err();
        assert!(
            format!("{error:#}").contains("app-level token"),
            "{error:#}"
        );
        let error = SlackCredentials::parse("xapp-a:").unwrap_err();
        assert!(format!("{error:#}").contains("bot token"), "{error:#}");
    }

    #[test]
    fn a_parseable_credential_reports_authenticated_and_names_its_capabilities() {
        let backend = SlackBackend::from_env(Some("xapp-a:xoxb-b".into()), "gw", None);
        let hello = backend.hello();
        assert!(hello.authenticated, "{:?}", hello.onboarding);
        assert_eq!(hello.platform, "slack");
        assert_eq!(hello.capabilities, vec!["message".to_string()]);
        assert!(hello.onboarding.is_none());
        assert_eq!(backend.health().state, "reconnecting");
    }

    #[test]
    fn an_unparseable_credential_is_a_problem_not_a_panic() {
        let backend = SlackBackend::from_env(Some("not-a-credential".into()), "gw", None);
        let hello = backend.hello();
        assert!(!hello.authenticated);
        let onboarding = hello
            .onboarding
            .expect("an unauthenticated backend says why");
        assert_eq!(onboarding.kind, OnboardingKind::ManualCode);
        assert!(
            onboarding.payload.contains("xapp-…:xoxb-…"),
            "{}",
            onboarding.payload
        );
        assert_eq!(backend.health().state, "failed");
        assert!(backend.health().detail.is_some());
    }

    #[tokio::test]
    async fn an_unconfigured_backend_refuses_to_stream() {
        let mut backend = SlackBackend::from_env(Some("garbage".into()), "gw", None);
        let error = backend
            .next_event()
            .await
            .expect_err("nothing to connect with");
        assert!(format!("{error:#}").contains("no usable credential"));
    }

    #[test]
    fn conversations_map_slack_channel_flags_to_kinds() {
        let row = |value: serde_json::Value| {
            let channel: SlackChannelInfo = serde_json::from_value(value).expect("a channel");
            conversation_row(&channel)
        };
        let channel = row(json!({
            "id": "C012AB3CD", "name": "general", "created": 1,
            "is_channel": true, "is_group": false, "is_im": false, "is_private": false
        }))
        .unwrap();
        assert_eq!(channel.id, "C012AB3CD");
        assert_eq!(channel.title.as_deref(), Some("general"));
        assert_eq!(channel.kind.as_deref(), Some("channel"));

        let group = row(json!({
            "id": "G1", "name": "secret", "created": 1, "is_group": true, "is_private": true
        }))
        .unwrap();
        assert_eq!(group.kind.as_deref(), Some("group"));

        let dm = row(json!({"id": "D0C0F7S8Y", "created": 1, "is_im": true})).unwrap();
        assert_eq!(dm.kind.as_deref(), Some("im"));

        let mpim = row(json!({
            "id": "G0AKFJBEU", "name": "mpdm-a--b-1", "created": 1,
            "is_group": true, "is_mpim": true
        }))
        .unwrap();
        assert_eq!(mpim.kind.as_deref(), Some("mpim"));
    }

    /// Serve one HTTP answer per request, in order, and record what arrived.
    fn fake_api(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::sync::{Arc, Mutex};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            let mut index = 0usize;
            loop {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    if index >= responses.len() {
                        return;
                    }
                    continue;
                };
                use std::io::{Read, Write};
                let mut buffer = [0u8; 8192];
                let read = socket.read(&mut buffer).unwrap_or(0);
                log.lock()
                    .expect("log")
                    .push(String::from_utf8_lossy(&buffer[..read]).into_owned());
                let (status, body) = responses
                    .get(index)
                    .copied()
                    .unwrap_or((200, r#"{"ok":false}"#));
                index += 1;
                let reason = if status == 200 { "OK" } else { "Error" };
                let answer = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(answer.as_bytes());
            }
        });
        (format!("http://{addr}"), seen)
    }

    async fn wait_for(seen: &std::sync::Mutex<Vec<String>>, count: usize) {
        for _ in 0..200 {
            if seen.lock().unwrap().len() >= count {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn login_checks_the_bot_token_then_the_app_token() {
        let (base, seen) = fake_api(vec![
            (
                200,
                r#"{"ok":true,"user":"onlyne","user_id":"U1","team":"T1","team_id":"T1","url":"https://t1.slack.com/"}"#,
            ),
            (
                200,
                r#"{"ok":true,"url":"wss://wss.example/link/?ticket=1"}"#,
            ),
        ]);
        let mut backend = SlackBackend::from_env(None, "gw", Some(base.clone()));
        assert_eq!(backend.base.as_deref(), Some(base.as_str()));
        let identity = backend
            .login(LoginArgs {
                gateway: "gw".into(),
                credential: "xapp-app:xoxb-bot".into(),
            })
            .await
            .expect("both tokens accepted");
        assert_eq!(identity.as_deref(), Some("onlyne"));
        assert_eq!(backend.self_id.as_deref(), Some("onlyne"));
        assert_eq!(backend.base.as_deref(), Some(base.as_str()));

        wait_for(&seen, 2).await;
        let requests = seen.lock().unwrap();
        assert!(requests.len() >= 2, "{requests:?}");
        // Both probes happened, each on its own credential. Asserted on the
        // recorded set rather than by index: the fake records one `read()` per
        // connection, and under load that can be a partial request line.
        assert!(
            requests
                .iter()
                .any(|request| request.starts_with("GET /auth.test")),
            "{requests:?}"
        );
        assert!(
            requests.iter().any(|request| request
                .to_lowercase()
                .contains("authorization: bearer xoxb-bot")),
            "the bot token is the one auth.test carries: {requests:?}"
        );
        assert!(
            requests
                .iter()
                .any(|request| request.starts_with("POST /apps.connections.open")),
            "{requests:?}"
        );
        assert!(
            requests.iter().any(|request| request
                .to_lowercase()
                .contains("authorization: bearer xapp-app")),
            "the app token is the one the mint carries: {requests:?}"
        );
    }

    #[tokio::test]
    async fn login_refuses_when_the_app_token_is_rejected() {
        let (base, _seen) = fake_api(vec![
            (
                200,
                r#"{"ok":true,"user":"onlyne","team":"T1","team_id":"T1","user_id":"U1","url":"https://t1.slack.com/"}"#,
            ),
            (200, r#"{"ok":false,"error":"invalid_auth"}"#),
        ]);
        let mut backend = SlackBackend::from_env(None, "gw", Some(base));
        let error = backend
            .login(LoginArgs {
                gateway: "gw".into(),
                credential: "xapp-bad:xoxb-bot".into(),
            })
            .await
            .expect_err("a bad app token is a refused login");
        assert!(
            format!("{error:#}").contains("app-level token"),
            "{error:#}"
        );
        // What it held before the refusal is still what it holds.
        assert!(!backend.hello().authenticated);
        assert!(backend.api.token().is_empty());
    }

    /// A payload with one real local file: a post (the anchor) then an upload.
    /// The directory must outlive the send, since the upload reads the bytes.
    fn attachment_payload() -> (SendArgs, tempfile::TempDir) {
        use onlyne_im_backend::payload::{ImAttachment, ImPayload};
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("slack-review-report.pdf");
        std::fs::write(&path, b"the report").expect("write the attachment");
        (
            SendArgs {
                conversation: "C1".into(),
                payload: ImPayload {
                    text: "here".into(),
                    attachments: vec![ImAttachment {
                        kind: "file".into(),
                        path: path.to_string_lossy().into_owned(),
                        alt: None,
                    }],
                    ..ImPayload::default()
                },
            },
            dir,
        )
    }

    /// One send against a fake API, answering the given responses in order.
    async fn send_against(
        responses: Vec<(u16, &'static str)>,
    ) -> (SentArgs, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let (base, seen) = fake_api(responses);
        let mut backend = SlackBackend::from_env(Some("xapp-a:xoxb-b".into()), "gw", Some(base));
        let (args, _dir) = attachment_payload();
        let sent = backend
            .send(args)
            .await
            .expect("a refusal is an answer, not a failed call");
        (sent, seen)
    }

    #[tokio::test]
    async fn a_send_failure_keeps_the_anchor_id_it_already_earned() {
        /// (post answer, upload-ticket answer, expected id, error expected).
        type Case = (
            (u16, &'static str),
            (u16, &'static str),
            Option<&'static str>,
            bool,
        );
        let cases: Vec<Case> = vec![
            (
                (
                    200,
                    r#"{"ok":true,"channel":"C1","ts":"111.222","message":{"text":"here","user":"U1","ts":"111.222"}}"#,
                ),
                (200, r#"{"ok":false,"error":"invalid_auth"}"#),
                Some("111.222"),
                true,
            ),
            (
                (
                    200,
                    r#"{"ok":true,"channel":"C1","ts":"111.222","message":{"text":"here","user":"U1","ts":"111.222"}}"#,
                ),
                (429, r#"{"ok":false,"error":"ratelimited"}"#),
                Some("111.222"),
                true,
            ),
            (
                (200, r#"{"ok":false,"error":"channel_not_found"}"#),
                (
                    200,
                    r#"{"ok":true,"upload_url":"http://127.0.0.1:9/upload","file_id":"F1"}"#,
                ),
                None,
                true,
            ),
        ];
        for (index, (post, ticket, expected_id, expects_error)) in cases.into_iter().enumerate() {
            let (sent, _seen) = send_against(vec![post, ticket]).await;
            assert_eq!(
                sent.message_id.as_deref(),
                expected_id,
                "case {index}: a posted anchor's id must survive a later failure"
            );
            assert_eq!(
                sent.error.is_some(),
                expects_error,
                "case {index}: every failure answers with its reason"
            );
            assert_eq!(sent.conversation, "C1");
        }
    }

    #[tokio::test]
    async fn a_rate_limit_stays_identifiable_in_the_error() {
        let (sent, _seen) = send_against(vec![
            (
                200,
                r#"{"ok":true,"channel":"C1","ts":"111.222","message":{"text":"here","user":"U1","ts":"111.222"}}"#,
            ),
            (429, r#"{"ok":false,"error":"ratelimited"}"#),
        ])
        .await;
        let error = sent.error.expect("a refused upload reports why");
        assert!(error.contains("rate limit"), "{error}");
        assert!(error.contains("ratelimited"), "{error}");
        assert!(error.contains("files.getUploadURLExternal"), "{error}");
    }

    #[tokio::test]
    async fn an_empty_payload_is_refused_rather_than_answered_with_no_id() {
        let mut backend = SlackBackend::from_env(Some("xapp-a:xoxb-b".into()), "gw", None);
        let error = backend
            .send(SendArgs {
                conversation: "C1".into(),
                payload: onlyne_im_backend::payload::ImPayload::default(),
            })
            .await
            .expect_err("nothing to send");
        assert!(
            format!("{error:#}").contains("no text and no attachment"),
            "{error:#}"
        );
    }
}
