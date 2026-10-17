//! A minimal HTTP/1.1 server for tests.
//!
//! Two things this crate talks to over HTTP: Linear's GraphQL endpoint and the
//! signed upload URL `fileUpload` hands back. Both are one request per
//! connection here, answered from a queue, with every request recorded so a
//! test can assert on the bytes that actually left the client. No TLS: the
//! client's endpoint is overridable, and a test asserting a request's shape
//! has nothing to learn from a handshake.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One request the server saw.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub head: String,
    pub authorization: Option<String>,
    pub body: String,
}

/// A running fake server and the requests it has seen.
pub struct Fake {
    base: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    responses: Arc<Mutex<VecDeque<String>>>,
}

impl Fake {
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    /// Queue one more response. A test uses this when a response body must name
    /// this server's own URL, which is known only after it is listening.
    pub fn push(&self, response: String) {
        self.responses.lock().unwrap().push_back(response);
    }

    /// The endpoint a [`crate::api::Client`] should be pointed at.
    pub fn graphql_url(&self) -> String {
        format!("{}/graphql", self.base)
    }

    /// A second path on the same server, for the upload step.
    pub fn upload_url(&self) -> String {
        format!("{}/upload", self.base)
    }
}

/// One HTTP response with a JSON body.
pub fn ok_json(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

/// One issue in the shape `linear_api`'s typed `Issue` decodes: every field
/// its `IssueFields` fragment selects has to be present, including the nested
/// state, team and connection objects.
pub fn issue(id: &str, identifier: &str, title: &str) -> String {
    format!(
        r#"{{"id":"{id}","identifier":"{identifier}","number":1,"title":"{title}",
            "description":null,"url":"https://linear.app/{identifier}","branchName":"{identifier}",
            "priority":0,"priorityLabel":"No priority","estimate":null,"dueDate":null,
            "sortOrder":1,"state":{{"id":"s1","name":"Todo","type":"unstarted","color":"000000"}},
            "team":{{"id":"t1","key":"ENG","name":"Engineering"}},"assignee":null,"creator":null,
            "labels":{{"nodes":[]}},"project":null,"projectMilestone":null,"parent":null,
            "relations":{{"nodes":[]}},"inverseRelations":{{"nodes":[]}},
            "createdAt":"2026-10-17T09:00:00.000Z","updatedAt":"2026-10-17T10:00:00.000Z"}}"#
    )
}

/// A `viewer` answer with every field the SDK's typed viewer query selects.
pub fn viewer(id: &str, name: &str) -> String {
    ok_json(&format!(
        r#"{{"data":{{"viewer":{{"id":"{id}","name":"{name}","displayName":"ada",
            "email":"ada@example.com","active":true,"admin":true}}}}}}"#
    ))
}

/// A `commentCreate` answer in the shape the SDK's typed `Comment` decodes.
/// It answers no `updatedAt`, because the typed mutation does not select one.
pub fn created(id: &str) -> String {
    ok_json(&format!(
        r#"{{"data":{{"commentCreate":{{"success":true,"comment":{{
            "id":"{id}","body":"sent","url":"https://linear.app/comment/{id}",
            "createdAt":"2026-10-17T10:00:05.000Z"}}}}}}}}"#
    ))
}

/// Start a server on a free port; each incoming request takes the next
/// response, and a request past the queue gets `{}`.
pub async fn serve(responses: Vec<String>) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
    let queued = Arc::clone(&responses);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let Some((head, body)) = read_request(&mut stream).await else {
                continue;
            };
            let request_line = head.lines().next().unwrap_or_default();
            let mut parts = request_line.split_whitespace();
            let method = parts.next().unwrap_or_default().to_string();
            let path = parts.next().unwrap_or_default().to_string();
            let authorization = head.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("authorization")
                    .then(|| value.trim().to_string())
            });
            recorded.lock().unwrap().push(Recorded {
                method,
                path,
                head,
                authorization,
                body,
            });
            let response = responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| ok_json("{}"));
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
            let _ = stream.shutdown().await;
        }
    });
    Fake {
        base: format!("http://{addr}"),
        requests,
        responses: queued,
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    let body_start = head_end + 4;
    while buffer.len() < body_start + length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body =
        String::from_utf8_lossy(&buffer[body_start..(body_start + length).min(buffer.len())])
            .to_string();
    Some((head, body))
}
