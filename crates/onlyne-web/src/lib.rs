//! `onlyne-web` — the optional graphical front end.
//!
//! One model, two renderers: this crate runs the same `view` reducer the TUI
//! runs (`onlyne_proto::view`) over the same snapshot-and-subscribe seam, and
//! hands the folded view to the browser — the browser renders and sends ops,
//! nothing else (`docs/v2-PLAN.md` §"网页前端 onlyne-web").
//!
//! ```text
//! GET  /api/view      the folded view and the boards read off it
//! GET  /api/stream    SSE: the view as the stream moves it, cursor-carried
//! POST /api/op        one admin op: send, control, repair, report, spec edit
//! ```
//!
//! ## The security floor
//!
//! The admin surface can send tasks, edit the spec, and shut the cluster
//! down, so every request — assets included — passes [`guard`]: a `Host` the
//! bind actually serves, an `Origin` that is absent or one of the same names,
//! and the token minted at startup. Nothing here ever sets a CORS header.
//! The bind is loopback unless the operator passed `--bind`, which is the one
//! explicit flag that widening takes.

use crate::admin::{exchange, Link, LinkState, OpError};
use crate::ops::WebOp;
use crate::render::boards;
use axum::extract::State;
use axum::http::{header, HeaderName, StatusCode, Uri};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{middleware, Json, Router};
use futures_util::Stream;
use rust_embed::RustEmbed;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

pub mod admin;
pub mod ops;
pub mod render;

/// The static bundle `web/` builds and `build.rs` demands.
#[derive(RustEmbed)]
#[folder = "assets/dist"]
struct Assets;

/// The minted startup token, printed in the URL `--open` opens.
pub const TOKEN_BYTES: usize = 32;

/// Mint one token: `TOKEN_BYTES` random bytes as lowercase hex.
pub fn mint_token() -> String {
    let bytes: [u8; TOKEN_BYTES] = rand::random();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The shared state every handler and the guard read.
pub struct App {
    /// The admin socket the link task watches and the ops dial.
    pub socket: PathBuf,
    /// The `--timeout` every admin exchange is bounded by.
    pub timeout_ms: u64,
    /// The startup token every request must carry.
    pub token: String,
    /// The address actually bound; the host allowlist derives from it.
    pub bind: SocketAddr,
    /// The folded view and the link's own health.
    pub link: Link,
    /// The `Host` values a request may name: the bound address's own
    /// spellings, and `localhost` only when the bind serves loopback.
    pub allowed_hosts: Vec<String>,
}

impl App {
    /// Assemble the app for a socket and a bind, starting the link task.
    pub fn new(socket: PathBuf, timeout_ms: u64, token: String, bind: SocketAddr) -> Arc<Self> {
        let link = admin::spawn(socket.clone(), timeout_ms);
        Arc::new(App {
            socket,
            timeout_ms,
            token,
            bind,
            link,
            allowed_hosts: allowed_hosts(bind),
        })
    }


    /// The router, guard and all.
    pub fn router(self: &Arc<Self>) -> Router {
        let state = Arc::clone(self);
        Router::new()
            .route("/", get(index))
            .route("/api/view", get(api_view))
            .route("/api/stream", get(api_stream))
            .route("/api/op", post(api_op))
            .fallback(static_asset)
            .layer(middleware::from_fn_with_state(Arc::clone(&state), guard))
            .with_state(state)
    }
}

// ---- the guard

/// Refuse a request whose `Host` the bind does not serve, whose `Origin` is
/// foreign, or that carries no valid token — in that order, assets included,
/// and never with a CORS header.
async fn guard(
    State(app): State<Arc<App>>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    let host = header_value_text(request.headers(), &header::HOST).unwrap_or_default();
    if !app
        .allowed_hosts
        .iter()
        .any(|allowed| eq_ignore_case(allowed, &host))
    {
        return refusal(
            StatusCode::FORBIDDEN,
            &format!(
                "onlyne-web: refused host \"{host}\": only the names the bind serves reach this surface"
            ),
        );
    }
    if let Some(origin) = header_value_text(request.headers(), &header::ORIGIN) {
        let host_part = origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
            .unwrap_or("");
        if !app
            .allowed_hosts
            .iter()
            .any(|allowed| eq_ignore_case(allowed, host_part))
        {
            return refusal(
                StatusCode::FORBIDDEN,
                &format!("onlyne-web: refused origin \"{origin}\""),
            );
        }
    }
    // The favicon is the one request a browser makes that no page references
    // and no token can reach: it is asked for on its own, carrying nothing of
    // the document's query, so the check below refused it 401 on every page
    // load. 401 says "unauthenticated", which misreports what is actually true
    // — the file does not exist — and a console full of 401s buries a real
    // refusal. The bundle ships no icon, so the honest answer is the 404
    // `serve_asset` gives a missing file; an icon added to the bundle later is
    // served here without anyone editing this. Nothing is given up: an icon is
    // public, the `Host` and `Origin` names still stand above, and no API
    // route is reachable on this path.
    if request.uri().path() == "/favicon.ico" {
        return serve_asset("favicon.ico");
    }
    let offered = query_token(request.uri())
        .map(str::to_string)
        .or_else(|| bearer_token(request.headers()));
    match offered {
        Some(token) if constant_time_eq(token.as_bytes(), app.token.as_bytes()) => {
            next.run(request).await
        }
        _ => refusal(
            StatusCode::UNAUTHORIZED,
            "onlyne-web: this surface needs its startup token",
        ),
    }
}

/// The names one bind serves: its own address spellings, with and without the
/// port, and `localhost` only when the bind is loopback (or all interfaces,
/// which serve loopback too). A rebound DNS name is none of them.
pub fn allowed_hosts(bind: SocketAddr) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let ip = bind.ip().to_string();
    if ip.contains(':') {
        // IPv6's brackets are part of the host header's spelling.
        names.push(format!("[{ip}]"));
    } else {
        names.push(ip);
    }
    if bind.ip().is_loopback() || bind.ip().is_unspecified() {
        names.push("localhost".to_string());
        if bind.ip().is_loopback() {
            names.push("127.0.0.1".to_string());
        }
    }
    let port = bind.port().to_string();
    let mut with_ports = Vec::new();
    for name in &names {
        with_ports.push(format!("{name}:{port}"));
    }
    names.extend(with_ports);
    names
}

/// Whether the bind may serve this address: loopback always, anything else
/// only because the operator passed `--bind`, which is the explicit flag the
/// contract names. The refusal says what to do instead.
pub fn ensure_loopback(addr: SocketAddr, explicit: bool) -> Result<(), String> {
    if addr.ip().is_loopback() || explicit {
        return Ok(());
    }
    Err(format!(
        "onlyne-web: refusing to bind {addr}: the default bind is 127.0.0.1; pass --bind {addr} to name another one"
    ))
}

fn header_value_text(headers: &axum::http::HeaderMap, name: &HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_string)
}

fn query_token(uri: &Uri) -> Option<&str> {
    uri.query()?.split('&').find_map(|pair| {
        pair.strip_prefix("token=")
            .filter(|token| !token.is_empty())
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// One refusal as plain text: nothing negotiable, nothing scriptable.
fn refusal(status: StatusCode, body: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body.to_string(),
    )
        .into_response()
}

// ---- the API

/// The payload `/api/view` and one SSE frame carry: the cursor, the link's
/// own word, the reducer's view, and the boards read off it.
pub fn payload(state: &LinkState) -> Value {
    json!({
        "cursor": state.cursor,
        "link": state.link.as_json(),
        "view": state.view,
        "boards": boards(&state.view),
    })
}

async fn api_view(State(app): State<Arc<App>>) -> Response {
    Json(payload(app.link.current().as_ref())).into_response()
}

async fn api_stream(
    State(app): State<Arc<App>>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let cursor = query
        .as_deref()
        .and_then(|query| {
            query.split('&').find_map(|pair| {
                pair.strip_prefix("cursor=")
                    .and_then(|value| value.parse::<u64>().ok())
            })
        })
        .unwrap_or(u64::MAX);
    let link = app.link.clone();
    let stream = futures_util::stream::unfold(
        (link, cursor, true),
        |(mut link, mut seen, mut first)| async move {
            if first {
                first = false;
                let current = link.current();
                // A client resuming with its last cursor gets the current
                // frame only when it is behind; a current one waits for news.
                if seen != current.cursor {
                    seen = current.cursor;
                    let frame = frame_event(&current);
                    return Some((Ok(frame), (link, seen, first)));
                }
            }
            if !link.changed().await {
                return None;
            }
            let current = link.current();
            seen = current.cursor;
            let frame = frame_event(&current);
            Some((Ok(frame), (link, seen, first)))
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn frame_event(state: &LinkState) -> SseEvent {
    SseEvent::default()
        .event("view")
        .data(payload(state).to_string())
}

async fn api_op(State(app): State<Arc<App>>, Json(op): Json<WebOp>) -> Response {
    let admin = match crate::ops::admin_op(op) {
        Ok(admin) => admin,
        Err(reason) => {
            return error_body(StatusCode::BAD_REQUEST, "invalid", &reason);
        }
    };
    match exchange(&app.socket, admin, app.timeout_ms).await {
        // A `spec_get` used to be the moment the layout file's place was
        // learned, so its answer carried a path the browser never saw. Nothing
        // here reads a path any more: where a board sits is the tab's own
        // memory, and the spec keeps semantics.
        Ok(data) => Json(data).into_response(),
        Err(OpError::Refused { code, message }) => {
            error_body(StatusCode::CONFLICT, &code, &message)
        }
        Err(OpError::Transport(reason)) => {
            error_body(StatusCode::BAD_GATEWAY, "transport", &reason)
        }
    }
}

fn error_body(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

// ---- the bundle

async fn index(uri: Uri) -> Response {
    // The document is the one request a browser makes with the token in its
    // query. The script and the stylesheet it names are fetched afterwards, as
    // subresources, and a subresource inherits nothing from the URL that
    // pulled the document in — so an asset reference that carried no token of
    // its own is refused, and the page comes up blank. The document therefore
    // mints its asset URLs with the token it was given. The guard is untouched:
    // every request still carries a token, and an asset URL quoted without one
    // is still refused.
    let token = query_token(&uri).unwrap_or_default();
    match Assets::get("index.html") {
        Some(file) => {
            let html = String::from_utf8_lossy(&file.data).into_owned();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                rewrite_asset_urls(&html, token),
            )
                .into_response()
        }
        None => refusal(
            StatusCode::NOT_FOUND,
            "onlyne-web: the built document is missing; run `npm run build` in crates/onlyne-web/web",
        ),
    }
}

/// Append the token to every `/assets/` reference in the document.
///
/// The scan is on the path rather than on the attribute name, because a build
/// may spell a reference `src=`, `href=`, or `poster=`, and all three are
/// fetched the same way: as a subresource the guard sees with no token.
fn rewrite_asset_urls(html: &str, token: &str) -> String {
    if token.is_empty() {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len() + 64);
    let mut rest = html;
    while let Some(at) = rest.find("/assets/") {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest
            .find(|c: char| c == '"' || c == '\'' || c.is_whitespace())
            .unwrap_or(rest.len());
        out.push_str(&rest[..end]);
        out.push(if rest[..end].contains('?') { '&' } else { '?' });
        out.push_str("token=");
        out.push_str(token);
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Serve one bundled asset by the path its request names.
///
/// The handler reads the path out of the [`Uri`] rather than through axum's
/// `Path`, because this is a *fallback* route: a fallback matches no pattern,
/// so there is no route path for `Path` to extract from and every asset
/// answered 500. The guard runs first, so an asset only reaches here when the
/// request already carried the token.
async fn static_asset(uri: Uri) -> Response {
    serve_asset(uri.path().trim_start_matches('/'))
}

fn serve_asset(key: &str) -> Response {
    match Assets::get(key) {
        Some(file) => {
            let mime = mime_for(key);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime)],
                file.data.into_owned(),
            )
                .into_response()
        }
        None => refusal(
            StatusCode::NOT_FOUND,
            &format!("onlyne-web: no such asset: {key}"),
        ),
    }
}

fn mime_for(key: &str) -> &'static str {
    match key.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Serve one request against the app in-process, for the tests.
pub async fn serve_request(
    app: &Arc<App>,
    request: axum::http::Request<axum::body::Body>,
) -> Response {
    let mut router = app.router();
    use tower::Service;
    router.call(request).await.unwrap_or_else(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the request failed to route: {error}"),
        )
            .into_response()
    })
}
