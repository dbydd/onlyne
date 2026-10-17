//! The Feishu Open API client, on the `openlark` SDK.
//!
//! Everything outbound is an SDK request: the chat list
//! (`ListChatsRequest`), a send (`CreateMessageRequest`), a reply
//! (`ReplyMessageRequest`), and the tenant-token check a login performs
//! (`AuthService::v3().tenant_access_token_internal`). The SDK's
//! `AuthTokenProvider` owns the token lifecycle for every authenticated call —
//! it fetches the tenant token, caches it, and refreshes it before expiry — so
//! nothing here handles a token by hand.
//!
//! Inbound does not come from here: Feishu has no polling API, events arrive
//! over the SDK's websocket connection (`open_lark::ws_client::LarkWsClient`,
//! exactly what v1 used), and that client holds its own connection lifecycle.

use anyhow::{Context, Result};
use onlyne_im_backend::frame::ImConversation;
use serde_json::Value;
use std::future::Future;
use std::time::Duration;

use open_lark::CoreConfig;
use open_lark::auth::{AuthService, AuthTokenProvider};
use open_lark::communication::im::v1::chat::list::ListChatsRequest;
use open_lark::communication::im::v1::message::create::{CreateMessageBody, CreateMessageRequest};
use open_lark::communication::im::v1::message::models::ReceiveIdType;
use open_lark::communication::im::v1::message::reply::{ReplyMessageBody, ReplyMessageRequest};

/// The chat-list page size this backend asks for: Feishu's maximum, so a
/// bot in many groups reaches the end in as few calls as the API allows.
const PAGE_SIZE: i32 = 100;

/// One Feishu API client: the app identity and the SDK configuration the
/// requests run on.
pub struct Api {
    creds: crate::auth::FeishuCredentials,
    /// The SDK configuration with the tenant-token provider attached, built
    /// once per credential pair so every request this backend makes shares one
    /// token cache. `None` when the backend holds no usable credential.
    config: Option<CoreConfig>,
}

impl Api {
    pub fn new(creds: crate::auth::FeishuCredentials) -> Self {
        let config = sdk_config(&creds);
        Self { creds, config }
    }

    /// The same client pointed at another credential pair. A login check swaps
    /// the pair in before fetching a tenant token, so what it verifies is
    /// exactly what it would send with.
    pub fn with_credentials(&self, creds: crate::auth::FeishuCredentials) -> Self {
        Self::new(creds)
    }

    /// The credential pair this client sends. A backend reports whether it
    /// holds one in its `hello` without contacting Feishu.
    pub fn credentials(&self) -> &crate::auth::FeishuCredentials {
        &self.creds
    }

    fn config(&self) -> Result<&CoreConfig> {
        self.config
            .as_ref()
            .context("this backend holds no usable Feishu credential")
    }

    /// Fetch a tenant access token. This is the whole of authentication: the
    /// pair is verified by whether Feishu issues a token, so `login` is one
    /// call. It is the SDK's own typed builder, which reads the endpoint's
    /// top-level `tenant_access_token`/`expire` body and declares no token.
    /// The provider behind the authenticated requests keeps its own cached
    /// token; this call exists to make Feishu answer for the pair.
    pub async fn tenant_token(&self) -> Result<String> {
        let config = self.config()?.clone();
        let response = AuthService::new(config)
            .v3()
            .tenant_access_token_internal()
            .app_id(self.creds.app_id.clone())
            .app_secret(self.creds.app_secret.clone())
            .execute()
            .await
            .context("feishu token request")?;
        Ok(response.data.tenant_access_token)
    }

    /// Post one rendered request body into one conversation.
    ///
    /// The `receive_id_type` follows the conversation's shape, as before: an
    /// `ou_…` open id is a p2p peer, anything else (`oc_…`) is a chat id.
    /// A body naming a `reply_id` goes through the reply endpoint, which is
    /// Feishu's own way to thread an answer under the message it answers.
    pub async fn post_message(&self, conversation: &str, request: Value) -> Result<String> {
        let msg_type = request
            .get("msg_type")
            .and_then(Value::as_str)
            .context("a rendered request names its message type")?
            .to_string();
        let content = request
            .get("content")
            .and_then(Value::as_str)
            .context("a rendered request carries its content as a JSON string")?
            .to_string();
        let config = self.config()?.clone();
        let data = match request
            .get("reply_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            Some(message_id) => ReplyMessageRequest::new(config)
                .message_id(message_id)
                .execute(ReplyMessageBody {
                    content,
                    msg_type,
                    reply_in_thread: None,
                    uuid: None,
                })
                .await
                .context("feishu reply")?,
            None => CreateMessageRequest::new(config)
                .receive_id_type(receive_id_type(conversation))
                .execute(CreateMessageBody {
                    receive_id: conversation.to_string(),
                    msg_type,
                    content,
                    uuid: None,
                })
                .await
                .context("feishu send")?,
        };
        data.get("message_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("feishu send missing message_id: {data}"))
    }

    /// The groups and chats this bot is in, walked page by page.
    ///
    /// `GET /open-apis/im/v1/chats` answers the chats the app itself belongs
    /// to, which is exactly the reach question: a chat the bot was just added
    /// to appears here without any event having arrived yet.
    pub async fn list_chats(&self) -> Result<Vec<ImConversation>> {
        let config = self.config()?.clone();
        walk_chats(move |token| {
            let config = config.clone();
            async move {
                let mut request = ListChatsRequest::new(config).page_size(PAGE_SIZE);
                if let Some(token) = token {
                    request = request.page_token(token);
                }
                request.execute().await.context("feishu chat list")
            }
        })
        .await
    }
}

/// Walk every page a fetch answers until one carries no next token.
///
/// A token the server repeats would loop forever, so a page whose token is the
/// one just used ends the walk: a malformed answer must not hang the setup
/// verb.
async fn walk_chats<F, Fut>(mut fetch: F) -> Result<Vec<ImConversation>>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<Value>>,
{
    let mut chats = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let page = fetch(page_token.clone()).await?;
        let (mut rows, next) = chat_page(&page);
        chats.append(&mut rows);
        match next {
            Some(token) if page_token.as_deref() != Some(token.as_str()) => {
                page_token = Some(token);
            }
            _ => break,
        }
    }
    Ok(chats)
}

/// One page of `GET /open-apis/im/v1/chats`: the conversations it carries and
/// the token for the next page, when there is one.
fn chat_page(page: &Value) -> (Vec<ImConversation>, Option<String>) {
    let rows = page
        .get("items")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(conversation_of).collect())
        .unwrap_or_default();
    let has_more = page
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let token = page
        .get("page_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_string);
    (rows, token.filter(|_| has_more))
}

/// One row of the chat list, in the standard shape.
///
/// Feishu names the group in `name` and its mode in `chat_mode` (`p2p`,
/// `group`, `topic`); the standard word for a one-to-one chat is `private`, so
/// that is what `p2p` becomes. A row without a `chat_mode` is a group — the
/// listing is what the bot belongs to, and a group is the default shape there.
fn conversation_of(item: &Value) -> Option<ImConversation> {
    let id = item.get("chat_id").and_then(Value::as_str)?.to_string();
    let title = item
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    let kind = match item.get("chat_mode").and_then(Value::as_str) {
        Some("p2p") => "private",
        Some("topic") => "topic",
        _ => "group",
    };
    Some(ImConversation {
        id,
        title,
        kind: Some(kind.to_string()),
    })
}

/// The SDK configuration one credential pair runs on.
///
/// The tenant-token provider is what makes an authenticated request work: it
/// fetches and caches `tenant_access_token`, and refreshes it before expiry,
/// so no call here carries a token. `None` for the placeholder identity an
/// unauthenticated backend constructs, which never contacts Feishu.
fn sdk_config(creds: &crate::auth::FeishuCredentials) -> Option<CoreConfig> {
    if creds.app_id.is_empty() || creds.app_secret.is_empty() {
        return None;
    }
    let base = CoreConfig::builder()
        .app_id(creds.app_id.clone())
        .app_secret(creds.app_secret.clone())
        .base_url(creds.base_url())
        .req_timeout(Duration::from_secs(30))
        .build();
    Some(base.with_token_provider(AuthTokenProvider::new(base.clone())))
}

/// Which id a conversation is addressed by: an `ou_…` open id is a p2p peer,
/// anything else (`oc_…`) is a chat id.
fn receive_id_type(conversation: &str) -> ReceiveIdType {
    if conversation.starts_with("ou_") {
        ReceiveIdType::OpenId
    } else {
        ReceiveIdType::ChatId
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn one_page_carries_its_rows_and_the_next_token() {
        let page = json!({
            "items": [
                {"chat_id": "oc_1", "name": "Release", "chat_mode": "group", "external": false},
                {"chat_id": "oc_2", "name": "External partners", "chat_mode": "group", "external": true}
            ],
            "has_more": true,
            "page_token": "page-2"
        });
        let (rows, next) = chat_page(&page);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "oc_1");
        assert_eq!(rows[0].title.as_deref(), Some("Release"));
        assert_eq!(rows[0].kind.as_deref(), Some("group"));
        assert_eq!(rows[1].title.as_deref(), Some("External partners"));
        assert_eq!(next.as_deref(), Some("page-2"));
    }

    #[test]
    fn the_last_page_carries_no_token_even_when_the_server_sends_one() {
        let page = json!({"items": [], "has_more": false, "page_token": "leftover"});
        let (rows, next) = chat_page(&page);
        assert!(rows.is_empty());
        assert!(next.is_none(), "has_more=false ends the walk");
    }

    #[test]
    fn a_p2p_row_is_private_and_a_row_without_a_mode_is_a_group() {
        let p2p = conversation_of(&json!({"chat_id": "oc_x", "name": "Ada", "chat_mode": "p2p"}))
            .expect("a chat id is enough");
        assert_eq!(p2p.kind.as_deref(), Some("private"));
        assert_eq!(p2p.title.as_deref(), Some("Ada"));

        let untyped =
            conversation_of(&json!({"chat_id": "oc_y", "name": ""})).expect("a chat id is enough");
        assert_eq!(untyped.kind.as_deref(), Some("group"));
        assert!(untyped.title.is_none(), "an empty name is no title");
    }

    #[test]
    fn a_row_without_a_chat_id_is_not_a_conversation() {
        assert!(conversation_of(&json!({"name": "nameless"})).is_none());
    }

    #[test]
    fn a_conversation_addresses_open_ids_as_peers_and_everything_else_as_chats() {
        assert_eq!(receive_id_type("ou_user"), ReceiveIdType::OpenId);
        assert_eq!(receive_id_type("oc_group"), ReceiveIdType::ChatId);
    }

    #[test]
    fn an_empty_credential_builds_no_sdk_configuration() {
        let creds = crate::auth::FeishuCredentials::new("", "", crate::auth::DEFAULT_DOMAIN);
        assert!(sdk_config(&creds).is_none());
        let creds =
            crate::auth::FeishuCredentials::new("cli_x", "secret", crate::auth::DEFAULT_DOMAIN);
        assert!(sdk_config(&creds).is_some());
    }

    #[tokio::test]
    async fn the_walk_follows_every_page_and_asks_for_each_token_once() {
        let pages = [
            json!({"items": [{"chat_id": "oc_1", "name": "One"}], "has_more": true, "page_token": "p2"}),
            json!({"items": [{"chat_id": "oc_2", "name": "Two"}], "has_more": true, "page_token": "p3"}),
            json!({"items": [{"chat_id": "oc_3", "name": "Three"}], "has_more": false}),
        ];
        let asked = std::sync::Mutex::new(Vec::new());
        let chats = walk_chats(|token| {
            let index = {
                let mut asked = asked.lock().unwrap();
                asked.push(token.clone());
                asked.len() - 1
            };
            let page = pages[index].clone();
            async move { Ok(page) }
        })
        .await
        .expect("three pages walk");
        let ids: Vec<&str> = chats.iter().map(|chat| chat.id.as_str()).collect();
        assert_eq!(ids, ["oc_1", "oc_2", "oc_3"]);
        assert_eq!(
            asked.into_inner().unwrap(),
            vec![None, Some("p2".to_string()), Some("p3".to_string())]
        );
    }

    #[tokio::test]
    async fn a_repeated_page_token_ends_the_walk() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let chats = walk_chats(|_| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                Ok(json!({
                    "items": [{"chat_id": "oc_same", "name": "Same"}],
                    "has_more": true,
                    "page_token": "same"
                }))
            }
        })
        .await
        .expect("a stuck token must not loop");
        assert_eq!(chats.len(), 2, "the second page repeats the token and ends");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
