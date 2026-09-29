// The security floor: every refusal case the contract names, plus the CORS
// check, plus the bind guard — in a real server whose socket the tests dial.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use onlyne_proto::{AdminOp, Frame, ResBody};
use onlyne_web::{allowed_hosts, ensure_loopback, mint_token, serve_request, App};
use onlyne_wire::{write_frame, FrameReader};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use tempfile::tempdir;
use tokio::net::UnixListener;

/// A socket the test serves itself, so no external server is needed. The
/// handler answers the five snapshot reads and the subscribe with an empty
/// page, and echoes every op as `ok: true`.
mod test_server {
    use super::*;
    pub async fn run(socket: PathBuf) {
        let listener = UnixListener::bind(&socket).expect("bind test socket");
        loop {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut reader = FrameReader::new();
            loop {
                let frame = match reader.next::<_, Frame<AdminOp>>(&mut stream).await {
                    Ok(Some(frame)) => frame,
                    Ok(None) | Err(_) => break,
                };
                if let Frame::Req { id, op } = frame {
                    let body = match op {
                        AdminOp::Status(_) => ResBody::ok(json!({"event_head": 0})),
                        AdminOp::Roles(_) => ResBody::ok(json!({"roles": []})),
                        AdminOp::Sessions(_) => ResBody::ok(json!({"sessions": []})),
                        AdminOp::Ledger(_) => ResBody::ok(json!({"ledger": []})),
                        AdminOp::Faults(_) => ResBody::ok(json!({"faults": []})),
                        AdminOp::Subscribe(_) => ResBody::ok(json!({"events": []})),
                        AdminOp::SpecGet(_) => ResBody::ok(json!({
                            "path": "/nonexistent/.onlyne/spec.toml",
                            "source_hash": "abc123",
                            "spec": {}
                        })),
                        _ => ResBody::ok(Value::Null),
                    };
                    write_frame(&mut stream, &Frame::res(id, body)).await.ok();
                }
            }
        }
    }
}

/// The guard refuses a request without the token.
#[tokio::test]
async fn no_token_refused() {
    let dir = tempdir().expect("temp dir");
    let socket = dir.path().join("test.sock");
    let _server = tokio::spawn(test_server::run(socket.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let bind = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let token = mint_token();
    let app = App::new(socket, 8000, token.clone(), bind);

    let req = Request::builder()
        .uri("/api/view")
        .header(header::HOST, "127.0.0.1")
        .body(Body::empty())
        .unwrap();
    let resp = serve_request(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("this surface needs its startup token"));
}

/// The guard refuses a request with a Host the bind does not serve.
#[tokio::test]
async fn rebound_host_refused() {
    let dir = tempdir().expect("temp dir");
    let socket = dir.path().join("test.sock");
    let _server = tokio::spawn(test_server::run(socket.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let bind = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let token = mint_token();
    let app = App::new(socket, 8000, token.clone(), bind);

    let req = Request::builder()
        .uri(format!("/api/view?token={token}"))
        .header(header::HOST, "evil.rebind.example")
        .body(Body::empty())
        .unwrap();
    let resp = serve_request(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("refused host"));
}

/// The guard refuses a request with a foreign Origin.
#[tokio::test]
async fn foreign_origin_refused() {
    let dir = tempdir().expect("temp dir");
    let socket = dir.path().join("test.sock");
    let _server = tokio::spawn(test_server::run(socket.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let bind = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let token = mint_token();
    let app = App::new(socket, 8000, token.clone(), bind);

    let req = Request::builder()
        .uri(format!("/api/stream?token={token}&cursor=0"))
        .header(header::HOST, "127.0.0.1")
        .header(header::ORIGIN, "https://attacker.example")
        .body(Body::empty())
        .unwrap();
    let resp = serve_request(&app, req).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("refused origin"));
}

/// An asset request without the token is also refused — assets travel the
/// same middleware.
#[tokio::test]
async fn asset_without_token_refused() {
    let dir = tempdir().expect("temp dir");
    let socket = dir.path().join("test.sock");
    let _server = tokio::spawn(test_server::run(socket.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let bind = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let token = mint_token();
    let app = App::new(socket, 8000, token.clone(), bind);

    let req = Request::builder()
        .uri("/some.js")
        .header(header::HOST, "127.0.0.1")
        .body(Body::empty())
        .unwrap();
    let resp = serve_request(&app, req).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// The document a browser is handed must name its assets with the token.
///
/// The refusal case above is right and stays: an asset request with no token is
/// refused. But a browser fetches the script and the stylesheet as
/// *subresources* of the document, and a subresource inherits nothing from the
/// URL that pulled the document in — so references carrying no token made every
/// asset a refusal and the page came up blank. The document is therefore minted
/// with the token on its own references, which is what this asserts: the guard
/// is still the only door, and a browser can actually walk through it.
#[tokio::test]
async fn the_document_names_its_assets_with_the_token() {
    let dir = tempdir().expect("temp dir");
    let socket = dir.path().join("test.sock");
    let _server = tokio::spawn(test_server::run(socket.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let bind = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let token = mint_token();
    let app = App::new(socket, 8000, token.clone(), bind);

    let req = Request::builder()
        .uri(format!("/?token={token}"))
        .header(header::HOST, "127.0.0.1")
        .body(Body::empty())
        .unwrap();
    let resp = serve_request(&app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);

    let references: Vec<&str> = text
        .match_indices("/assets/")
        .map(|(at, _)| {
            let tail = &text[at..];
            let end = tail
                .find(|c: char| c == '"' || c == '\'' || c.is_whitespace())
                .unwrap_or(tail.len());
            &tail[..end]
        })
        .collect();
    assert!(
        !references.is_empty(),
        "the document names no asset: {text}"
    );
    for reference in &references {
        assert!(
            reference.contains(&format!("token={token}")),
            "an asset reference carries no token, so the browser is refused it: {reference}\n{text}"
        );
    }
}

/// An asset carrying the token is served, not merely admitted.
///
/// The two refusals above are the guard; this is the other half. The asset
/// route is a *fallback*, so it matches no pattern and axum's `Path` has no
/// route path to extract from — every asset answered 500, and the guard's 401
/// on the untokened form had been hiding it. A page that cannot load its own
/// script is blank, so the served body is the assertion that matters.
#[tokio::test]
async fn an_asset_with_the_token_is_served() {
    let dir = tempdir().expect("temp dir");
    let socket = dir.path().join("test.sock");
    let _server = tokio::spawn(test_server::run(socket.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let bind = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let token = mint_token();
    let app = App::new(socket, 8000, token.clone(), bind);

    // The document names its script, so the test asks for that same path rather
    // than a name it invented: a served asset is one the page really needs.
    let req = Request::builder()
        .uri(format!("/?token={token}"))
        .header(header::HOST, "127.0.0.1")
        .body(Body::empty())
        .unwrap();
    let document = serve_request(&app, req).await;
    let bytes = axum::body::to_bytes(document.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    let script = text
        .match_indices("/assets/")
        .map(|(at, _)| {
            let tail = &text[at..];
            let end = tail
                .find(|c: char| c == '"' || c == '\'' || c.is_whitespace())
                .unwrap_or(tail.len());
            tail[..end].to_string()
        })
        .find(|reference| {
            reference
                .split('?')
                .next()
                .unwrap_or_default()
                .ends_with(".js")
        })
        .expect("the document names a script");
    let script_path = script.split('?').next().unwrap().to_string();

    let req = Request::builder()
        .uri(&script_path)
        .header(header::HOST, "127.0.0.1")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let resp = serve_request(&app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "the script the document names is not served: {script_path}"
    );
    let served = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(!served.is_empty(), "the script came back empty");
}

/// No response ever carries a CORS header — the guard never adds one and
/// the static server never adds one.
#[tokio::test]
async fn no_cors_header() {
    let dir = tempdir().expect("temp dir");
    let socket = dir.path().join("test.sock");
    let _server = tokio::spawn(test_server::run(socket.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let bind = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let token = mint_token();
    let app = App::new(socket, 8000, token.clone(), bind);

    let req = Request::builder()
        .uri(format!("/api/view?token={token}"))
        .header(header::HOST, "127.0.0.1")
        .header(header::ORIGIN, "http://127.0.0.1")
        .body(Body::empty())
        .unwrap();
    let resp = serve_request(&app, req).await;
    assert!(!resp.headers().contains_key("access-control-allow-origin"));
    assert!(!resp
        .headers()
        .contains_key("access-control-allow-credentials"));
    assert!(!resp.headers().contains_key("access-control-allow-headers"));
    assert!(!resp.headers().contains_key("access-control-allow-methods"));
}

/// The bind guard refuses a non-loopback bind when the flag was not given.
#[test]
fn bind_guard_refuses_non_loopback_without_flag() {
    let addr = "192.168.1.5:8080".parse::<SocketAddr>().unwrap();
    let err = ensure_loopback(addr, false).unwrap_err();
    assert!(err.contains("refusing to bind"));
    assert!(err.contains("--bind"));
}

/// A non-loopback bind succeeds when the flag was given.
#[test]
fn bind_guard_allows_non_loopback_with_flag() {
    let addr = "192.168.1.5:8080".parse::<SocketAddr>().unwrap();
    ensure_loopback(addr, true).expect("explicit flag allows it");
}

/// The allowed hosts list contains the bound names and localhost when loopback.
#[test]
fn allowed_hosts_includes_bound_names() {
    let addr = "127.0.0.1:8787".parse::<SocketAddr>().unwrap();
    let hosts = allowed_hosts(addr);
    assert!(hosts.iter().any(|h| h == "127.0.0.1"));
    assert!(hosts.iter().any(|h| h == "localhost"));
    assert!(hosts.iter().any(|h| h == "127.0.0.1:8787"));
    assert!(hosts.iter().any(|h| h == "localhost:8787"));
}

/// The operator's board exists even when the spec does not declare _supervisor.
#[test]
fn operator_board_always_rendered() {
    // Uses only the reducer: a snapshot with three roles, no _supervisor in the
    // roles read → the boards function adds the operator board last.
    let view = onlyne_proto::view::View {
        roles: std::collections::BTreeMap::from([
            (
                "planner".into(),
                onlyne_proto::RoleInfo {
                    name: "planner".into(),
                    admin: false,
                    max_sessions: 3,
                    runtime: Default::default(),
                    spec_hash: "h1".into(),
                    prose: Some("plan".into()),
                    state: onlyne_proto::Presence::Online,
                    sessions: 1,
                    queued: 0,
                    edges: vec![],
                    detail: None,
                    aggregate: None,
                },
            ),
            (
                "builder".into(),
                onlyne_proto::RoleInfo {
                    name: "builder".into(),
                    admin: false,
                    max_sessions: 3,
                    runtime: Default::default(),
                    spec_hash: "h2".into(),
                    prose: Some("build".into()),
                    state: onlyne_proto::Presence::Online,
                    sessions: 1,
                    queued: 0,
                    edges: vec![],
                    detail: None,
                    aggregate: None,
                },
            ),
        ]),
        ..Default::default()
    };
    let boards = onlyne_web::render::boards(&view);
    assert_eq!(boards.len(), 3);
    assert_eq!(boards[2].role, "_supervisor");
    assert!(boards[2].operator);
}
