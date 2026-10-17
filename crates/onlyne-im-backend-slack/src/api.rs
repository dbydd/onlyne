//! The Slack client: one handle over Slack Morphism's Web API session.
//!
//! Every call this backend makes is an SDK method — `auth.test`,
//! `apps.connections.open`, `conversations.list`, `chat.postMessage`, and the
//! external upload trio — so this module holds no request shape, no URL, and
//! no `{ok, error}` envelope: it holds the two tokens and the connector they
//! are spent through. Everything above speaks the standard payload or the
//! standard event.
//!
//! Two tokens, two jobs: the app-level token (`xapp-…`) is what
//! `apps.connections.open` accepts and the only thing Socket Mode needs; the
//! bot token (`xoxb-…`) authenticates everything else.

use crate::wire::{SlackConnector, connector};
use anyhow::{Context, Result};
use slack_morphism::prelude::*;
use std::sync::Arc;

/// The largest page `conversations.list` recommends.
const PAGE_LIMIT: u16 = 200;
/// The media type the external upload endpoint receives.
const UPLOAD_CONTENT_TYPE: &str = "application/octet-stream";

/// One Web API client: the SDK client and the two tokens it spends.
#[derive(Clone)]
pub struct SlackApi {
    client: Arc<SlackClient<SlackConnector>>,
    bot: SlackApiToken,
    app: SlackApiToken,
}

impl SlackApi {
    /// A client holding `app_token` and `bot_token`, answering on Slack's own
    /// Web API root.
    pub fn new(app_token: impl Into<String>, bot_token: impl Into<String>) -> Result<Self> {
        Self::on(connector(None)?, app_token, bot_token)
    }

    /// The same pair on an existing connector: a login candidate reuses the
    /// transport of the client it was made from.
    pub fn on(
        connector: SlackConnector,
        app_token: impl Into<String>,
        bot_token: impl Into<String>,
    ) -> Result<Self> {
        Ok(Self {
            client: Arc::new(SlackClient::new(connector)),
            bot: SlackApiToken::new(SlackApiTokenValue(bot_token.into())),
            app: SlackApiToken::new(SlackApiTokenValue(app_token.into())),
        })
    }

    /// The same tokens on `base`, when this backend answers somewhere other
    /// than Slack. A login checks a candidate here, so what it verifies is
    /// exactly what this backend would send with.
    pub fn on_base(&self, base: Option<&str>) -> Result<Self> {
        Self::on(
            connector(base)?,
            self.app.token_value.value().clone(),
            self.bot.token_value.value().clone(),
        )
    }

    /// The connector its calls go through, for the listener.
    pub fn connector(&self) -> &SlackConnector {
        &self.client.http_api.connector
    }

    /// The bot token this client sends. A backend reports whether it holds one
    /// in its `hello` without contacting Slack.
    pub fn token(&self) -> &str {
        self.bot.token_value.value()
    }

    /// The app-level token a socket is minted with.
    pub fn app_token(&self) -> &str {
        self.app.token_value.value()
    }

    /// The same connector with the bot token replaced.
    pub fn with_token(&self, bot_token: impl Into<String>) -> Self {
        Self {
            client: self.client.clone(),
            bot: SlackApiToken::new(SlackApiTokenValue(bot_token.into())),
            app: self.app.clone(),
        }
    }

    /// The same connector with the app-level token replaced.
    pub fn with_app_token(&self, app_token: impl Into<String>) -> Self {
        Self {
            client: self.client.clone(),
            bot: self.bot.clone(),
            app: SlackApiToken::new(SlackApiTokenValue(app_token.into())),
        }
    }

    /// The bot this token belongs to: `auth.test`.
    ///
    /// The whole of authentication. Slack has no separate login — the token is
    /// the credential — so verifying it is one call, and the name it answers
    /// is the bot username an operator reads back.
    pub async fn auth_test(&self) -> Result<String> {
        let answer = self.session().auth_test().await.context("auth.test")?;
        Ok(answer
            .user
            .clone()
            .unwrap_or_else(|| answer.user_id.value().clone()))
    }

    /// Mint one Socket Mode connection URL: `apps.connections.open` with the
    /// app-level token.
    ///
    /// The websocket URL is not static: Slack mints it per connection and
    /// expires it. This call is how `login` checks the app token; the listener
    /// mints its own connections by the same method.
    pub async fn open_socket(&self) -> Result<String> {
        let session = self.client.open_session(&self.app);
        let answer = session
            .apps_connections_open(&SlackApiAppsConnectionOpenRequest::new())
            .await
            .context("apps.connections.open")?;
        Ok(answer.url.value().to_string())
    }

    /// Post one message: `chat.postMessage`.
    ///
    /// `thread_ts` is what makes the message a thread reply; the answer's `ts`
    /// is this message's own id, and the only id a later reply can point at.
    pub async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
        mrkdwn: bool,
    ) -> Result<String> {
        let request = SlackApiChatPostMessageRequest::new(
            SlackChannelId(channel.to_string()),
            SlackMessageContent::new().with_text(text.to_string()),
        )
        .opt_thread_ts(thread_ts.map(|ts| SlackTs(ts.to_string())));
        // A plain payload turns Slack's markup parsing off, so an agent's
        // `*word*` and `<path>` stay literal. The switch lives on the
        // serializer, which the SDK's request model leaves to the caller.
        let request = if mrkdwn {
            request
        } else {
            request.with_parse("none".to_string())
        };
        let sent = self
            .session()
            .chat_post_message(&request)
            .await
            .context("chat.postMessage")?;
        Ok(sent.ts.value().clone())
    }

    /// The conversations this bot can see: `conversations.list`, every page.
    ///
    /// Public channels, private channels, group DMs, and DMs the bot belongs
    /// to. Archived channels are excluded — an operator cannot send there.
    pub async fn conversations(&self) -> Result<Vec<SlackChannelInfo>> {
        let mut conversations = Vec::new();
        let mut cursor: Option<SlackCursorId> = None;
        let request = SlackApiConversationsListRequest::new()
            .with_types(vec![
                SlackConversationType::Public,
                SlackConversationType::Private,
                SlackConversationType::Mpim,
                SlackConversationType::Im,
            ])
            .with_exclude_archived(true)
            .with_limit(PAGE_LIMIT);
        loop {
            let page = self
                .session()
                .conversations_list(&request.clone().opt_cursor(cursor.clone()))
                .await
                .context("conversations.list")?;
            conversations.extend(page.channels);
            match page.response_metadata.and_then(|meta| meta.next_cursor) {
                Some(next) if Some(&next) != cursor.as_ref() => cursor = Some(next),
                _ => break,
            }
        }
        Ok(conversations)
    }

    /// Share one local file into a conversation.
    ///
    /// The SDK's external upload flow: ask for a URL and a file id, POST the
    /// bytes to that URL, then finalize — the finalize call is what actually
    /// shares the file, and skipping it discards the upload. An error names
    /// the file, because a path that never arrived must not read as delivered.
    pub async fn upload_file(
        &self,
        path: &str,
        channel: &str,
        thread_ts: Option<&str>,
    ) -> Result<()> {
        let bytes = tokio::fs::read(path)
            .await
            .with_context(|| format!("reading {path} for upload"))?;
        let name = std::path::Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let session = self.session();
        let ticket = session
            .get_upload_url_external(&SlackApiFilesGetUploadUrlExternalRequest::new(
                name.clone(),
                bytes.len(),
            ))
            .await
            .with_context(|| format!("files.getUploadURLExternal for {path}"))?;
        session
            .files_upload_via_url(&SlackApiFilesUploadViaUrlRequest::new(
                ticket.upload_url,
                bytes,
                UPLOAD_CONTENT_TYPE.to_string(),
            ))
            .await
            // The upload URL is a signed ticket to Slack's file service; the
            // error names the file and not that URL.
            .with_context(|| format!("uploading the bytes of {path}"))?;
        session
            .files_complete_upload_external(
                &SlackApiFilesCompleteUploadExternalRequest::new(vec![
                    SlackApiFilesComplete::new(ticket.file_id).with_title(name),
                ])
                .with_channel_id(SlackChannelId(channel.to_string()))
                .opt_thread_ts(thread_ts.map(|ts| SlackTs(ts.to_string()))),
            )
            .await
            .with_context(|| format!("files.completeUploadExternal for {path}"))?;
        Ok(())
    }

    fn session(&self) -> SlackClientSession<'_, SlackConnector> {
        self.client.open_session(&self.bot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One canned response the fake server hands out per request, in order.
    type Response = (u16, String);

    async fn bind() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a fake server");
        let addr = listener.local_addr().expect("local addr");
        (listener, format!("http://{addr}"))
    }

    /// Serve `responses` in order, answering the methods they were written
    /// for, and record every request it saw.
    ///
    /// A response is keyed by index, so the recording order is the request
    /// order; `Connection: close` keeps one connection per request, which is
    /// what makes that order trustworthy. A body that is JSON goes out as
    /// JSON; anything else — Slack's file upload endpoint answers a plain
    /// `OK` — goes out as text, which is what the SDK has to accept.
    fn serve(
        listener: tokio::net::TcpListener,
        responses: Vec<Response>,
    ) -> Arc<Mutex<Vec<String>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            let mut index = 0usize;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let request = read_request(&mut socket).await;
                let (status, body) = responses
                    .get(index)
                    .cloned()
                    .unwrap_or((500, "{}".to_string()));
                index += 1;
                log.lock().expect("request log").push(request);
                let reason = if status == 200 { "OK" } else { "Error" };
                let content_type = match serde_json::from_str::<Value>(&body) {
                    Ok(_) => "application/json",
                    Err(_) => "text/plain",
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        seen
    }

    /// Read one HTTP request: head, then the body its `Content-Length` names.
    async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = socket.read(&mut chunk).await.unwrap_or(0);
            if read > 0 {
                buffer.extend_from_slice(&chunk[..read]);
            }
            let Some(head_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
                if read == 0 {
                    break;
                }
                continue;
            };
            let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    if !name.eq_ignore_ascii_case("content-length") {
                        return None;
                    }
                    value.trim().parse::<usize>().ok()
                })
                .unwrap_or(0);
            if buffer.len() >= head_end + 4 + length || read == 0 {
                break;
            }
        }
        String::from_utf8_lossy(&buffer).into_owned()
    }

    fn body_of(request: &str) -> Value {
        let body = request
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or("");
        serde_json::from_str(body).unwrap_or(Value::Null)
    }

    #[tokio::test]
    async fn post_message_carries_channel_text_and_thread() {
        let (listener, base) = bind().await;
        let seen = serve(
            listener,
            vec![(
                200,
                r#"{"ok":true,"channel":"C1","ts":"1355517523.000005","message":{"text":"hello","user":"U1","ts":"1355517523.000005"}}"#
                    .to_string(),
            )],
        );
        let api = SlackApi::new("xapp-app", "xoxb-test")
            .expect("a connector")
            .on_base(Some(&base))
            .expect("the fake root");
        let ts = api
            .post_message("C1", "hello", Some("1355517523.000001"), true)
            .await
            .expect("post");
        assert_eq!(ts, "1355517523.000005");

        let requests = seen.lock().unwrap();
        let request = &requests[0];
        assert!(
            request.starts_with("POST /chat.postMessage HTTP/1.1"),
            "{request}"
        );
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer xoxb-test"),
            "{request}"
        );
        let body = body_of(request);
        assert_eq!(body["channel"], "C1");
        assert_eq!(body["text"], "hello");
        assert_eq!(body["thread_ts"], "1355517523.000001");
        assert!(
            body.get("parse").is_none(),
            "markdown leaves Slack's default parsing alone: {body}"
        );
    }

    #[tokio::test]
    async fn a_plain_payload_disables_slack_markup() {
        let (listener, base) = bind().await;
        let seen = serve(
            listener,
            vec![(
                200,
                r#"{"ok":true,"channel":"C1","ts":"1","message":{"text":"a *word*","user":"U1","ts":"1"}}"#
                    .to_string(),
            )],
        );
        let api = SlackApi::new("xapp-app", "xoxb-test")
            .expect("a connector")
            .on_base(Some(&base))
            .expect("the fake root");
        api.post_message("C1", "a *word*", None, false)
            .await
            .expect("post");
        let requests = seen.lock().unwrap();
        let body = body_of(&requests[0]);
        assert_eq!(body["parse"], "none");
        assert!(body.get("thread_ts").is_none());
    }

    #[tokio::test]
    async fn a_refusal_names_slacks_own_error() {
        let (listener, base) = bind().await;
        serve(
            listener,
            vec![(
                200,
                r#"{"ok":false,"error":"channel_not_found"}"#.to_string(),
            )],
        );
        let api = SlackApi::new("xapp-app", "xoxb-test")
            .expect("a connector")
            .on_base(Some(&base))
            .expect("the fake root");
        let error = api
            .post_message("C404", "hello", None, true)
            .await
            .expect_err("a refused post is an error");
        assert!(
            format!("{error:#}").contains("channel_not_found"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn conversations_reads_every_page() {
        let (listener, base) = bind().await;
        let seen = serve(
            listener,
            vec![
                (
                    200,
                    r#"{"ok":true,"channels":[{"id":"C1","name":"general","is_channel":true,"created":1}],"response_metadata":{"next_cursor":"abc"}}"#.to_string(),
                ),
                (
                    200,
                    r#"{"ok":true,"channels":[{"id":"D1","is_im":true,"created":2}],"response_metadata":{"next_cursor":""}}"#.to_string(),
                ),
            ],
        );
        let api = SlackApi::new("xapp-app", "xoxb-test")
            .expect("a connector")
            .on_base(Some(&base))
            .expect("the fake root");
        let channels = api.conversations().await.expect("list");
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].id.value(), "C1");
        assert_eq!(channels[1].id.value(), "D1");

        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 2, "one request per page");
        assert!(requests[0].starts_with("GET /conversations.list?"));
        assert!(requests[0].contains("exclude_archived=true"));
        assert!(requests[0].contains("limit=200"));
        assert!(requests[1].contains("cursor=abc"), "{}", requests[1]);
    }

    #[tokio::test]
    async fn open_socket_uses_the_app_token() {
        let (listener, base) = bind().await;
        let seen = serve(
            listener,
            vec![(
                200,
                r#"{"ok":true,"url":"wss://wss.example/link/?ticket=1"}"#.to_string(),
            )],
        );
        let api = SlackApi::new("xapp-app", "xoxb-test")
            .expect("a connector")
            .on_base(Some(&base))
            .expect("the fake root");
        let url = api.open_socket().await.expect("open");
        assert_eq!(url, "wss://wss.example/link/?ticket=1");
        let requests = seen.lock().unwrap();
        assert!(requests[0].starts_with("POST /apps.connections.open"));
        assert!(
            requests[0]
                .to_lowercase()
                .contains("authorization: bearer xapp-app"),
            "{}",
            requests[0]
        );
    }

    #[tokio::test]
    async fn upload_file_walks_the_external_upload_flow() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("note.txt");
        std::fs::write(&path, b"the bytes").expect("write the file");
        let path = path.to_string_lossy().into_owned();

        let (listener, base) = bind().await;
        let seen = serve(
            listener,
            vec![
                (
                    200,
                    format!(
                        r#"{{"ok":true,"upload_url":"{}/upload/v1/abc","file_id":"F1"}}"#,
                        base.clone()
                    ),
                ),
                // Slack's file upload endpoint answers a plain `OK` with a
                // text content type, not a JSON envelope; the SDK accepts it,
                // and the fixture has to be that shape so this test covers
                // the real contract rather than a friendlier one.
                (200, "OK".to_string()),
                (
                    200,
                    r#"{"ok":true,"files":[{"id":"F1","title":"note.txt"}]}"#.to_string(),
                ),
            ],
        );
        let api = SlackApi::new("xapp-app", "xoxb-test")
            .expect("a connector")
            .on_base(Some(&base))
            .expect("the fake root");
        api.upload_file(&path, "C1", Some("1.2"))
            .await
            .expect("upload");

        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 3, "{requests:?}");
        let ticket_line = requests[0].lines().next().unwrap_or_default();
        assert!(
            ticket_line.starts_with("GET /files.getUploadURLExternal?"),
            "{ticket_line}"
        );
        assert!(requests[0].contains("filename=note.txt"), "{}", requests[0]);
        assert!(requests[0].contains("length=9"), "{}", requests[0]);
        assert!(requests[1].starts_with("POST /upload/v1/abc"));
        assert!(
            requests[1].ends_with("the bytes"),
            "the upload carries the file's bytes"
        );
        assert!(requests[2].starts_with("POST /files.completeUploadExternal"));
        let complete = body_of(&requests[2]);
        assert_eq!(complete["files"][0]["id"], "F1");
        assert_eq!(complete["channel_id"], "C1");
        assert_eq!(complete["thread_ts"], "1.2");
    }
}
