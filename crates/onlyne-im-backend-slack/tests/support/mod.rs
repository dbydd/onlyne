//! A fake Slack Web API, shared by the integration tests.

use std::sync::{Arc, Mutex};

/// Serve one HTTP answer per request, in the order the requests arrive, and
/// record each request's bytes.
///
/// `Connection: close` keeps one connection per request, so the recorded order
/// is the request order.
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
            use std::io::{Read, Write};
            let mut buffer = [0u8; 8192];
            let read = socket.read(&mut buffer).unwrap_or(0);
            log.lock()
                .expect("request log")
                .push(String::from_utf8_lossy(&buffer[..read]).into_owned());
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
