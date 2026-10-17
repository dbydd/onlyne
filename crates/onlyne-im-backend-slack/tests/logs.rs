//! Stderr must not carry the minted socket ticket.
//!
//! Slack Morphism's socket client logs a failed dial at `ERROR` with the URL it
//! dialled, and that URL's `?ticket=…` is a bearer credential for a Socket Mode
//! connection. The binary's default log filter switches that module off, so
//! this test dials an address that refuses the connection and reads the
//! process's stderr — what an operator actually sees.
//!
//! The control run sets `RUST_LOG=info` explicitly and expects the ticket to
//! appear. That is what proves the first case is measuring the filter and not a
//! dial that never happened: an explicit `RUST_LOG` is the operator's own
//! choice and overrides the shipped default.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const TICKET: &str = "secret-ticket-1234";

/// An address nothing can be listening on: port 1 needs a privileged bind.
///
/// It is fixed rather than a freshly freed ephemeral port, because two tests
/// that free a port and then dial it can have the other test's fake API take
/// that port in between — and then the dial succeeds and the case under test
/// never happens.
const REFUSING_SOCKET: &str = "ws://127.0.0.1:1/link";

/// A fake Slack Web API: `apps.connections.open` answers `url`, and every other
/// call answers `{"ok":true}`.
///
/// Stops when `stop` is set; nothing here waits on a request count, because the
/// backend may ask for more than the test needs and would then block on join.
///
/// The accepted socket is put back into blocking mode and read to the end of
/// the request headers. The listener polls `accept`, and on macOS the accepted
/// socket inherits the non-blocking flag, so an unguarded read returned
/// `WouldBlock`, the request came back empty, and the mint answer went out as
/// the `auth.test` one — the SDK then dialled Slack's own URL instead of the
/// refusing socket, which is a control run that proves nothing.
fn minting_api(url: String) -> (String, Arc<AtomicBool>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the fake api");
    let addr = listener.local_addr().expect("addr");
    listener.set_nonblocking(true).expect("nonblocking");
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    std::thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            let Ok((mut socket, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            let _ = socket.set_nonblocking(false);
            let _ = socket.set_read_timeout(Some(Duration::from_millis(100)));
            let mut request = Vec::new();
            let mut buffer = [0u8; 8192];
            while let Ok(read) = socket.read(&mut buffer) {
                request.extend_from_slice(&buffer[..read]);
                if read == 0 || request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request).into_owned();
            let body = if request.contains("/apps.connections.open") {
                format!(r#"{{"ok":true,"url":"{url}"}}"#)
            } else {
                r#"{"ok":true,"user":"onlyne","user_id":"U1","team":"T1","team_id":"T1","url":"https://t1.slack.com/"}"#
                    .to_string()
            };
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(answer.as_bytes());
        }
    });
    (format!("http://{addr}"), stop)
}

/// One run: spawn the backend, hand it a login frame, wait until `settled`
/// holds on stderr or the deadline passes, then kill it and return everything
/// it wrote to stderr.
///
/// `settled` is what keeps this from measuring a fixed sleep: the suppressed
/// run waits a bounded while for any output at all, and the control waits for
/// the line it expects, so a slow machine cannot turn either into a false pass
/// or a false failure.
fn stderr_of_one_run(
    base: &str,
    rust_log: Option<&str>,
    settled: impl Fn(&str) -> bool,
    deadline: Duration,
    linger: Duration,
) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_onlyne-im-backend-slack"));
    command
        .args(["--gateway", "gw", "--token", "xapp-a:xoxb-b", "--api-base"])
        .arg(base)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    match rust_log {
        Some(filter) => command.env("RUST_LOG", filter),
        None => command.env_remove("RUST_LOG"),
    };
    let mut child: Child = command.spawn().expect("spawn the backend");

    // A `login` frame: it makes the credential accepted, which turns polling on
    // and makes the listener register — and dial — a connection.
    let login = serde_json::json!({
        "op": "login",
        "args": {"gateway": "gw", "credential": "xapp-a:xoxb-b"}
    })
    .to_string();
    let mut frame = (login.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(login.as_bytes());
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(&frame)
        .expect("write the login frame");

    // Drain stderr into a shared buffer: reading it after the kill would block
    // until the process dies, and this test cannot know how long the SDK's
    // reconnect ladder takes before then.
    let mut pipe = child.stderr.take().expect("stderr");
    let collected = Arc::new(std::sync::Mutex::new(String::new()));
    let sink = collected.clone();
    let reader = std::thread::spawn(move || {
        // Line by line, into the shared buffer as each line arrives: a
        // whole-stream read would hold everything until the process dies, and
        // the wait below would never see what it is waiting for.
        for line in BufReader::new(&mut pipe).lines() {
            let Ok(line) = line else {
                return;
            };
            let mut text = sink.lock().expect("stderr sink");
            text.push_str(&line);
            text.push('\n');
        }
    });

    let started = Instant::now();
    while started.elapsed() < deadline {
        if settled(&collected.lock().expect("stderr sink")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // The settle point is "the stream is registered"; the dial that fails
    // comes after it, so give it room to happen and be logged.
    std::thread::sleep(linger);
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    collected.lock().expect("stderr sink").clone()
}

#[test]
fn a_failed_dial_never_prints_the_socket_ticket() {
    // `apps.connections.open` still mints a URL, and the SDK's dial to it
    // fails — the path that logs the URL at ERROR.
    let (base, stop) = minting_api(format!("{REFUSING_SOCKET}/?ticket={TICKET}"));

    // Wait for the stream to be registered — the dial follows it — and then
    // linger, so what is asserted on is whatever the failed dial wrote.
    let stderr = stderr_of_one_run(
        &base,
        None,
        |text| text.contains("the slack event stream is open"),
        Duration::from_secs(20),
        Duration::from_secs(2),
    );
    assert!(
        stderr.contains("the slack event stream is open"),
        "the run never got as far as dialling, so it proves nothing:\n{stderr}"
    );
    stop.store(true, Ordering::Relaxed);

    assert!(
        !stderr.contains("ticket="),
        "a minted socket ticket reached stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains(TICKET),
        "the socket URL reached stderr:\n{stderr}"
    );
}

#[test]
fn an_explicit_rust_log_still_shows_what_it_asks_for() {
    // The control: same failing dial, but the operator asked for the SDK's own
    // logging. The ticket appears, which is what shows the suppressed run was
    // suppressed rather than a dial that never happened.
    let (base, stop) = minting_api(format!("{REFUSING_SOCKET}/?ticket={TICKET}"));

    // Wait for the SDK's own line, so a slow machine reads this as slow
    // rather than as "the override does not work".
    let stderr = stderr_of_one_run(
        &base,
        Some("info"),
        |text| text.contains(TICKET),
        Duration::from_secs(30),
        Duration::from_secs(0),
    );
    stop.store(true, Ordering::Relaxed);

    assert!(
        stderr.contains(TICKET),
        "the SDK's own ERROR line should carry the URL it dialled when RUST_LOG asks for it:\n{stderr}"
    );
}

#[test]
fn the_default_filter_switches_the_socket_client_off() {
    let filter = onlyne_im_backend_slack::DEFAULT_LOG_FILTER;
    assert!(
        filter.contains("slack_morphism::hyper_tokio::socket_mode::tungstenite_wss_client=off"),
        "{filter}"
    );
    assert!(filter.starts_with("info"), "{filter}");
}
