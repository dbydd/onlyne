//! The Linear GraphQL client, on top of `linear-api`.
//!
//! One endpoint, one credential: an API key travels in the `Authorization`
//! header with no scheme prefix, against `https://api.linear.app/graphql`.
//! Everything Linear-specific this backend sends lives here, so Linear's
//! request and response shapes stop at this module's public methods.
//!
//! The typed operations (viewer, issue list, comment create) and the retry
//! and rate-limit handling come from [`linear_api::LinearClient`]; this module
//! only names the operations and maps their answers. Two things stay outside
//! that client: the root `comments` connection filtered by `updatedAt` (a
//! document the SDK does not model, executed as a query through
//! `execute_raw`), and `fileUpload` plus the signed PUT that follows it (an
//! operation the SDK does not provide, so it is a plain POST and then a plain
//! PUT).
//!
//! The schema facts these queries depend on were read from Linear's published
//! schema and developer docs (2026-10-17,
//! <https://linear.app/developers/graphql>, <https://linear.app/developers/pagination>,
//! <https://linear.app/developers/filtering>, <https://linear.app/developers/attachments>):
//! `comments(filter: CommentFilter, orderBy: PaginationOrderBy, after, first)`
//! is the root-level comments connection, `CommentFilter.updatedAt` is a
//! `DateComparator`, `commentCreate(input: CommentCreateInput!)` answers
//! `CommentPayload { success, comment }`, and `fileUpload` hands back a signed
//! URL plus headers rather than accepting bytes on the GraphQL endpoint.

use anyhow::{Context, Result, bail};
use linear_api::comments::CommentCreateInput;
use linear_api::issues::ListIssuesRequest;
use linear_api::{CommentId, LinearClient, PaginationOrderBy};
use serde_json::{Value, json};

/// The real endpoint. [`Client::with_base`] points elsewhere (a fake in tests).
pub const DEFAULT_API_URL: &str = "https://api.linear.app/graphql";

/// The comments window: every comment at or after `mark`, oldest first.
const COMMENTS_SINCE: &str = r#"query Comments($mark: DateTime!, $after: String) {
  comments(
    filter: { updatedAt: { gte: $mark } }
    orderBy: updatedAt
    first: 50
    after: $after
  ) {
    nodes {
      id
      body
      createdAt
      updatedAt
      url
      parent { id }
      user { id name }
      externalUser { id name }
      issue { id identifier title }
    }
    pageInfo { hasNextPage endCursor }
  }
}"#;

/// The one mutation the SDK does not cover. Linear's documented flow is two
/// steps: ask `fileUpload` for a signed URL, then PUT the bytes there.
const FILE_UPLOAD: &str = r#"mutation Upload($contentType: String!, $filename: String!, $size: Int!) {
  fileUpload(contentType: $contentType, filename: $filename, size: $size, makePublic: false) {
    success
    uploadFile {
      uploadUrl
      assetUrl
      headers { key value }
    }
  }
}"#;

/// The viewer a credential belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    pub id: String,
    pub name: Option<String>,
}

/// One page of the root comments connection.
#[derive(Debug, Clone, Default)]
pub struct CommentsPage {
    /// The comments themselves, in the order the server returned them.
    pub nodes: Vec<Value>,
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
}

/// One issue, as `list_conversations` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRow {
    pub id: String,
    pub identifier: String,
    pub title: String,
}

/// A comment this backend just created: the id is all Linear's typed create
/// returns that the echo set needs (it answers no `updatedAt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub id: String,
}

/// One API key and the endpoint it talks to.
pub struct Client {
    key: String,
    base: String,
    sdk: std::result::Result<LinearClient, String>,
    /// Uploads only: the signed PUT carries no GraphQL and no API key, and the
    /// `fileUpload` call that precedes it is not part of the SDK's surface.
    http: reqwest::Client,
}

impl Client {
    pub fn new(key: impl Into<String>) -> Self {
        Self::at(key.into(), DEFAULT_API_URL.to_string(), None)
    }

    /// One client, reusing an existing connection pool when there is one.
    ///
    /// The provider is installed before the HTTP client is built: this crate's
    /// reqwest uses `rustls-no-provider`, and a `Client` constructed without a
    /// provider panics.
    fn at(key: String, base: String, http: Option<reqwest::Client>) -> Self {
        // reqwest was built with `rustls-no-provider`, so the provider has to
        // be named before any client exists. Doing it here covers every entry
        // point — the binary installs it too, and a second install is the
        // same provider.
        onlyne_im_backend::install_crypto_provider();
        // `Client::new` is `Default`, and the provider above is what keeps the
        // default constructor from panicking.
        let http = http.unwrap_or_default();
        let sdk = build(&key, &base);
        Self {
            key,
            base,
            sdk,
            http,
        }
    }

    /// Point the client at another GraphQL endpoint (a fake server in tests).
    pub fn with_base(self, base: impl Into<String>) -> Self {
        Self::at(self.key, base.into(), Some(self.http))
    }

    /// The key this client sends. A backend reports whether it holds one in its
    /// `hello` without contacting Linear.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The same client pointed at another key, reusing the connection pool.
    /// A login check swaps the key in before asking `viewer`, so what it
    /// verifies is exactly what it would send with.
    pub fn with_key(&self, key: impl Into<String>) -> Self {
        Self::at(key.into(), self.base.clone(), Some(self.http.clone()))
    }

    /// The identity this key belongs to, through the SDK's typed viewer query.
    ///
    /// This is the whole of authentication. A Linear API key *is* the
    /// credential, so verifying it is one query, and the name it answers is
    /// what an operator reads back to know they pasted the right key.
    pub async fn viewer(&self) -> Result<Viewer> {
        let viewer = self
            .sdk()?
            .viewer()
            .await
            .map_err(sdk_error)
            .context("POST Linear's GraphQL endpoint")?;
        Ok(Viewer {
            id: viewer.id.to_string(),
            name: Some(viewer.name),
        })
    }

    /// One page of comments updated at or after `mark`, oldest first.
    ///
    /// `after` is the cursor from the previous page. The filter is inclusive,
    /// so the caller owns the boundary dedupe (see `LinearBackend::poll`).
    ///
    /// The document goes through `execute_raw`, which classifies it as a query:
    /// transport failures and rate limits are retried, and a retry can never
    /// replay a write.
    pub async fn comments_since(&self, mark: &str, after: Option<&str>) -> Result<CommentsPage> {
        let data = self
            .sdk()?
            .execute_raw(COMMENTS_SINCE, json!({ "mark": mark, "after": after }))
            .await
            .map_err(sdk_error)
            .context("POST Linear's GraphQL endpoint")?;
        let connection = data.get("comments").context("comments answered nothing")?;
        let page_info = connection.get("pageInfo");
        Ok(CommentsPage {
            nodes: connection
                .get("nodes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            end_cursor: page_info
                .and_then(|info| info.get("endCursor"))
                .and_then(Value::as_str)
                .map(str::to_string),
            has_next_page: page_info
                .and_then(|info| info.get("hasNextPage"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    /// Create one comment on an issue, optionally nested under a parent
    /// comment, through the SDK's typed mutation.
    ///
    /// The typed create is a mutation, so the SDK will not retry it after a
    /// post-send transport error or a 5xx: Linear has no idempotency keys and a
    /// blind retry can double-post. Rate-limit rejections happen before
    /// execution and are still retried by the SDK.
    pub async fn create_comment(
        &self,
        issue_id: &str,
        body: &str,
        parent_id: Option<&str>,
    ) -> Result<Created> {
        let input = CommentCreateInput::builder()
            .issue_id(issue_id)
            .body(body.to_string())
            .maybe_parent_id(parent_id.map(CommentId::new))
            .build();
        let comment = self
            .sdk()?
            .comments()
            .create(input)
            .await
            .map_err(sdk_error)
            .context("POST Linear's GraphQL endpoint")?;
        Ok(Created {
            id: comment.id.into_inner(),
        })
    }

    /// Recently updated issues, newest first, through the SDK's typed list.
    pub async fn recent_issues(&self, first: i32) -> Result<Vec<IssueRow>> {
        let page = self
            .sdk()?
            .issues()
            .list(
                ListIssuesRequest::builder()
                    .first(first)
                    .order_by(PaginationOrderBy::UpdatedAt)
                    .build(),
            )
            .await
            .map_err(sdk_error)
            .context("POST Linear's GraphQL endpoint")?;
        Ok(page
            .nodes
            .into_iter()
            .map(|issue| IssueRow {
                id: issue.id.into_inner(),
                identifier: issue.identifier,
                title: issue.title,
            })
            .collect())
    }

    /// Upload one local file to Linear's storage and answer the asset URL a
    /// comment body can link to.
    ///
    /// The asset is private (`makePublic: false`), which is the right default:
    /// only signed in users of this workspace can open it, and comment readers
    /// are exactly those users.
    pub async fn upload(&self, path: &str) -> Result<String> {
        let bytes = tokio::fs::read(path)
            .await
            .with_context(|| format!("reading {path}"))?;
        let name = std::path::Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let size =
            i32::try_from(bytes.len()).with_context(|| format!("{path} is too large to upload"))?;
        let data = self.file_upload(media_type(path), &name, size).await?;
        let payload = data
            .get("fileUpload")
            .context("fileUpload answered nothing")?;
        if !payload
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            bail!("Linear refused the upload of {path}");
        }
        let file = payload
            .get("uploadFile")
            .context("Linear accepted the upload but named no file")?;
        let upload_url = file
            .get("uploadUrl")
            .and_then(Value::as_str)
            .context("Linear's upload answered no uploadUrl")?;
        let asset_url = file
            .get("assetUrl")
            .and_then(Value::as_str)
            .context("Linear's upload answered no assetUrl")?;
        let mut request = self.http.put(upload_url).body(bytes);
        if let Some(headers) = file.get("headers").and_then(Value::as_array) {
            for header in headers {
                if let (Some(key), Some(value)) = (
                    header.get("key").and_then(Value::as_str),
                    header.get("value").and_then(Value::as_str),
                ) {
                    request = request.header(key, value);
                }
            }
        }
        let response = request
            .send()
            .await
            // The signed upload URL carries a signature in its query string,
            // and reqwest's error Display carries the URL, so an un-stripped
            // error writes that signature into the operator's log.
            .map_err(reqwest::Error::without_url)
            .with_context(|| format!("uploading {path}"))?;
        if !response.status().is_success() {
            bail!(
                "Linear's upload endpoint refused {path} with HTTP {}",
                response.status()
            );
        }
        Ok(asset_url.to_string())
    }

    /// The SDK client, or the reason it could not be built.
    fn sdk(&self) -> Result<&LinearClient> {
        self.sdk
            .as_ref()
            .map_err(|reason| anyhow::anyhow!("{reason}"))
    }

    /// The GraphQL half of `fileUpload`: one plain POST that asks Linear for a
    /// signed URL. It stays outside the SDK because the SDK has no upload
    /// surface; sending it through `execute_raw` would classify a
    /// non-idempotent mutation as a query and replay it on a transport retry.
    async fn file_upload(&self, content_type: &str, filename: &str, size: i32) -> Result<Value> {
        let response = self
            .http
            .post(&self.base)
            .header("Authorization", &self.key)
            .json(&json!({
                "query": FILE_UPLOAD,
                "variables": {
                    "contentType": content_type,
                    "filename": filename,
                    "size": size,
                },
            }))
            .send()
            .await
            // A transport error's Display carries the URL, and an operator's
            // endpoint is user-supplied configuration that can hold a query
            // token. The request's identity is all that belongs in the log.
            .map_err(reqwest::Error::without_url)
            .context("POST Linear's GraphQL endpoint")?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(reqwest::Error::without_url)
            .context("Linear answered no JSON")?;
        let errors = value
            .get("errors")
            .and_then(Value::as_array)
            .filter(|errors| !errors.is_empty());
        if let Some(errors) = errors {
            let messages: Vec<&str> = errors
                .iter()
                .filter_map(|error| error.get("message").and_then(Value::as_str))
                .collect();
            bail!(
                "Linear refused the request (HTTP {status}): {}",
                if messages.is_empty() {
                    "no message".to_string()
                } else {
                    messages.join("; ")
                }
            );
        }
        if !status.is_success() {
            bail!("Linear answered HTTP {status}");
        }
        Ok(value.get("data").cloned().unwrap_or(Value::Null))
    }
}

/// Build the SDK client, keeping its configuration message free of the
/// endpoint: the SDK's own `Config` error quotes the URL, and an operator's
/// endpoint override can carry a query token.
fn build(key: &str, base: &str) -> std::result::Result<LinearClient, String> {
    LinearClient::builder()
        .api_key(key)
        .endpoint(base)
        .build()
        .map_err(|error| match error {
            linear_api::Error::Config(_) => {
                "the Linear client could not be configured; check the API key and the endpoint URL"
                    .to_string()
            }
            other => format!("{other}"),
        })
}

/// One SDK error as an operator-facing sentence, with no URL in it.
///
/// The SDK's `[Error::Transport]` is a `reqwest::Error` whose Display carries
/// the request URL — which is the configured endpoint, possibly holding a
/// token. It is stripped here; every other variant's Display is already just
/// the failure. GraphQL messages are Linear's own words and ride along.
fn sdk_error(error: linear_api::Error) -> anyhow::Error {
    match error {
        linear_api::Error::Transport(error) => anyhow::anyhow!("{}", error.without_url()),
        linear_api::Error::Api { errors, .. } => {
            let messages: Vec<&str> = errors.iter().map(|error| error.message.as_str()).collect();
            anyhow::anyhow!(
                "Linear refused the request: {}",
                if messages.is_empty() {
                    "no message".to_string()
                } else {
                    messages.join("; ")
                }
            )
        }
        linear_api::Error::Http { status, .. } => anyhow::anyhow!("Linear answered HTTP {status}"),
        other => anyhow::anyhow!("{other}"),
    }
}

/// A MIME type by extension, for `fileUpload`. Linear stores what it is given
/// and serves it back with that type; `application/octet-stream` is the honest
/// answer for anything not in this short list.
fn media_type(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("txt" | "log") => "text/plain",
        Some("json") => "application/json",
        Some("csv") => "text/csv",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{self, ok_json};

    #[tokio::test]
    async fn the_viewer_query_sends_the_key_without_a_bearer_prefix() {
        let fake = fake::serve(vec![fake::viewer("user-1", "Ada")]).await;
        let client = Client::new("lin_api_test").with_base(fake.graphql_url());
        let viewer = client.viewer().await.unwrap();
        assert_eq!(viewer.id, "user-1");
        assert_eq!(viewer.name.as_deref(), Some("Ada"));

        let requests = fake.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/graphql");
        assert_eq!(requests[0].authorization.as_deref(), Some("lin_api_test"));
    }

    #[tokio::test]
    async fn comments_since_passes_the_mark_and_cursor_and_reads_the_page() {
        let fake = fake::serve(vec![ok_json(
            r#"{"data":{"comments":{"nodes":[
                {"id":"c1","body":"first","updatedAt":"2026-10-17T10:00:00.000Z",
                 "issue":{"id":"i1","identifier":"ENG-1","title":"One"},
                 "user":{"id":"u1","name":"Ada"},"parent":null}
            ],"pageInfo":{"hasNextPage":true,"endCursor":"cursor-1"}}}}"#,
        )])
        .await;
        let client = Client::new("key").with_base(fake.graphql_url());
        let page = client
            .comments_since("2026-10-17T09:00:00.000Z", Some("cursor-0"))
            .await
            .unwrap();
        assert_eq!(page.nodes.len(), 1);
        assert_eq!(page.nodes[0]["id"], "c1");
        assert!(page.has_next_page);
        assert_eq!(page.end_cursor.as_deref(), Some("cursor-1"));

        let request = &fake.requests()[0];
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["variables"]["mark"], "2026-10-17T09:00:00.000Z");
        assert_eq!(body["variables"]["after"], "cursor-0");
        let query = body["query"].as_str().unwrap();
        assert!(query.contains("comments("), "{query}");
        assert!(query.contains("updatedAt: { gte: $mark }"), "{query}");
        assert!(query.contains("orderBy: updatedAt"), "{query}");
        assert!(query.contains("first: 50"), "{query}");
    }

    #[tokio::test]
    async fn create_comment_sends_issue_body_and_parent() {
        let fake = fake::serve(vec![ok_json(
            r#"{"data":{"commentCreate":{"success":true,"comment":{
                "id":"c9","body":"hello","url":"https://linear.app/comment/c9",
                "user":{"id":"u1","name":"Ada","displayName":"ada"},
                "createdAt":"2026-10-17T10:05:00.000Z",
                "editedAt":null,"resolvedAt":null,"parent":null}}}}"#,
        )])
        .await;
        let client = Client::new("key").with_base(fake.graphql_url());
        let comment = client
            .create_comment("issue-1", "hello", Some("c8"))
            .await
            .unwrap();
        assert_eq!(comment.id, "c9");

        let body: Value = serde_json::from_str(&fake.requests()[0].body).unwrap();
        let input = &body["variables"]["input"];
        assert_eq!(input["issueId"], "issue-1");
        assert_eq!(input["body"], "hello");
        assert_eq!(input["parentId"], "c8");
    }

    #[tokio::test]
    async fn a_comment_without_a_parent_omits_the_field() {
        let fake = fake::serve(vec![fake::created("c9")]).await;
        let client = Client::new("key").with_base(fake.graphql_url());
        client
            .create_comment("issue-1", "hello", None)
            .await
            .unwrap();
        let body: Value = serde_json::from_str(&fake.requests()[0].body).unwrap();
        assert!(body["variables"]["input"].get("parentId").is_none());
    }

    #[tokio::test]
    async fn a_refused_mutation_is_the_platforms_sentence() {
        let fake = fake::serve(vec![ok_json(
            r#"{"errors":[{"message":"Issue not found"}]}"#,
        )])
        .await;
        let client = Client::new("key").with_base(fake.graphql_url());
        let error = client
            .create_comment("missing", "hello", None)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("Issue not found"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn upload_asks_for_a_signed_url_then_puts_the_bytes() {
        let fake = fake::serve(vec![]).await;
        let response = ok_json(&format!(
            r#"{{"data":{{"fileUpload":{{"success":true,"uploadFile":{{
                "uploadUrl":"{}",
                "assetUrl":"https://files.linear.app/asset-1",
                "headers":[{{"key":"x-goog-meta-test","value":"yes"}}]
            }}}}}}}}"#,
            fake.upload_url()
        ));
        fake.push(response);
        fake.push(ok_json(""));

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.txt");
        tokio::fs::write(&path, b"the bytes").await.unwrap();

        let client = Client::new("key").with_base(fake.graphql_url());
        let asset = client.upload(path.to_str().unwrap()).await.unwrap();
        assert_eq!(asset, "https://files.linear.app/asset-1");

        let requests = fake.requests();
        assert_eq!(requests.len(), 2);
        let body: Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body["variables"]["filename"], "report.txt");
        assert_eq!(body["variables"]["contentType"], "text/plain");
        assert_eq!(requests[1].method, "PUT");
        assert_eq!(requests[1].path, "/upload");
        assert_eq!(requests[1].body, "the bytes");
        assert!(
            requests[1]
                .head
                .to_ascii_lowercase()
                .contains("x-goog-meta-test: yes"),
            "{}",
            requests[1].head
        );
    }

    #[test]
    fn media_types_follow_the_extension() {
        assert_eq!(media_type("/tmp/a.PNG"), "image/png");
        assert_eq!(media_type("/tmp/a.tar.gz"), "application/octet-stream");
    }

    /// A custom endpoint can carry a token in its query string, and a refused
    /// connection is the ordinary way an operator first meets it. The error
    /// must name the request, never the operator's URL.
    #[tokio::test]
    async fn a_transport_failure_does_not_echo_the_endpoint() {
        // Bind and drop, so the port is almost certainly closed.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let client =
            Client::new("key").with_base(format!("http://{addr}/graphql?token=super-secret"));
        let error = client.viewer().await.unwrap_err();
        let message = format!("{error:#}");
        assert!(
            !message.contains("super-secret"),
            "the endpoint's query token leaked: {message}"
        );
        assert!(
            !message.contains(&addr.to_string()),
            "the endpoint leaked: {message}"
        );
        assert!(message.contains("GraphQL endpoint"), "{message}");
    }

    /// An endpoint that is not a URL fails at construction, and the sentence
    /// carries no part of it: the override is user configuration.
    #[tokio::test]
    async fn an_invalid_endpoint_is_reported_without_quoting_it() {
        let client = Client::new("key").with_base("not a url?token=super-secret");
        let error = client.viewer().await.unwrap_err();
        let message = format!("{error:#}");
        assert!(
            !message.contains("super-secret"),
            "the endpoint's query token leaked: {message}"
        );
        assert!(message.contains("could not be configured"), "{message}");
    }
}
