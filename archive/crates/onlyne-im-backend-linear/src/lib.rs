//! Linear platform backend for the Onlyne IM gateway.
//!
//! Implements the standard backend protocol (`onlyne-im-backend`) and talks to
//! Linear's GraphQL API. The two translation halves live in [`update`] (a
//! comment becomes a standard event) and [`render`] (a standard payload becomes
//! a comment body); [`api`] holds every GraphQL call, on top of the
//! [`linear_api`] client, which brings the typed operations, the retries and
//! the rate-limit handling. This module is the `Backend` that joins them.
//!
//! Inbound is polling, not webhooks: the gateway is a local process and Linear
//! webhooks need a public HTTPS endpoint. The poll reads the root `comments`
//! connection for comments whose `updatedAt` is at or after a high-water mark,
//! oldest first. Only comment writes surface as events — an issue edit, a
//! state change, or a description rewrite produces no event — which is the
//! honest noise bound of this shape.

mod api;
mod render;
mod update;

#[cfg(test)]
mod fake;

use anyhow::{Context, Result, bail};
use api::{Client, CommentsPage};
use async_trait::async_trait;
use onlyne_im_backend::backend::Backend;
use onlyne_im_backend::frame::{
    HealthArgs, HelloArgs, ImConversation, LoginArgs, OnboardingKind, OnboardingPrompt, SendArgs,
    SentArgs,
};
use onlyne_im_backend::payload::{ImEvent, ImPayload};
use serde_json::Value;
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

/// The conventional key variable, for an operator who exports one instead of
/// running `onlyne-im login`.
pub const API_KEY_ENV: &str = "LINEAR_API_KEY";
/// The GraphQL endpoint override, so a fake server can stand in for tests.
pub const API_URL_ENV: &str = "ONLYNE_LINEAR_API_URL";
/// The poll interval in seconds; ten by default.
pub const POLL_SECS_ENV: &str = "ONLYNE_LINEAR_POLL_SECS";
/// The default poll interval.
pub const DEFAULT_POLL_SECS: u64 = 10;
/// How many created comment ids the echo set remembers. A comment this backend
/// sent more than this many sends ago may be reported once more; no comment is
/// ever lost because of the cap.
const ECHO_CAP: usize = 1024;
/// How many recently updated issues `list_conversations` reports.
const ISSUE_LIST_SIZE: i32 = 50;

/// One Linear backend: an API key, the poll mark, and the comments a poll
/// fetched but has not reported yet.
pub struct LinearBackend {
    client: Client,
    gateway: String,
    platform: String,
    interval: Duration,
    /// Comments fetched by the last poll and not yet handed to the gateway.
    /// A poll drains the whole window, so a `next_event` call between polls
    /// serves from here instead of fetching again.
    queue: VecDeque<Value>,
    /// The highest `updatedAt` served. The query filters with this as an
    /// inclusive lower bound, so the id set below dedupes the boundary.
    ///
    /// It is set when the backend is constructed (to "now", so a cold start
    /// has no backlog) and advanced only by [`LinearBackend::ingest`], after a
    /// poll has drained the whole window. A cancelled or failed poll therefore
    /// cannot move it.
    mark: String,
    /// Comment ids already served at exactly `mark` (the filter is inclusive).
    /// It is cleared whenever the mark advances, so it holds one window's
    /// boundary; every id in it is a comment the gateway has been told about.
    /// Its size is bounded by the comments sharing one server-assigned
    /// millisecond, which is why it needs no arbitrary cap — and a cap here
    /// would be the wrong trade: dropping an id re-serves a comment, and the
    /// window is fully drained before the mark moves, so a large same-instant
    /// group cannot starve anything.
    at_mark: HashSet<String>,
    /// Comments this backend itself created, with the `updatedAt` Linear
    /// answered. They are skipped on the way back in, so the poll loop does not
    /// feed the gateway its own output.
    ///
    /// A personal API key carries no separate bot identity — the key acts as
    /// its owner — so filtering by *author* would also drop that person's own
    /// comments typed in Linear's UI. Filtering by created id drops exactly the
    /// comments this backend can vouch for, and nothing else.
    echoes: VecDeque<(String, String)>,
    started: Instant,
    /// When the last poll was attempted, for pacing retries.
    last_poll: Option<Instant>,
    /// When the last poll succeeded, which is what `health` reports.
    last_success: Option<Instant>,
    last_error: Option<String>,
    /// Why no credential was found, when that is the case. It becomes the
    /// onboarding sentence, so an operator reads the exact reason.
    problem: Option<String>,
}

impl LinearBackend {
    pub fn new(client: Client, gateway: impl Into<String>) -> Self {
        Self {
            client,
            gateway: gateway.into(),
            platform: "linear".to_string(),
            interval: Duration::from_secs(DEFAULT_POLL_SECS),
            queue: VecDeque::new(),
            // Fixed at construction rather than in the first poll: the poll is
            // cancelable, and a cancellation before it committed "now" would
            // move the window forward and skip comments written in between.
            mark: now(),
            at_mark: HashSet::new(),
            echoes: VecDeque::new(),
            started: Instant::now(),
            last_poll: None,
            last_success: None,
            last_error: None,
            problem: None,
        }
    }

    /// Build from the environment: `--token`, then the credential file the
    /// gateway names, then `LINEAR_API_KEY`. A missing credential is carried
    /// as [`Self::problem`] rather than raised, because it is the state
    /// `onlyne-im login` exists to fix and this backend has to be running to
    /// answer that login.
    pub fn from_env(
        explicit: Option<String>,
        gateway: impl Into<String>,
        base: Option<String>,
    ) -> Self {
        let loaded = onlyne_im_backend::credential::load(explicit.as_deref(), API_KEY_ENV);
        let mut client = Client::new(loaded.value);
        let base = base.or_else(|| std::env::var(API_URL_ENV).ok());
        if let Some(base) = base.filter(|base| !base.trim().is_empty()) {
            client = client.with_base(base);
        }
        let mut backend = Self::new(client, gateway);
        backend.problem = loaded.problem;
        backend.interval = Duration::from_secs(poll_secs_from_env());
        backend
    }

    /// The onboarding sentence an unauthenticated backend reports. Linear's
    /// credential is one API key, created under Security & access, pasted.
    fn key_prompt(problem: Option<&str>) -> OnboardingPrompt {
        let base = "paste a Linear API key (lin_api_…), or set LINEAR_API_KEY";
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

    /// One poll: the whole window from the mark, page by page, to the end.
    ///
    /// There is no page cap. Committing the newest timestamp of a truncated
    /// window would put every remaining page below the mark, and those
    /// comments would never be served: the mark moves only once the connection
    /// reports no next page. The cursor lives in this call, so nothing on
    /// `self` changes until the last await has returned — a cancelled poll (the
    /// gateway may cancel `next_event` while it delivers a send) leaves the
    /// mark where it was and the next poll starts from the same place.
    ///
    /// A repeated cursor ends the drain. `hasNextPage: true` with the cursor it
    /// was given back would refetch one page forever — no event reaches the
    /// gateway and the request count has no bound — so the page that repeats a
    /// cursor is kept and the connection is treated as finished, which is what
    /// `linear_api::pagination::paginate` does.
    async fn poll(&self) -> Result<Vec<Value>> {
        let mut nodes = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page: CommentsPage = self
                .client
                .comments_since(&self.mark, cursor.as_deref())
                .await?;
            nodes.extend(page.nodes);
            if !page.has_next_page || page.end_cursor.is_none() {
                break;
            }
            if page.end_cursor == cursor {
                break;
            }
            cursor = page.end_cursor;
        }
        // Draining the window makes this independent of page order; sorting
        // ascending then lets the ingest advance the mark node by node.
        nodes.sort_by(|left, right| {
            compare(
                update::updated_at(left).unwrap_or_default(),
                update::updated_at(right).unwrap_or_default(),
            )
        });
        Ok(nodes)
    }

    /// Record one poll's outcome: advance the mark, skip boundary duplicates
    /// and this backend's own comments, and queue the rest.
    fn ingest(&mut self, nodes: Vec<Value>) {
        let mut fresh = Vec::new();
        for node in nodes {
            let Some(id) = node.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(updated) = update::updated_at(&node) else {
                continue;
            };
            let newer = {
                let order = compare(updated, &self.mark);
                if order == std::cmp::Ordering::Less {
                    continue;
                }
                // The filter is inclusive, so a comment at exactly the mark
                // comes back until the mark moves past it.
                if order == std::cmp::Ordering::Equal && self.at_mark.contains(id) {
                    continue;
                }
                order == std::cmp::Ordering::Greater
            };
            if newer {
                // A new newest timestamp starts a new boundary; the ids kept
                // for the previous one can never be asked about again.
                self.at_mark.clear();
                self.mark = updated.to_string();
            }
            if self.echoes.iter().any(|(echo, _)| echo == id) {
                continue;
            }
            self.at_mark.insert(id.to_string());
            fresh.push(node);
        }
        // A comment answered without an `updatedAt` keeps its echo entry:
        // dropping it would let that one comment come back as an event, and
        // `ECHO_CAP` already bounds the set.
        self.echoes.retain(|(_, updated)| {
            updated.is_empty() || compare(updated, &self.mark) != std::cmp::Ordering::Less
        });
        self.queue.extend(fresh);
    }

    /// Remember one comment this backend created, so the poll skips it.
    fn remember_echo(&mut self, id: &str, updated_at: Option<&str>) {
        if self.echoes.len() >= ECHO_CAP {
            self.echoes.pop_front();
        }
        self.echoes
            .push_back((id.to_string(), updated_at.unwrap_or_default().to_string()));
    }

    /// The body one payload becomes: its text, then one markdown link per
    /// attachment. Uploads happen before any comment is created, so a file
    /// Linear will not take is an error rather than a dead link in a message
    /// that already went out.
    async fn body_of(&self, payload: &ImPayload) -> Result<String> {
        let rendered = render::render(payload)?;
        if rendered.attachments.is_empty() {
            return Ok(rendered.body);
        }
        let mut lines = Vec::new();
        for attachment in &rendered.attachments {
            let url = match attachment {
                render::Attachment::Url { url, .. } => url.clone(),
                render::Attachment::Local { path, label } => self
                    .client
                    .upload(path)
                    .await
                    .with_context(|| format!("uploading {label} from {path}"))?,
            };
            let label = match attachment {
                render::Attachment::Url { label, .. } | render::Attachment::Local { label, .. } => {
                    label
                }
            };
            lines.push(render::link_line(label, &url));
        }
        let links = lines.join("\n");
        if rendered.body.is_empty() {
            Ok(links)
        } else {
            Ok(format!("{}\n\n{links}", rendered.body))
        }
    }
}

#[async_trait]
impl Backend for LinearBackend {
    fn hello(&self) -> HelloArgs {
        // Linear has no credential-less mode: an empty key is a backend that
        // cannot do anything, and saying so here is what lets the gateway print
        // one actionable sentence instead of idling silently.
        let authenticated = !self.client.key().trim().is_empty();
        HelloArgs {
            platform: self.platform.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            // Comments carry no buttons, so `action` stays off and the core
            // never aims a press at this backend; there is no typing indicator
            // on an issue either.
            capabilities: vec!["message".into()],
            authenticated,
            onboarding: (!authenticated).then(|| Self::key_prompt(self.problem.as_deref())),
        }
    }

    /// Verify a pasted key with `viewer` and answer the name it belongs to.
    async fn login(&mut self, args: LoginArgs) -> Result<Option<String>> {
        let key = args.credential.trim();
        if key.is_empty() {
            bail!("the API key is empty");
        }
        // The candidate key is verified on its own client and adopted only
        // after Linear accepts it: a refused login must leave a working
        // backend working, not polling with a key the platform just rejected.
        let candidate = self.client.with_key(key);
        let viewer = candidate
            .viewer()
            .await
            .context("Linear refused this API key; check it under Security & access")?;
        self.client = candidate;
        // A credential change can mean another workspace: start a fresh window
        // rather than comparing against the old one's timestamps.
        self.mark = now();
        self.at_mark.clear();
        self.echoes.clear();
        self.queue.clear();
        self.problem = None;
        Ok(viewer.name.or(Some(viewer.id)))
    }

    /// The issues this key can see, most recently updated first.
    ///
    /// A Linear "conversation" is an issue: comments attach to an issue and
    /// there is no chat id in between.
    async fn list_conversations(&mut self) -> Result<Vec<ImConversation>> {
        let issues = self.client.recent_issues(ISSUE_LIST_SIZE).await?;
        Ok(issues
            .into_iter()
            .map(|issue| ImConversation {
                title: Some(format!("{} {}", issue.identifier, issue.title)),
                id: issue.id,
                kind: Some("issue".to_string()),
            })
            .collect())
    }

    async fn next_event(&mut self) -> Result<Option<ImEvent>> {
        loop {
            if let Some(comment) = self.queue.pop_front() {
                if let Some(event) =
                    update::event_from_comment(&self.gateway, &self.platform, &comment)
                {
                    return Ok(Some(event));
                }
                continue;
            }
            // Wait out the interval since the last attempt. A failed poll still
            // counts as an attempt, so a platform outage is retried at the
            // configured pace instead of in a tight loop.
            if let Some(last) = self.last_poll {
                if let Some(remaining) = self.interval.checked_sub(last.elapsed()) {
                    tokio::time::sleep(remaining).await;
                }
            }
            // Poll errors do not end the stream: a dropped connection is what
            // `health` reports as `reconnecting`, and a backend whose event
            // stream ends is a backend the gateway loses for good.
            match self.poll().await {
                Ok(nodes) => {
                    self.last_poll = Some(Instant::now());
                    self.last_success = self.last_poll;
                    self.last_error = None;
                    self.ingest(nodes);
                }
                Err(error) => {
                    tracing::warn!("linear poll failed: {error:#}");
                    self.last_poll = Some(Instant::now());
                    self.last_error = Some(format!("{error:#}"));
                }
            }
        }
    }

    async fn send(&mut self, args: SendArgs) -> Result<SentArgs> {
        // A local file that cannot be uploaded fails the send before
        // `commentCreate`, so a reader never gets a message with a dead link.
        let body = self.body_of(&args.payload).await?;
        let comment = self
            .client
            .create_comment(&args.conversation, &body, args.payload.reply_to.as_deref())
            .await
            .with_context(|| format!("commenting on {}", args.conversation))?;
        // Linear's typed create answers the new comment without its
        // `updatedAt`, so the echo entry carries an empty timestamp and is
        // pruned by the cap rather than by the mark.
        self.remember_echo(&comment.id, None);
        Ok(SentArgs {
            conversation: args.conversation,
            message_id: Some(comment.id),
            // One id per comment on Linear: a parent is named by id in the
            // create call, not by a second handle the platform hands back.
            ref_handle: None,
            error: None,
        })
    }

    fn health(&self) -> HealthArgs {
        let last = match self.last_success {
            Some(at) => format!("last successful poll {}s ago", at.elapsed().as_secs()),
            None => "no successful poll yet".to_string(),
        };
        let detail = match &self.last_error {
            Some(error) => format!("{last}; interval {}s; {error}", self.interval.as_secs()),
            None => format!("{last}; interval {}s", self.interval.as_secs()),
        };
        HealthArgs {
            state: if self.last_error.is_some() {
                "reconnecting".to_string()
            } else {
                "online".to_string()
            },
            detail: Some(detail),
            uptime_s: self.started.elapsed().as_secs(),
        }
    }
}

/// The poll interval, from `ONLYNE_LINEAR_POLL_SECS`; a missing, unparseable,
/// or zero value falls back to the default rather than polling in a tight loop
/// or refusing to start.
fn poll_secs_from_env() -> u64 {
    poll_secs(std::env::var(POLL_SECS_ENV).ok())
}

/// The parsed interval, falling back on anything that is not a positive number.
fn poll_secs(value: Option<String>) -> u64 {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_POLL_SECS)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Order two Linear timestamps as instants.
///
/// Comparing the strings would depend on Linear always emitting the same
/// precision and offset; parsing costs little at a page of comments and makes
/// `2026-10-17T10:00:00Z` and `2026-10-17T10:00:00.000Z` the same instant,
/// which is what the inclusive `gte` boundary assumes.
fn compare(left: &str, right: &str) -> std::cmp::Ordering {
    match (
        chrono::DateTime::parse_from_rfc3339(left),
        chrono::DateTime::parse_from_rfc3339(right),
    ) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        _ => left.cmp(right),
    }
}

/// Build the backend from the environment and run the standard protocol loop.
pub async fn run(gateway: String, token: String, base: Option<String>) -> Result<()> {
    let explicit = (!token.trim().is_empty()).then_some(token);
    onlyne_im_backend::serve(LinearBackend::from_env(explicit, gateway, base)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use fake::ok_json;
    use onlyne_im_backend::payload::{ImAttachment, ImFormat, ImPayload};

    fn page(body: &str) -> String {
        ok_json(body)
    }

    fn comment(id: &str, updated: &str, body: &str) -> String {
        format!(
            r#"{{"id":"{id}","body":"{body}","createdAt":"{updated}","updatedAt":"{updated}",
                "issue":{{"id":"issue-1","identifier":"ENG-1","title":"One"}},
                "user":{{"id":"u1","name":"Ada"}},"parent":null}}"#
        )
    }

    fn backend(base: &str) -> LinearBackend {
        LinearBackend::new(Client::new("lin_api_test").with_base(base), "gw")
    }

    /// A poll serves every page, advances the mark only after the last page,
    /// and reports each comment exactly once across the inclusive boundary.
    /// The first page reports `hasNextPage`, so a drain that stopped at a page
    /// cap would lose `b` below the advanced mark — the regression this pins.
    #[tokio::test]
    async fn polling_drains_pages_and_does_not_repeat_the_boundary() {
        let fake = fake::serve(vec![
            page(&format!(
                r#"{{"data":{{"comments":{{"nodes":[{}],"pageInfo":{{"hasNextPage":true,"endCursor":"c1"}}}}}}}}"#,
                comment("a", "2026-10-17T10:00:00.000Z", "one")
            )),
            page(&format!(
                r#"{{"data":{{"comments":{{"nodes":[{}],"pageInfo":{{"hasNextPage":false,"endCursor":"c2"}}}}}}}}"#,
                comment("b", "2026-10-17T10:00:01.000Z", "two")
            )),
            // The next poll: the mark is inclusive, so the server hands back
            // `b` again; only `c` is new.
            page(&format!(
                r#"{{"data":{{"comments":{{"nodes":[{},{}],"pageInfo":{{"hasNextPage":false,"endCursor":"c2"}}}}}}}}"#,
                comment("b", "2026-10-17T10:00:01.000Z", "two"),
                comment("c", "2026-10-17T10:00:02.000Z", "three")
            )),
        ])
        .await;
        let mut backend = backend(&fake.graphql_url());
        backend.mark = "2026-10-17T09:00:00.000Z".to_string();

        let nodes = backend.poll().await.unwrap();
        backend.ingest(nodes);
        assert_eq!(backend.queue.len(), 2, "both pages are drained");
        assert_eq!(backend.mark, "2026-10-17T10:00:01.000Z");

        let nodes = backend.poll().await.unwrap();
        backend.ingest(nodes);
        let ids: Vec<&str> = backend
            .queue
            .iter()
            .map(|node| node["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["a", "b", "c"], "b is not served twice");
        assert_eq!(backend.mark, "2026-10-17T10:00:02.000Z");
    }

    /// A window larger than any page cap is drained whole, and every id
    /// reaches the gateway. 25 pages is the count that a 20-page cap would
    /// have truncated.
    #[tokio::test]
    async fn a_window_larger_than_twenty_pages_is_drained_whole() {
        let mut responses = Vec::new();
        for index in 0..25 {
            let updated = format!("2026-10-17T10:00:{:02}.000Z", index);
            responses.push(fake::ok_json(&format!(
                r#"{{"data":{{"comments":{{"nodes":[{}],"pageInfo":{{"hasNextPage":{},"endCursor":"p{index}"}}}}}}}}"#,
                comment(&format!("c{index}"), &updated, "line"),
                index < 24
            )));
        }
        let fake = fake::serve(responses).await;
        let mut backend = backend(&fake.graphql_url());
        backend.mark = "2026-10-17T09:00:00.000Z".to_string();

        let nodes = backend.poll().await.unwrap();
        backend.ingest(nodes);
        let served: Vec<String> = backend
            .queue
            .iter()
            .map(|node| node["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(served.len(), 25, "every page's comment arrived");
        assert_eq!(served.first().map(String::as_str), Some("c0"));
        assert_eq!(served.last().map(String::as_str), Some("c24"));
        assert_eq!(fake.requests().len(), 25, "every page was fetched");
        assert_eq!(
            backend.mark, "2026-10-17T10:00:24.000Z",
            "the mark advances to the window's last comment"
        );
    }

    /// A cold start has no backlog: the window begins when the backend is
    /// constructed, not at the beginning of the workspace.
    #[tokio::test]
    async fn the_first_poll_starts_at_now_and_finds_nothing_old() {
        let fake = fake::serve(vec![page(
            r#"{"data":{"comments":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}"#,
        )])
        .await;
        let before = now();
        let mut backend = backend(&fake.graphql_url());
        let nodes = backend.poll().await.unwrap();
        backend.ingest(nodes);
        assert!(backend.queue.is_empty());
        let mark = backend.mark.clone();
        assert!(mark.as_str() >= before.as_str(), "{mark} vs {before}");
        let request = &fake.requests()[0];
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["variables"]["mark"], mark);
    }

    /// One page of comments with an explicit cursor and `hasNextPage`.
    fn page_of(id: &str, updated: &str, cursor: &str, has_next: bool) -> String {
        page(&format!(
            r#"{{"data":{{"comments":{{"nodes":[{}],"pageInfo":{{"hasNextPage":{has_next},"endCursor":"{cursor}"}}}}}}}}"#,
            comment(id, updated, "line")
        ))
    }

    /// A drain is bounded by the server's cursors, not by how many answers a
    /// broken server is willing to give: a `hasNextPage: true` that hands back
    /// the cursor it was asked with ends the connection, and what was fetched
    /// before that point still reaches the gateway.
    ///
    /// The repeating row queues eight answers and must stop after two. A drain
    /// without the guard consumes all eight and then fails on the server's
    /// empty answer — or, against a server that keeps repeating, never returns
    /// at all, which is the shape that issued tens of thousands of requests
    /// and delivered no event.
    #[tokio::test]
    async fn a_drain_stops_on_a_repeated_cursor_and_still_serves_what_it_fetched() {
        let stuck = |id: &str, updated: &str| page_of(id, updated, "stuck", true);
        let cases: Vec<(&str, Vec<String>, usize, Vec<&str>)> = vec![
            (
                "a cursor that never advances ends the drain",
                vec![
                    stuck("a", "2026-10-17T10:00:01.000Z"),
                    stuck("b", "2026-10-17T10:00:02.000Z"),
                    stuck("c", "2026-10-17T10:00:03.000Z"),
                    stuck("d", "2026-10-17T10:00:04.000Z"),
                    stuck("e", "2026-10-17T10:00:05.000Z"),
                    stuck("f", "2026-10-17T10:00:06.000Z"),
                    stuck("g", "2026-10-17T10:00:07.000Z"),
                    stuck("h", "2026-10-17T10:00:08.000Z"),
                ],
                2,
                vec!["a", "b"],
            ),
            (
                "advancing cursors still drain to the end",
                vec![
                    page_of("a", "2026-10-17T10:00:01.000Z", "p1", true),
                    page_of("b", "2026-10-17T10:00:02.000Z", "p2", true),
                    page_of("c", "2026-10-17T10:00:03.000Z", "p3", false),
                ],
                3,
                vec!["a", "b", "c"],
            ),
            (
                "hasNextPage without a cursor ends the drain",
                vec![page(&format!(
                    r#"{{"data":{{"comments":{{"nodes":[{}],"pageInfo":{{"hasNextPage":true,"endCursor":null}}}}}}}}"#,
                    comment("a", "2026-10-17T10:00:01.000Z", "line")
                ))],
                1,
                vec!["a"],
            ),
        ];

        for (label, responses, requests, served) in cases {
            let fake = fake::serve(responses).await;
            let mut backend = backend(&fake.graphql_url());
            backend.mark = "2026-10-17T09:00:00.000Z".to_string();
            let nodes = backend
                .poll()
                .await
                .unwrap_or_else(|error| panic!("{label}: {error:#}"));
            backend.ingest(nodes);
            let ids: Vec<String> = backend
                .queue
                .iter()
                .map(|node| node["id"].as_str().unwrap().to_string())
                .collect();
            assert_eq!(ids, served, "{label}: delivered comments");
            assert_eq!(fake.requests().len(), requests, "{label}: requests issued");
        }
    }

    /// The backend's own comments are skipped on the way back in, and a
    /// comment from anyone else on the same issue is still served.
    #[tokio::test]
    async fn the_backends_own_comment_does_not_come_back_as_an_event() {
        let fake = fake::serve(vec![page(&format!(
            r#"{{"data":{{"comments":{{"nodes":[{},{}],"pageInfo":{{"hasNextPage":false,"endCursor":null}}}}}}}}"#,
            comment("mine", "2026-10-17T10:00:01.000Z", "the bot's answer"),
            comment("theirs", "2026-10-17T10:00:01.000Z", "a human's reply")
        ))])
        .await;
        let mut backend = backend(&fake.graphql_url());
        backend.mark = "2026-10-17T09:00:00.000Z".to_string();
        backend.remember_echo("mine", Some("2026-10-17T10:00:01.000Z"));

        let nodes = backend.poll().await.unwrap();
        backend.ingest(nodes);
        let ids: Vec<&str> = backend
            .queue
            .iter()
            .map(|node| node["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["theirs"]);
    }

    /// Sending renders the text, uploads a local attachment, and asks Linear
    /// for a comment whose id is then the message id.
    #[tokio::test]
    async fn sending_comments_on_the_issue_and_returns_the_comment_id() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.txt");
        tokio::fs::write(&path, b"bytes").await.unwrap();

        let fake = fake::serve(vec![]).await;
        fake.push(page(&format!(
            r#"{{"data":{{"fileUpload":{{"success":true,"uploadFile":{{
                "uploadUrl":"{}",
                "assetUrl":"https://files.linear.app/asset-1",
                "headers":[]
            }}}}}}}}"#,
            fake.upload_url()
        )));
        fake.push(ok_json(""));
        fake.push(fake::created("c10"));

        let mut backend = backend(&fake.graphql_url());
        let sent = backend
            .send(SendArgs {
                conversation: "issue-1".to_string(),
                payload: ImPayload {
                    text: "here it is".to_string(),
                    format: ImFormat::Markdown,
                    attachments: vec![ImAttachment {
                        kind: "file".into(),
                        path: path.to_str().unwrap().to_string(),
                        alt: Some("the report".into()),
                    }],
                    ..ImPayload::default()
                },
            })
            .await
            .unwrap();
        assert_eq!(sent.message_id.as_deref(), Some("c10"));
        assert_eq!(sent.conversation, "issue-1");

        let requests = fake.requests();
        assert_eq!(requests.len(), 3);
        let create: Value = serde_json::from_str(&requests[2].body).unwrap();
        let input = &create["variables"]["input"];
        assert_eq!(input["issueId"], "issue-1");
        assert_eq!(
            input["body"],
            "here it is\n\n- [the report](https://files.linear.app/asset-1)"
        );
        assert!(input.get("parentId").is_none());

        // The comment is now suppressed when the poll reads it back.
        assert_eq!(backend.echoes.len(), 1);
        assert_eq!(backend.echoes[0].0, "c10");
        assert_eq!(
            backend.echoes[0].1, "",
            "the typed create answers no updatedAt, so the entry is capped, not marked"
        );
    }

    /// A reply names its parent comment, which is the only thread coordinate
    /// Linear actually has.
    #[tokio::test]
    async fn a_reply_names_the_parent_comment() {
        let fake = fake::serve(vec![fake::created("c11")]).await;
        let mut backend = backend(&fake.graphql_url());
        backend
            .send(SendArgs {
                conversation: "issue-1".to_string(),
                payload: ImPayload {
                    text: "answering".to_string(),
                    format: ImFormat::Plain,
                    reply_to: Some("c9".to_string()),
                    ..ImPayload::default()
                },
            })
            .await
            .unwrap();
        let create: Value = serde_json::from_str(&fake.requests()[0].body).unwrap();
        assert_eq!(create["variables"]["input"]["parentId"], "c9");
    }

    /// An upload failure is a send error naming the file, and no comment is
    /// created — not a message with a dead link.
    #[tokio::test]
    async fn a_failed_upload_names_the_file_and_sends_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.txt");
        tokio::fs::write(&path, b"bytes").await.unwrap();
        let fake = fake::serve(vec![page(
            r#"{"data":{"fileUpload":{"success":false,"uploadFile":null}}}"#,
        )])
        .await;
        let mut backend = backend(&fake.graphql_url());
        let error = backend
            .send(SendArgs {
                conversation: "issue-1".to_string(),
                payload: ImPayload {
                    text: "see attached".to_string(),
                    format: ImFormat::Markdown,
                    attachments: vec![ImAttachment {
                        kind: "file".into(),
                        path: path.to_str().unwrap().to_string(),
                        alt: Some("the report".into()),
                    }],
                    ..ImPayload::default()
                },
            })
            .await
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("the report"), "{message}");
        assert_eq!(
            fake.requests().len(),
            1,
            "the upload was attempted and no comment was created"
        );
    }

    #[tokio::test]
    async fn list_conversations_reports_issues_with_their_identifier() {
        let fake = fake::serve(vec![page(&format!(
            r#"{{"data":{{"issues":{{"nodes":[{},{}],
                    "pageInfo":{{"hasNextPage":false,"hasPreviousPage":false,
                    "startCursor":null,"endCursor":null}}}}}}}}"#,
            fake::issue("i1", "ENG-7", "Ship the gateway"),
            fake::issue("i2", "ENG-8", "Fix the poll")
        ))])
        .await;
        let mut backend = backend(&fake.graphql_url());
        let rows = backend.list_conversations().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "i1");
        assert_eq!(rows[0].title.as_deref(), Some("ENG-7 Ship the gateway"));
        assert_eq!(rows[0].kind.as_deref(), Some("issue"));
    }

    #[test]
    fn hello_declares_messages_only_and_asks_for_a_key_when_missing() {
        let mut backend = LinearBackend::new(Client::new(""), "gw");
        let hello = backend.hello();
        assert_eq!(hello.platform, "linear");
        assert_eq!(hello.capabilities, vec!["message".to_string()]);
        assert!(!hello.authenticated);
        assert!(hello.onboarding.unwrap().payload.contains(API_KEY_ENV));

        backend.client = Client::new("lin_api_x");
        let hello = backend.hello();
        assert!(hello.authenticated);
        assert!(hello.onboarding.is_none());
    }

    /// A refused login leaves a working backend working: the key the platform
    /// just rejected is never adopted, so polling does not turn into a stream
    /// of auth failures under it.
    #[tokio::test]
    async fn a_refused_login_keeps_the_working_key() {
        let fake = fake::serve(vec![page(
            r#"{"errors":[{"message":"Authentication failed"}]}"#,
        )])
        .await;
        let mut backend = backend(&fake.graphql_url());
        let working = backend.client.key().to_string();
        let mark_before = backend.mark.clone();

        let error = backend
            .login(LoginArgs {
                gateway: "gw".to_string(),
                credential: "lin_api_wrong".to_string(),
            })
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("Authentication failed"),
            "{error:#}"
        );
        assert_eq!(backend.client.key(), working, "the working key survived");
        assert_eq!(backend.mark, mark_before, "the cursor was not reset");
        assert_eq!(
            fake.requests()[0].authorization.as_deref(),
            Some("lin_api_wrong"),
            "the candidate key is what was checked"
        );
    }

    /// An accepted login adopts the key and starts a fresh window.
    #[tokio::test]
    async fn an_accepted_login_adopts_the_key_and_resets_the_window() {
        let fake = fake::serve(vec![fake::viewer("u9", "Grace")]).await;
        let mut backend = backend(&fake.graphql_url());
        backend.mark = "2026-10-17T09:00:00.000Z".to_string();
        backend.at_mark.insert("old".to_string());
        backend.remember_echo("c-old", Some("2026-10-17T09:00:00.000Z"));

        let identity = backend
            .login(LoginArgs {
                gateway: "gw".to_string(),
                credential: "lin_api_good".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(identity.as_deref(), Some("Grace"));
        assert_eq!(backend.client.key(), "lin_api_good");
        assert!(backend.at_mark.is_empty());
        assert!(backend.echoes.is_empty());
        assert_ne!(backend.mark, "2026-10-17T09:00:00.000Z");
    }

    /// Health names the last attempt and the configured interval, and says
    /// `reconnecting` while the last poll failed.
    #[test]
    fn health_reports_the_poll_pace_and_a_failure() {
        let mut backend = LinearBackend::new(Client::new("key"), "gw");
        let health = backend.health();
        assert_eq!(health.state, "online");
        let detail = health.detail.unwrap();
        assert!(detail.contains("interval 10s"), "{detail}");
        assert!(detail.contains("no successful poll yet"), "{detail}");

        backend.last_success = Some(Instant::now());
        backend.last_error = Some("connection refused".to_string());
        let health = backend.health();
        assert_eq!(health.state, "reconnecting");
        let detail = health.detail.unwrap();
        assert!(detail.contains("last successful poll"), "{detail}");
        assert!(detail.contains("connection refused"), "{detail}");
    }

    /// A zero or unparseable interval falls back instead of polling flat out.
    #[test]
    fn the_interval_falls_back_on_junk() {
        assert_eq!(
            poll_secs(Some(" 3 ".into())),
            3,
            "a configured interval is honored"
        );
        assert_eq!(poll_secs(Some("0".into())), DEFAULT_POLL_SECS);
        assert_eq!(poll_secs(Some("soon".into())), DEFAULT_POLL_SECS);
        assert_eq!(poll_secs(None), DEFAULT_POLL_SECS);
    }

    /// The mark comparison treats two spellings of one instant as equal, so
    /// the inclusive boundary dedupe does not depend on Linear's precision.
    #[test]
    fn timestamps_compare_as_instants() {
        assert_eq!(
            compare("2026-10-17T10:00:00Z", "2026-10-17T10:00:00.000Z"),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            compare("2026-10-17T10:00:00.001Z", "2026-10-17T10:00:00.000Z"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare("2026-10-17T09:59:59.999Z", "2026-10-17T10:00:00.000Z"),
            std::cmp::Ordering::Less
        );
    }
}
