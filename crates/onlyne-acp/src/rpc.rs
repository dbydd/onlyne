//! The connection: the reader thread, the background writer thread, the stderr
//! drain, the pending-request map, and the event fan-out.
//!
//! Threading is the whole design, so the ownership rules are stated once here:
//!
//! * **one thread reads.** `read_loop` is the only code that decodes an inbound
//!   frame and the only thread that holds the `Sender` half of a pending
//!   response. That is what makes a parked caller wake when the agent dies: an
//!   `mpsc::Receiver` unblocks on a send *or* when every sender is dropped, and
//!   the reader drops them all on its way out. No timeout is needed to notice a
//!   dead agent, and this crate invents none.
//! * **one thread owns the child.** `read_loop` takes the `Child` handle at spawn
//!   and reaps it when the stdout stream ends, so `wait` is called from exactly
//!   one place, `Event::Exited` is emitted exactly once, and `Exited` always
//!   names a real exit status rather than a signal this process hoped would work.
//!   Every other thread reaches the process by pid only.
//! * **one thread writes.** `write_loop` owns the child's stdin. Frames go
//!   through an unbounded channel so a caller never blocks on a slow pipe, and
//!   dropping the last sender is what closes stdin and lets a well-behaved agent
//!   leave on its own.
//! * **the reader never blocks.** Every hand-off out of it is an unbounded
//!   `try_send`: it cannot fill, and a `Disconnected` result means the *sink* is
//!   gone, so the sink is removed and the message still goes to whoever remains.
//!   A caller that stopped reading its events must be able to wedge its own turn,
//!   never another session's.
//!
//! Locks here recover from poisoning instead of reporting it. Every critical
//! section is a map or vector operation with no user code in it, the structures
//! stay valid after a panic, and the alternative — a permanently deaf agent
//! handle because one thread died elsewhere — turns a local failure into a
//! crate-wide one.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

use crate::types::{AgentCapabilities, Event, PermissionOutcome, PermissionRequest, Update};
use crate::wire::{self, FrameRead, LineReader, Message, RequestId, RpcError};

/// How a caller's parked request is answered.
pub(crate) type Reply = std::result::Result<Value, RpcError>;

/// Lines kept from the agent's stderr for the `Exited` detail. Enough to show why
/// a process refused to start, small enough to hold for every agent process.
const STDERR_TAIL_LINES: usize = 40;
/// One stderr line is logged and retained no longer than this; an agent that
/// dumps a stack trace as a single line cannot grow this buffer.
const STDERR_LINE_CAP: usize = 4 * 1024;

/// `session/request_permission` is the one agent-to-client request with an
/// answer this crate models. Everything else gets `-32601`.
pub(crate) const PERMISSION_REQUEST: &str = "session/request_permission";

pub(crate) struct Conn {
    /// Monotonic, so an id is never reused and two threads cannot register the
    /// same key. Starts at 1; 0 is reserved as "not a request".
    next_id: AtomicI64,
    pending: Mutex<HashMap<RequestId, Sender<Reply>>>,
    /// The writer thread's inbox. `None` once it has been closed for teardown or
    /// after the child's stdin rejected a frame.
    out: Mutex<Option<Sender<String>>>,
    sinks: Mutex<Vec<Sender<Event>>>,
    /// Session ids this process handed out, so a stale id fails here rather than
    /// at the agent.
    sessions: Mutex<HashSet<String>>,
    /// The negotiated handshake, and with it the fact that there was one.
    pub(crate) initialized: OnceLock<AgentCapabilities>,
    /// Set once, by the reaper, with the exit detail. Its presence is how every
    /// other thread learns the agent is gone.
    exit: Mutex<Option<String>>,
    stderr_tail: Mutex<VecDeque<String>>,
}

/// Take a lock, recovering from poisoning; see the module note.
fn guard<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

impl Conn {
    pub(crate) fn new(out: Sender<String>) -> Arc<Conn> {
        Arc::new(Conn {
            next_id: AtomicI64::new(1),
            pending: Mutex::new(HashMap::new()),
            out: Mutex::new(Some(out)),
            sinks: Mutex::new(Vec::new()),
            sessions: Mutex::new(HashSet::new()),
            initialized: OnceLock::new(),
            exit: Mutex::new(None),
            stderr_tail: Mutex::new(VecDeque::new()),
        })
    }

    // ---------------------------------------------------------------- ids

    fn alloc_id(&self) -> RequestId {
        RequestId::Number(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Ids of the requests this client still has outstanding. The pairing for
    /// [`crate::Agent::cancel_request`]: an abandoned `session/prompt` can only
    /// be cancelled by the id it was sent with, and a caller cannot know that id
    /// while the call is parked.
    pub(crate) fn outstanding(&self) -> Vec<RequestId> {
        guard(&self.pending).keys().cloned().collect()
    }

    fn take_pending(&self, id: &RequestId) -> Option<Sender<Reply>> {
        guard(&self.pending).remove(id)
    }

    /// Forget every parked request. Dropping the senders is what wakes their
    /// callers, so this must run on any path where no response can arrive again.
    fn drain_pending(&self, reason: &str) {
        let abandoned = std::mem::take(&mut *guard(&self.pending));
        if !abandoned.is_empty() {
            tracing::warn!(
                count = abandoned.len(),
                reason,
                "acp: failing parked requests"
            );
        }
    }

    // ----------------------------------------------------------- lifecycle

    pub(crate) fn exited(&self) -> bool {
        guard(&self.exit).is_some()
    }

    pub(crate) fn exit_detail(&self) -> Option<String> {
        guard(&self.exit).clone()
    }

    /// Record the exit of the agent process, first writer only: park no caller
    /// any longer, replay the fact to late subscribers, and tell the live ones.
    pub(crate) fn record_exit(&self, detail: String) {
        {
            let mut exit = guard(&self.exit);
            if exit.is_some() {
                return;
            }
            *exit = Some(detail.clone());
        }
        self.drain_pending("the agent process exited");
        let mut sinks = guard(&self.sinks);
        let mut live = 0usize;
        // std mpsc has no liveness probe, so the send itself is the test: a
        // receiver that is gone answers `Err` and is dropped from the list.
        sinks.retain(|sender| {
            let sent = sender
                .send(Event::Exited {
                    detail: detail.clone(),
                })
                .is_ok();
            live += usize::from(sent);
            sent
        });
        tracing::info!(subscribers = live, %detail, "acp: agent process ended");
    }

    pub(crate) fn require_live(&self, verb: &str) -> Result<()> {
        match self.exit_detail() {
            Some(detail) => bail!("acp: cannot {verb}: {detail}"),
            None => Ok(()),
        }
    }

    /// The protocol requires the handshake before any session work. The escape
    /// hatches (`cancel`, `cancel_request`, `answer`) stay open: a wedged
    /// `initialize` is exactly the case a caller must be able to abort.
    pub(crate) fn require_initialized(&self, verb: &str) -> Result<()> {
        self.require_live(verb)?;
        if self.initialized.get().is_some() {
            return Ok(());
        }
        bail!(
            "acp: cannot {verb}: initialize has not completed on this agent process \
             (an agent that never answers initialize is shut down with Agent::shutdown)"
        )
    }

    pub(crate) fn require_session(&self, verb: &str, session_id: &str) -> Result<()> {
        if guard(&self.sessions).contains(session_id) {
            return Ok(());
        }
        bail!(
            "acp: cannot {verb} for session {session_id}: this agent process never \
             reported that id from session/new"
        )
    }

    /// `session/request_permission` is a *request*: the agent parks its turn
    /// until the client answers. A caller that has not subscribed cannot see the
    /// request, so the turn would be answered by this crate's fallback rather
    /// than by the caller's policy. Saying so up front is the difference between
    /// a caught bug and a silently declined permission.
    ///
    /// `std::sync::mpsc::Sender` has no liveness probe, so "still listening" is
    /// only known at the next fan-out; this gate catches the case it exists for —
    /// a caller that never subscribed — and the fallback plus its warning covers
    /// a receiver subscribed and then dropped.
    pub(crate) fn require_subscriber(&self, verb: &str) -> Result<()> {
        if guard(&self.sinks).is_empty() {
            bail!(
                "acp: cannot {verb} before subscribe(): a permission request would be \
                 answered by the no-subscriber fallback instead of by the caller"
            );
        }
        Ok(())
    }

    // ------------------------------------------------------------ sessions

    pub(crate) fn add_session(&self, session_id: String) {
        guard(&self.sessions).insert(session_id);
    }

    pub(crate) fn remove_session(&self, session_id: &str) {
        guard(&self.sessions).remove(session_id);
    }

    // -------------------------------------------------------------- output

    pub(crate) fn subscribe(&self) -> Receiver<Event> {
        let (sender, receiver) = mpsc::channel();
        let mut sinks = guard(&self.sinks);
        if let Some(detail) = guard(&self.exit).as_ref() {
            let _ = sender.send(Event::Exited {
                detail: detail.clone(),
            });
        }
        sinks.push(sender);
        receiver
    }

    /// Fan an event out, dropping sinks whose receiver is gone. Returns whether
    /// any live receiver took it, which is how the permission fallback decides
    /// that nobody is watching. An unbounded `send` never blocks, so the reader
    /// thread cannot be stalled by a subscriber that stopped reading.
    fn emit(&self, event: Event) -> bool {
        let mut sinks = guard(&self.sinks);
        let mut delivered = false;
        sinks.retain(|sender| {
            let sent = sender.send(event.clone()).is_ok();
            delivered |= sent;
            sent
        });
        delivered
    }

    fn send_frame(&self, line: String) -> Result<()> {
        let mut out = guard(&self.out);
        let Some(sender) = out.as_ref() else {
            bail!("acp: this agent's stdin is closed; the process is being shut down")
        };
        if sender.send(line).is_err() {
            *out = None;
            bail!("acp: the writer thread for this agent has stopped");
        }
        Ok(())
    }

    /// Close the write half, which is the graceful part of shutdown: an agent
    /// treats end-of-stdin as "the client left".
    pub(crate) fn close_output(&self) {
        drop(guard(&self.out).take());
    }

    // ------------------------------------------------------------- requests

    /// Send a request and park until the answer, the agent dies, or the agent
    /// answers with an error. There is deliberately no deadline here: this crate
    /// detects, and the supervisor decides how long a turn gets.
    pub(crate) fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.require_live(method)?;
        let id = self.alloc_id();
        let (sender, receiver) = mpsc::channel();
        guard(&self.pending).insert(id.clone(), sender);
        // The registration guard below removes this entry on every path out of
        // this function, a panic included, which is what bounds the map by the
        // set of live callers without a liveness probe this API does not offer.
        let _registered = PendingRegistration {
            conn: self,
            id: id.clone(),
        };
        self.send_frame(wire::encode_request(&id, method, params))?;
        match receiver.recv() {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => {
                Err(anyhow::Error::new(error).context(format!("acp: the agent refused {method}")))
            }
            Err(mpsc::RecvError) => Err(anyhow!(match self.exit_detail() {
                Some(detail) => format!("acp: {method} was never answered; {detail}"),
                None => format!(
                    "acp: {method} was never answered; the agent closed the protocol \
                     stream (id {id})"
                ),
            })),
        }
    }

    pub(crate) fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send_frame(wire::encode_notification(method, params))
    }

    pub(crate) fn reply(&self, id: &RequestId, result: Value) -> Result<()> {
        self.send_frame(wire::encode_result(id, result))
    }

    pub(crate) fn reply_error(&self, id: &RequestId, error: &RpcError) -> Result<()> {
        self.send_frame(wire::encode_failure(id, error))
    }
}

/// Removes a pending entry on every exit path, including a panic in the caller.
/// Ids come from a monotonic counter and are never reused, so this cannot evict a
/// registration that belongs to someone else.
struct PendingRegistration<'a> {
    conn: &'a Conn,
    id: RequestId,
}

impl Drop for PendingRegistration<'_> {
    fn drop(&mut self) {
        self.conn.take_pending(&self.id);
    }
}

// ------------------------------------------------------------------ inbound

pub(crate) fn route(conn: &Conn, message: Message) {
    match message {
        Message::Request { id, method, params } => handle_request(conn, id, &method, &params),
        Message::Notification { method, params } => handle_notification(conn, &method, &params),
        Message::Result { id, result } => deliver(conn, id, Ok(result)),
        Message::Failure { id, error } => deliver(conn, id, Err(error)),
    }
}

fn deliver(conn: &Conn, id: RequestId, reply: Reply) {
    match conn.take_pending(&id) {
        Some(sender) => {
            if sender.send(reply).is_err() {
                tracing::debug!(%id, "acp: response for an abandoned request was dropped");
            }
        }
        None => tracing::debug!(
            %id,
            "acp: response for a request this client is not holding (answered late or \
             already cancelled)"
        ),
    }
}

fn handle_request(conn: &Conn, id: RequestId, method: &str, params: &Value) {
    if method != PERMISSION_REQUEST {
        tracing::debug!(
            method,
            "acp: declining an agent request this client cannot serve"
        );
        let _ = conn.reply_error(&id, &RpcError::method_not_found(method));
        return;
    }
    let request = PermissionRequest::parse(id, params);
    if conn.emit(Event::Permission(request.clone())) {
        return;
    }
    // Nothing is listening. Parking the agent forever would make one client-side
    // ordering mistake look like an agent hang, and deciding on the caller's
    // behalf is not this crate's call to make, so the answer is ACP's own "the
    // user did not decide": the agent declines the action and finishes the turn.
    tracing::warn!(
        request_id = %request.request_id,
        session_id = %request.session_id,
        "acp: permission request answered cancelled because nobody has subscribed"
    );
    let _ = conn.reply(
        &request.request_id,
        PermissionOutcome::Cancelled.to_result(),
    );
}

fn handle_notification(conn: &Conn, method: &str, params: &Value) {
    if method != "session/update" {
        tracing::debug!(method, "acp: ignoring an unmodelled agent notification");
        return;
    }
    let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else {
        tracing::warn!("acp: session/update arrived without a usable sessionId");
        return;
    };
    let update = Update::from_params(params);
    if !conn.emit(Event::Update {
        session_id: session_id.to_string(),
        update,
    }) {
        tracing::trace!(session_id, "acp: no subscriber for a session/update");
    }
}

// ----------------------------------------------------------------- threads

/// How long the exit report waits for the stderr drain to finish. The child's
/// write end of the pipe closes when it exits, so the drain reaches end of file on
/// its own; the bound is only for a descendant that inherited stderr and is still
/// alive. Without the wait, the last lines an agent wrote before dying — usually
/// the reason it died — race the report and are usually missing from it.
const STDERR_DRAIN_WAIT: Duration = Duration::from_millis(250);

/// Read frames until the stream ends, then reap the child and announce it.
pub(crate) fn read_loop(
    stdout: ChildStdout,
    mut child: Child,
    conn: Arc<Conn>,
    label: String,
    stderr_drained: Receiver<()>,
) {
    let reader_conn = Arc::clone(&conn);
    let reader_label = label.clone();
    let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        read_frames(stdout, &reader_conn, &reader_label)
    }));
    if let Err(payload) = read {
        tracing::error!(
            "acp: the frame reader panicked ({}); reaping the agent",
            panic_text(&payload)
        );
    }
    // The only `wait` in this crate. It blocks until the process is gone, which
    // is the fact `Event::Exited` is allowed to assert.
    let detail = match child.wait() {
        Ok(status) => format!("{label} exited ({status})"),
        Err(error) => format!("{label} could not be reaped ({error})"),
    };
    let _ = stderr_drained.recv_timeout(STDERR_DRAIN_WAIT);
    conn.record_exit(with_stderr(&detail, &conn.stderr_snapshot()));
}

fn read_frames(stdout: ChildStdout, conn: &Arc<Conn>, label: &str) {
    let mut reader = LineReader::new(stdout, wire::MAX_LINE_BYTES);
    loop {
        match reader.next() {
            FrameRead::Eof => return,
            FrameRead::Failed(error) => {
                tracing::warn!("acp: {label} stdout read failed: {error}");
                return;
            }
            FrameRead::Line { text, truncated } => {
                if truncated {
                    tracing::error!(
                        limit = wire::MAX_LINE_BYTES,
                        "acp: {label} sent a frame longer than the limit; a stream that \
                         cannot be resynchronised is ended, not guessed at"
                    );
                    return;
                }
                if text.trim().is_empty() {
                    continue;
                }
                match wire::decode(&text) {
                    Ok(message) => route(conn, message),
                    Err(error) => tracing::warn!("acp: {label} sent a bad frame: {error}"),
                }
            }
        }
    }
}

/// Write frames to the child's stdin until the channel closes, then leave the
/// write end so the agent sees end-of-stdin.
pub(crate) fn write_loop(mut stdin: ChildStdin, inbox: Receiver<String>, conn: Arc<Conn>) {
    while let Ok(line) = inbox.recv() {
        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        let written = stdin.write_all(&bytes).and_then(|()| stdin.flush());
        if let Err(error) = written {
            // The agent is gone or no longer reading. Nothing can answer a
            // parked caller now, so wake them; `read_loop` still owns the reap.
            tracing::debug!("acp: the agent's stdin rejected a frame: {error}");
            conn.close_output();
            conn.drain_pending("the agent closed its stdin");
            return;
        }
    }
}

/// Drain stderr to `tracing` forever. stdout carries protocol frames only, so a
/// chatty agent must not be able to fill the pipe and block itself.
pub(crate) fn drain_stderr(mut stderr: ChildStderr, conn: Arc<Conn>) {
    let mut reader = LineReader::new(&mut stderr, STDERR_LINE_CAP);
    loop {
        match reader.next() {
            FrameRead::Eof => return,
            FrameRead::Failed(error) => {
                tracing::trace!("acp: agent stderr read failed: {error}");
                return;
            }
            FrameRead::Line {
                mut text,
                truncated,
            } => {
                if truncated {
                    text.push_str("…[truncated]");
                }
                conn.note_stderr(&text);
                tracing::debug!(target: "onlyne_acp::agent_stderr", "{}", text.trim_end());
            }
        }
    }
}

fn panic_text(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "unknown payload".to_string()
}

fn with_stderr(detail: &str, tail: &[String]) -> String {
    if tail.is_empty() {
        return detail.to_string();
    }
    format!("{detail}\nagent stderr:\n{}", tail.join("\n"))
}

impl Conn {
    fn note_stderr(&self, line: &str) {
        let mut tail = guard(&self.stderr_tail);
        if tail.len() == STDERR_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line.to_string());
    }

    fn stderr_snapshot(&self) -> Vec<String> {
        guard(&self.stderr_tail).iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn conn() -> (Arc<Conn>, Receiver<String>) {
        let (sender, inbox) = mpsc::channel();
        (Conn::new(sender), inbox)
    }

    #[test]
    fn a_numeric_id_and_its_string_spelling_are_not_the_same_request() {
        let (conn, _inbox) = conn();
        let (number_sender, _number_receiver) = mpsc::channel();
        let (text_sender, _text_receiver) = mpsc::channel();
        {
            let mut pending = guard(&conn.pending);
            pending.insert(RequestId::Number(1), number_sender);
            pending.insert(RequestId::Text("1".to_string()), text_sender);
        }

        assert_eq!(conn.outstanding().len(), 2);
        route(
            &conn,
            Message::Result {
                id: RequestId::Number(1),
                result: json!({"settled": "number"}),
            },
        );

        assert_eq!(
            conn.outstanding(),
            vec![RequestId::Text("1".to_string())],
            "an answer for 1 must not settle the request spelled \"1\""
        );
    }

    #[test]
    fn ids_come_from_one_monotonic_counter() {
        let (conn, _inbox) = conn();

        assert_eq!(conn.alloc_id(), RequestId::Number(1));
        assert_eq!(conn.alloc_id(), RequestId::Number(2));
        assert_eq!(conn.alloc_id(), RequestId::Number(3));
    }

    #[test]
    fn a_parked_caller_wakes_when_the_agent_dies() {
        let (conn, inbox) = conn();
        let caller = {
            let conn = Arc::clone(&conn);
            std::thread::spawn(move || conn.request("session/prompt", json!({})))
        };
        while conn.outstanding().is_empty() {
            std::thread::yield_now();
        }
        conn.record_exit("fake agent exited (signal: 9, SIGKILL)".to_string());

        let error = caller
            .join()
            .unwrap()
            .expect_err("death must fail the request");
        assert!(
            error.to_string().contains("signal: 9"),
            "the parked caller is told why: {error}"
        );
        assert!(conn.outstanding().is_empty(), "no entry survives the drain");
        drop(inbox);
    }

    #[test]
    fn the_response_is_routed_to_its_own_request() {
        let (conn, inbox) = conn();
        let caller = {
            let conn = Arc::clone(&conn);
            std::thread::spawn(move || conn.request("initialize", json!({})))
        };
        while conn.outstanding().is_empty() {
            std::thread::yield_now();
        }
        let line = inbox.recv().unwrap();
        let frame: Value = serde_json::from_str(&line).unwrap();
        let echoed = RequestId::from_value(&frame["id"]).unwrap();
        route(
            &conn,
            Message::Result {
                id: echoed,
                result: json!({"protocolVersion": 1}),
            },
        );

        assert_eq!(
            caller.join().unwrap().unwrap(),
            json!({"protocolVersion": 1})
        );
        assert!(conn.outstanding().is_empty());
    }

    #[test]
    fn an_agent_error_is_handed_back_as_a_typed_rpc_error() {
        let (conn, inbox) = conn();
        let caller = {
            let conn = Arc::clone(&conn);
            std::thread::spawn(move || conn.request("session/new", json!({})))
        };
        while conn.outstanding().is_empty() {
            std::thread::yield_now();
        }
        let frame: Value = serde_json::from_str(&inbox.recv().unwrap()).unwrap();
        let id = RequestId::from_value(&frame["id"]).unwrap();
        route(
            &conn,
            Message::Failure {
                id,
                error: RpcError {
                    code: RpcError::AUTH_REQUIRED,
                    message: "login first".to_string(),
                    data: Some(json!({"methodId": "qoderclicn-login"})),
                },
            },
        );

        let error = caller.join().unwrap().expect_err("refused");
        let rpc = error
            .downcast_ref::<RpcError>()
            .expect("the error survives anyhow's context wrapper");
        assert_eq!(rpc.code, RpcError::AUTH_REQUIRED);
        assert_eq!(
            rpc.data.as_ref().and_then(|data| data.get("methodId")),
            Some(&json!("qoderclicn-login")),
            "data is what tells two -32000 errors apart"
        );
    }

    #[test]
    fn an_unknown_reverse_request_is_answered_method_not_found_naming_the_method() {
        let (conn, inbox) = conn();
        route(
            &conn,
            Message::Request {
                id: RequestId::Number(77),
                method: "terminal/output".to_string(),
                params: json!({}),
            },
        );

        let line = inbox.recv().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap(),
            json!({
                "jsonrpc": "2.0",
                "id": 77,
                "error": {"code": -32601, "message": "client does not implement terminal/output"},
            })
        );
    }

    #[test]
    fn a_permission_request_with_no_subscriber_is_cancelled_not_parked() {
        let (conn, inbox) = conn();
        route(
            &conn,
            Message::Request {
                id: RequestId::Text("perm-1".to_string()),
                method: PERMISSION_REQUEST.to_string(),
                params: json!({"sessionId": "s1", "options": []}),
            },
        );

        assert_eq!(
            serde_json::from_str::<Value>(&inbox.recv().unwrap()).unwrap(),
            json!({
                "jsonrpc": "2.0",
                "id": "perm-1",
                "result": {"outcome": {"outcome": "cancelled"}},
            })
        );
    }

    #[test]
    fn a_permission_request_reaches_a_subscriber_and_stops_being_ours_to_answer() {
        let (conn, inbox) = conn();
        let events = conn.subscribe();

        route(
            &conn,
            Message::Request {
                id: RequestId::Number(5),
                method: PERMISSION_REQUEST.to_string(),
                params: json!({"sessionId": "s1", "toolCall": {"toolCallId": "t"}
                                , "options": [{"optionId": "no", "kind": "reject_once", "name": "No"}]}),
            },
        );

        let Event::Permission(request) = events.recv().unwrap() else {
            panic!("a permission request must surface as Event::Permission");
        };
        assert_eq!(request.session_id, "s1");
        assert_eq!(request.request_id, RequestId::Number(5));
        assert_eq!(request.options[0].option_id, "no");
        assert!(
            inbox.try_recv().is_err(),
            "with a subscriber the client answers nothing on its own"
        );
    }

    #[test]
    fn the_subscriber_gate_tracks_the_receivers_this_crate_handed_out() {
        let (conn, _inbox) = conn();
        let error = conn.require_subscriber("session/prompt").unwrap_err();
        assert!(error.to_string().contains("subscribe()"), "{error}");

        let dropped = conn.subscribe();
        conn.require_subscriber("session/prompt")
            .expect("a receiver exists now");
        drop(dropped);
        // std mpsc has no liveness probe, so the next fan-out is what learns the
        // receiver is gone; the gate closes then, not sooner.
        route(
            conn.as_ref(),
            Message::Notification {
                method: "session/update".to_string(),
                params: json!({"sessionId": "s1", "sessionUpdate": "plan", "entries": []}),
            },
        );
        assert!(conn.require_subscriber("session/prompt").is_err());
    }

    #[test]
    fn a_dropped_sink_does_not_stop_the_live_one_receiving_the_stream() {
        let (conn, _inbox) = conn();
        let gone = conn.subscribe();
        let alive = conn.subscribe();
        drop(gone);

        route(
            conn.as_ref(),
            Message::Notification {
                method: "session/update".to_string(),
                params: json!({"sessionId": "s1", "sessionUpdate": "agent_message_chunk",
                               "content": {"type": "text", "text": "hi"}}),
            },
        );

        match alive.recv().unwrap() {
            Event::Update { session_id, update } => {
                assert_eq!(session_id, "s1");
                assert_eq!(update.text(), Some("hi"));
            }
            other => panic!("expected an update, got {other:?}"),
        }
        assert_eq!(guard(&conn.sinks).len(), 1, "the dead sink was pruned");
    }

    #[test]
    fn session_work_refuses_an_uninitialised_process_and_an_unknown_session() {
        let (conn, inbox) = conn();
        let events = conn.subscribe();

        let error = conn.require_initialized("session/new").unwrap_err();
        assert!(error.to_string().contains("initialize"), "{error}");
        // The escape hatches stay open even before the handshake.
        assert!(conn.require_live("session/cancel").is_ok());

        conn.initialized
            .set(AgentCapabilities::default())
            .expect("first initialize");
        assert!(conn.require_initialized("session/new").is_ok());
        assert!(
            conn.require_session("session/prompt", "s1")
                .unwrap_err()
                .to_string()
                .contains("never reported that id")
        );
        conn.add_session("s1".to_string());
        assert!(conn.require_session("session/prompt", "s1").is_ok());
        conn.remove_session("s1");
        assert!(conn.require_session("session/prompt", "s1").is_err());
        drop(events);
        drop(inbox);
    }

    #[test]
    fn closing_the_output_refuses_further_frames() {
        let (conn, inbox) = conn();
        conn.notify("session/cancel", json!({"sessionId": "s1"}))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&inbox.recv().unwrap()).unwrap(),
            json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "s1"}})
        );

        conn.close_output();
        assert!(conn.notify("session/cancel", json!({})).is_err());
    }

    #[test]
    fn a_late_subscriber_is_told_the_agent_already_left() {
        let (conn, inbox) = conn();
        conn.record_exit("fake agent exited (exit status: 1)".to_string());

        let events = conn.subscribe();
        let event = events.recv().unwrap();
        assert!(
            matches!(&event, Event::Exited { detail } if detail.contains("exit status: 1")),
            "{event:?}"
        );
        assert!(conn.request("initialize", json!({})).is_err());
        drop(inbox);
    }

    #[test]
    fn stderr_tail_keeps_only_the_last_window() {
        let (conn, inbox) = conn();
        for index in 0..(STDERR_TAIL_LINES + 25) {
            conn.note_stderr(&format!("line {index}"));
        }

        let tail = conn.stderr_snapshot();
        assert_eq!(tail.len(), STDERR_TAIL_LINES);
        assert_eq!(tail[0], format!("line {}", 25));
        assert_eq!(
            tail[STDERR_TAIL_LINES - 1],
            format!("line {}", STDERR_TAIL_LINES + 24)
        );
        assert!(with_stderr("detail", &tail).contains("agent stderr:"));
        assert_eq!(with_stderr("detail", &[]), "detail");
        drop(inbox);
    }
}
