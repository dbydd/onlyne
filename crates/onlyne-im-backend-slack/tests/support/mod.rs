//! A fake Slack Web API, shared by the integration tests.

use std::sync::{Arc, Mutex};

/// Held for the whole of one case that opens a Socket Mode stream.
///
/// The SDK's push-events callback is a plain `fn`, so the backend finds its
/// live stream through process state (`ws::LIVE`) rather than through a
/// closure. A process therefore serves one stream at a time — which is what a
/// deployment is — and two cases that both open one, run in parallel by
/// `cargo test`, overwrite each other's registration: the second case's stream
/// takes the queue the first one's callback would have filled, and the first
/// case waits out its deadline with an event that went to the other test.
///
/// An async mutex rather than a std one: the guard is held across the whole
/// case, and a std guard across an `await` is what `clippy::await_holding_lock`
/// names. Tokio's mutex has no poisoning, so a case that panics still releases
/// it and one failure does not turn every later case into a second one.
pub async fn stream_gate() -> tokio::sync::MutexGuard<'static, ()> {
    static GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    GATE.lock().await
}

/// Serve one HTTP answer per request, in the order the requests arrive, and
/// record each request's bytes.
///
/// `Connection: close` keeps one connection per request, so the recorded order
/// is the request order.
///
/// The accepted socket is put back into blocking mode: the listener polls
/// `accept`, and on macOS the accepted socket inherits the non-blocking flag,
/// so a read here returned `WouldBlock`, which `unwrap_or(0)` recorded as an
/// empty request while the response that belonged to it went to the next
/// caller.
pub fn fake_api(responses: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the fake api");
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
            let _ = socket.set_nonblocking(false);
            use std::io::{Read, Write};
            let mut request = Vec::new();
            let mut buffer = [0u8; 8192];
            while let Ok(read) = socket.read(&mut buffer) {
                request.extend_from_slice(&buffer[..read]);
                if read == 0 || request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            log.lock()
                .expect("request log")
                .push(String::from_utf8_lossy(&request).into_owned());
            let (status, body) = responses
                .get(index)
                .cloned()
                .unwrap_or((200, r#"{"ok":false}"#.to_string()));
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

/// Every request recorded so far.
pub fn recorded(seen: &Mutex<Vec<String>>) -> Vec<String> {
    seen.lock().expect("request log").clone()
}

/// Wait, bounded, until `count` requests (or acks) have landed.
pub async fn wait_for<T>(seen: &Mutex<Vec<T>>, count: usize) {
    for _ in 0..500 {
        if seen.lock().expect("log").len() >= count {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
