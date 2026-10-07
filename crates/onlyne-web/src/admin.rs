//! The web's one admin link: it owns the admin connection, and nothing else.
//!
//! This is the same task the TUI's `io.rs` is (`crates/onlyne-cli/src/tui/`),
//! on the same seam: the five admin reads into one snapshot, slice 4's
//! streaming `subscribe`, and the resync a lagging subscriber makes. The two
//! differences are what it does with what it learns — it publishes the folded
//! [`View`] for the HTTP surface instead of drawing it — and who asks for ops:
//! each HTTP op dials a connection of its own ([`exchange`]), so this task
//! carries no action channel at all.
//!
//! The fold itself is never written here. Every state change goes through
//! [`onlyne_proto::view::snapshot_to_view`] or [`onlyne_proto::view::update`],
//! which is what keeps the web on the one reducer the TUI is on; a reader that
//! wants proof can compare `/api/view` against those functions applied to the
//! same frames, which is exactly what `tests/one_reducer.rs` does.

use onlyne_proto::view::{is_resync_lag, Snapshot, View};
use onlyne_proto::{
    AdminOp, ErrorCode, EventRow, EventTier, Frame, Frame as ClientFrame, LedgerEntry, LedgerQuery,
    Principal, QueryFaultsArgs, QueryRolesArgs, QuerySessionsArgs, ResBody, Subscribe,
};

use onlyne_wire::socket::{connect_local, LocalStream};
use onlyne_wire::FrameReader;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::{timeout, Instant};

/// How long a link whose stream dropped waits before dialing again — the same
/// 250 ms the TUI and `onlyne watch --follow` wait, because all three recover
/// the same way.
const RECONNECT_PAUSE: Duration = Duration::from_millis(250);

/// How many rows each read of the snapshot keeps, matching the TUI's first
/// frame: the snapshot is the resync, the subscription is what stays current.
const SNAPSHOT_LIMIT: u32 = 200;

/// How long a row the stream minted waits before its causality is read off the
/// ledger. A burst of sends settles into one read, and the pause is short
/// enough that a single operator action is traced while the operator watches.
const HEAL_PAUSE: Duration = Duration::from_millis(250);

/// Whether this event replaced the spec, so the registry must be re-read.
///
/// A `spec_reloaded` event carries the new counts and hash but no role list, so
/// the fold alone cannot learn a role's new routes — `onlyne_proto::view::update`
/// says exactly that at its own arm. This is the sibling of `is_resync_lag`:
/// both name the events this link answers by re-reading rather than by folding,
/// and the loop below checks them in one place so a third such event is added
/// to one match rather than two.
fn is_spec_reload(event: &onlyne_proto::Event) -> bool {
    matches!(event, onlyne_proto::Event::SpecReloaded(_))
}

/// Whether this event just left its row without the family the browser's trace
/// is drawn from.
///
/// A `ledger_state` event is a transition and carries no causality, so a row
/// this view had never read arrives with `family` empty. A row the snapshot
/// already read keeps the family it was read with, whatever the event says —
/// and the fold never clears it — so this is true only for the rows a
/// transition minted, which are exactly the rows owed one ledger read.
fn needs_heal(view: &View, event: &onlyne_proto::Event) -> bool {
    let onlyne_proto::Event::LedgerState(reported) = event else {
        return false;
    };
    view.deliveries
        .get(&reported.msg_id)
        .is_some_and(|delivery| delivery.family.is_none())
}

/// What the link task last said about the admin connection. `View::stale` is
/// the third fact — the link is up and events were lost — and stays the
/// reducer's, exactly as it is for the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkStatus {
    Live,
    Offline(String),
}

impl LinkStatus {
    /// The word the HTTP surface carries; the reason travels with it when the
    /// link is down, because that sentence is what an operator reads next.
    pub fn as_json(&self) -> Value {
        match self {
            LinkStatus::Live => Value::String("live".into()),
            LinkStatus::Offline(why) => Value::String(format!("offline: {why}")),
        }
    }
}

/// The one state the HTTP surface reads: the folded view, the stream cursor it
/// got there, and whether the link is up.
#[derive(Debug, Clone)]
pub struct LinkState {
    pub view: View,
    pub cursor: u64,
    pub link: LinkStatus,
}

/// A running link task. Dropping it ends the task.
#[derive(Clone)]
pub struct Link {
    state: watch::Receiver<Arc<LinkState>>,
}

impl Link {
    /// The current state, cheap to borrow and cheap to clone.
    pub fn current(&self) -> Arc<LinkState> {
        self.state.borrow().clone()
    }

    /// Wait until the state moves. `false` when every sender is gone, which is
    /// the shutdown signal.
    pub async fn changed(&mut self) -> bool {
        self.state.changed().await.is_ok()
    }
}

/// Start the link task against an admin socket.
pub fn spawn(socket: PathBuf, timeout_ms: u64) -> Link {
    let (tx, state) = watch::channel(Arc::new(LinkState {
        view: View::default(),
        cursor: 0,
        link: LinkStatus::Offline("connecting".into()),
    }));
    tokio::spawn(run(socket, timeout_ms, tx));
    Link { state }
}

/// The task's whole life: connect, read, subscribe, fold, publish.
async fn run(socket: PathBuf, timeout_ms: u64, publish: watch::Sender<Arc<LinkState>>) {
    let mut cursor: u64 = 0;
    let mut subscription: Option<Subscription> = None;
    let mut ready_at = Instant::now();
    // When a row born on the stream is due its one ledger read, if one is
    // pending. `None` means idle, and a fresh snapshot has already filled
    // every row it read, so the reconnect clears it with the view.
    let mut heal_at: Option<Instant> = None;
    let mut view = View::default();
    loop {
        tokio::select! {
            frame = next_frame(&mut subscription, timeout_ms), if subscription.is_some() => {
                match frame {
                    Ok(Frame::Ev { seq, event }) => {
                        // A gap and a reload each need the registry the event
                        // does not carry, and each is healed the same way: fold
                        // the event for what it does say, then re-read. A gap's
                        // `seq` is a drop count rather than a cursor, so only
                        // the event's own arm may move the cursor, and the
                        // reload waits no pause — a spec the operator just
                        // changed should not sit unread while a link sleeps.
                        let gap = is_resync_lag(&event);
                        if !gap {
                            cursor = cursor.max(seq);
                        }
                        view = onlyne_proto::view::update(view, &event);
                        // The reload replaced the spec, and the event carries
                        // its counts and hash but no role list — the fold
                        // cannot read a registry it has never been handed
                        // (`onlyne_proto::view::update` says so at that arm).
                        // So the registry is re-read the way a gap is healed:
                        // the snapshot is what knows the new routes, and a
                        // board that kept the old ones would offer an edge the
                        // spec no longer allows.
                        let reread = gap || is_spec_reload(&event);
                        // A `ledger_state` event is a transition: it carries no
                        // family, no hop and no clock, so a row this view had
                        // never read arrives with all eight of them empty and
                        // the browser's family trace has nothing to draw.
                        // The ledger owns those columns, so the row is owed one
                        // read — armed only when none is pending, so a burst of
                        // sends costs one read rather than one per event, and
                        // so the deadline is never pushed out from under a
                        // heal that is already due.
                        if needs_heal(&view, &event) && heal_at.is_none() {
                            heal_at = Some(Instant::now() + HEAL_PAUSE);
                        }
                        send(&publish, view.clone(), cursor, LinkStatus::Live);
                        if reread {
                            subscription = None;
                            // The snapshot this reconnect is about to take
                            // fills every row it reads, so a pending heal is
                            // the snapshot's work now, not this loop's.
                            heal_at = None;
                            ready_at = if gap {
                                Instant::now() + RECONNECT_PAUSE
                            } else {
                                Instant::now()
                            };
                        }
                    }
                    Ok(Frame::Bye { .. }) => {
                        send(&publish, view.clone(), cursor, LinkStatus::Offline(
                            "the server closed the stream".into(),
                        ));
                        subscription = None;
                        ready_at = Instant::now() + RECONNECT_PAUSE;
                    }
                    // A quiet stream is a quiet stream: the bound on one read
                    // is not a verdict on the link.
                    Ok(_) | Err(ExchangeError::Timeout) => {}
                    Err(error) => {
                        send(&publish, view.clone(), cursor, LinkStatus::Offline(describe(&error)));
                        subscription = None;
                        ready_at = Instant::now() + RECONNECT_PAUSE;
                    }
                }
            }
            _ = tokio::time::sleep_until(ready_at), if subscription.is_none() => {
                ready_at = Instant::now() + RECONNECT_PAUSE;
                let snapshot = match read_snapshot(&socket, timeout_ms).await {
                    Ok(snapshot) => snapshot,
                    Err(reason) => {
                        send(&publish, view.clone(), cursor, LinkStatus::Offline(reason));
                        continue;
                    }
                };
                // The cursor is the server's own head only until this link has
                // seen its first event; after that every reconnect resumes
                // from where the board got to.
                if cursor == 0 {
                    cursor = event_head(&snapshot);
                }
                view = onlyne_proto::view::snapshot_to_view(&snapshot);
                send(&publish, view.clone(), cursor, LinkStatus::Live);
                match subscribe(&socket, cursor, timeout_ms).await {
                    Ok((link, page)) => {
                        // The page is where a reload most often arrives: the
                        // snapshot above was read a moment before the page, so
                        // an operator's edit that landed in that gap is
                        // delivered here and not on the live stream. A page
                        // that carries one is therefore answered the same way
                        // the stream answers it — by re-reading — because the
                        // snapshot this view was just built from cannot know
                        // the routes the reload brought.
                        let mut reread = false;
                        for row in page {
                            cursor = cursor.max(row.seq);
                            reread |= is_resync_lag(&row.event) || is_spec_reload(&row.event);
                            view = onlyne_proto::view::update(view, &row.event);
                            // A page can carry the very events the snapshot
                            // did not include, so a row minted here is owed the
                            // same one ledger read a row minted on the live
                            // stream is.
                            if needs_heal(&view, &row.event) && heal_at.is_none() {
                                heal_at = Some(Instant::now() + HEAL_PAUSE);
                            }
                        }
                        send(&publish, view.clone(), cursor, LinkStatus::Live);
                        if reread {
                            drop(link);
                            heal_at = None;
                            ready_at = Instant::now();
                        } else {
                            subscription = Some(link);
                        }
                    }
                    Err(reason) => {
                        send(&publish, view.clone(), cursor, LinkStatus::Offline(reason));
                    }
                }
            }
            _ = tokio::time::sleep_until(heal_at.unwrap_or(Instant::now() + HEAL_PAUSE)), if heal_at.is_some() => {
                // The deadline is cleared before the read, so a failed read
                // cannot spin this branch: a heal is not retried by its own
                // timer, it is re-armed by the next ledger event that leaves a
                // row without its family. Nothing here drops the subscription
                // or replaces the view — a full re-read would throw away the
                // event tail the browser is drawing — the merge only fills the
                // columns a stream-born row is missing.
                heal_at = None;
                if let Ok(rows) = read_ledger(&socket, timeout_ms).await {
                    view.merge_ledger(&rows);
                    send(&publish, view.clone(), cursor, LinkStatus::Live);
                }
            }
        }
    }
}

/// Publish one state. A receiver that lagged misses intermediates, which is
/// what the cursor is for: the next frame it does see carries the whole view.
fn send(publish: &watch::Sender<Arc<LinkState>>, view: View, cursor: u64, link: LinkStatus) {
    let _ = publish.send(Arc::new(LinkState { view, cursor, link }));
}

/// A live subscription: the connection, and the reader that keeps its partial
/// frames across the idle timeouts between them.
struct Subscription {
    reader: FrameReader,
    stream: LocalStream,
}

/// A socket operation failed, in the same three words the TUI's footer uses.
#[derive(Debug)]
pub enum ExchangeError {
    Timeout,
    Closed,
    Wire(ErrorCode, String),
}

/// Read the five admin reads into one snapshot — the same five, in the same
/// order, the TUI reads, because the two front ends open with the same frame.
pub async fn read_snapshot(socket: &Path, timeout_ms: u64) -> Result<Snapshot, String> {
    let mut stream = connect(socket, timeout_ms)
        .await
        .map_err(|error| local_error(&error))?;
    let status = read_data(
        &mut stream,
        AdminOp::Status(Value::Object(Default::default())),
        timeout_ms,
    )
    .await?;
    let roles = read_list(
        &mut stream,
        AdminOp::Roles(QueryRolesArgs::default()),
        "roles",
        timeout_ms,
    )
    .await?;
    let sessions = read_list(
        &mut stream,
        AdminOp::Sessions(QuerySessionsArgs {
            limit: SNAPSHOT_LIMIT,
            ..QuerySessionsArgs::default()
        }),
        "sessions",
        timeout_ms,
    )
    .await?;
    let ledger = read_list(
        &mut stream,
        AdminOp::Ledger(LedgerQuery {
            limit: SNAPSHOT_LIMIT,
            ..LedgerQuery::default()
        }),
        "ledger",
        timeout_ms,
    )
    .await?;
    let faults = read_list(
        &mut stream,
        AdminOp::Faults(QueryFaultsArgs {
            open_only: true,
            limit: SNAPSHOT_LIMIT,
            ..QueryFaultsArgs::default()
        }),
        "faults",
        timeout_ms,
    )
    .await?;
    Ok(Snapshot {
        status: Some(status),
        roles,
        sessions,
        ledger,
        faults,
    })
}

/// Read the `ledger` alone — the one read a heal needs.
///
/// A row the stream minted carries no family and no clock, and the ledger owns
/// both, so a heal that wants those columns reads this and nothing else: the
/// other four reads would cost four more round trips to fill nothing. The
/// window is [`SNAPSHOT_LIMIT`], the same one the snapshot's own `ledger` read
/// uses, so a merge sees the rows the snapshot would have shown and not a
/// narrower page of them.
async fn read_ledger(socket: &Path, timeout_ms: u64) -> Result<Vec<LedgerEntry>, String> {
    let mut stream = connect(socket, timeout_ms)
        .await
        .map_err(|error| local_error(&error))?;
    read_list(
        &mut stream,
        AdminOp::Ledger(LedgerQuery {
            limit: SNAPSHOT_LIMIT,
            ..LedgerQuery::default()
        }),
        "ledger",
        timeout_ms,
    )
    .await
}

/// Open the streaming subscription from a cursor and read the page it answers
/// with, exactly as the TUI does: durable and advisory tiers, no filters.
async fn subscribe(
    socket: &Path,
    cursor: u64,
    timeout_ms: u64,
) -> Result<(Subscription, Vec<EventRow>), String> {
    let mut stream = connect(socket, timeout_ms)
        .await
        .map_err(|error| local_error(&error))?;
    let request = Frame::req(
        onlyne_proto::new_id(),
        AdminOp::Subscribe(Subscribe {
            since_seq: cursor,
            tiers: vec![EventTier::Durable, EventTier::Advisory],
            kinds: Vec::new(),
            roles: Vec::new(),
        }),
    );
    send_admin_frame(&mut stream, &request, timeout_ms)
        .await
        .map_err(|error| describe(&error))?;
    let mut reader = FrameReader::new();
    match recv_stream_frame(&mut reader, &mut stream, timeout_ms).await {
        Ok(Frame::Res { body, .. }) => {
            if !body.ok {
                return Err(refusal(&body));
            }
            let events = body
                .data
                .as_ref()
                .and_then(|data| data.get("events"))
                .cloned()
                .ok_or_else(|| "the subscription answer carries no page".to_string())?;
            let page = serde_json::from_value(events).map_err(|error| error.to_string())?;
            Ok((Subscription { reader, stream }, page))
        }
        Ok(other) => Err(format!(
            "expected the subscription's page, got a {} frame",
            frame_name(&other)
        )),
        Err(error) => Err(describe(&error)),
    }
}

/// Perform one admin op over a connection of its own and read its answer.
///
/// An op the browser asked for never shares the subscription's connection:
/// that connection carries frames the op cannot tell apart from its own
/// answer, so one request per connection is what keeps an answer answerable.
pub async fn exchange(socket: &Path, op: AdminOp, timeout_ms: u64) -> Result<Value, OpError> {
    let mut stream = connect(socket, timeout_ms)
        .await
        .map_err(|error| OpError::Transport(local_error(&error)))?;
    let request = Frame::req(onlyne_proto::new_id(), op);
    send_admin_frame(&mut stream, &request, timeout_ms)
        .await
        .map_err(|error| OpError::Transport(describe(&error)))?;
    let answer = recv_stream_frame(&mut FrameReader::new(), &mut stream, timeout_ms)
        .await
        .map_err(|error| OpError::Transport(describe(&error)))?;
    let body = match answer {
        Frame::Res { body, .. } => body,
        _ => return Err(OpError::Transport("expected a res frame".into())),
    };
    if !body.ok {
        let error = body.error.unwrap_or_else(refused_payload);
        return Err(OpError::Refused {
            code: error.code.as_str().to_string(),
            message: error.message,
        });
    }
    Ok(body.data.unwrap_or(Value::Null))
}

/// One admin op's answer as a refusal sentence.
fn refusal(body: &ResBody) -> String {
    match &body.error {
        Some(error) => format!("{}: {}", error.code, error.message),
        None => "refused".to_string(),
    }
}

/// The placeholder shape when a refusal carried no error payload; the wire
/// never sends one, and the arm still has to name something.
fn refused_payload() -> onlyne_proto::ErrorPayload {
    onlyne_proto::ErrorPayload {
        code: ErrorCode::Internal,
        message: "refused".into(),
        field: None,
    }
}

/// Why one exchange failed, as one answerable `OpError`.
#[derive(Debug)]
pub enum OpError {
    /// The server answered `ok = false`.
    Refused { code: String, message: String },
    /// The socket could not be reached or the answer could not be read.
    Transport(String),
}

/// Wait for the link's next frame, or for nothing at all while it is down.
async fn next_frame(
    subscription: &mut Option<Subscription>,
    timeout_ms: u64,
) -> Result<Frame, ExchangeError> {
    match subscription {
        Some(subscription) => {
            recv_stream_frame(
                &mut subscription.reader,
                &mut subscription.stream,
                timeout_ms,
            )
            .await
        }
        None => std::future::pending().await,
    }
}

/// The event cursor a snapshot's own `status` read names, which is where a
/// subscription that starts now has to resume from.
fn event_head(snapshot: &Snapshot) -> u64 {
    snapshot
        .status
        .as_ref()
        .map(onlyne_proto::view::ClusterSummary::from_status)
        .and_then(|summary| summary.event_head)
        .unwrap_or_default()
}

// ---- the wire helpers, on the seam `onlyne-cli/src/wire.rs` owns for the TUI;
// this crate cannot depend on the CLI, so the same five helpers live here.

type AdminFrame = Frame<AdminOp>;

/// Connect to the local socket, bounded by the timeout, retrying a busy pipe.
async fn connect(path: &Path, timeout_ms: u64) -> std::io::Result<LocalStream> {
    match timeout(Duration::from_millis(timeout_ms), async {
        loop {
            match connect_local(path).await {
                Ok(stream) => return Ok(stream),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::ResourceBusy
                    ) || error.raw_os_error() == Some(231) =>
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("socket timeout after {timeout_ms}ms"),
        )),
    }
}

/// Write one admin frame, bounded by the timeout.
async fn send_admin_frame(
    stream: &mut LocalStream,
    frame: &AdminFrame,
    timeout_ms: u64,
) -> Result<(), ExchangeError> {
    match timeout(
        Duration::from_millis(timeout_ms),
        onlyne_wire::write_frame(stream, frame),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(wire_error(&error)),
        Err(_) => Err(ExchangeError::Timeout),
    }
}

/// Read one frame of a stream, bounded by the timeout, through a reader that
/// keeps a partial frame's bytes across the timeouts between them.
async fn recv_stream_frame(
    reader: &mut FrameReader,
    stream: &mut LocalStream,
    timeout_ms: u64,
) -> Result<Frame, ExchangeError> {
    match timeout(
        Duration::from_millis(timeout_ms),
        reader.next::<_, ClientFrame>(stream),
    )
    .await
    {
        Ok(Ok(Some(frame))) => Ok(frame),
        Ok(Ok(None)) => Err(ExchangeError::Closed),
        Ok(Err(error)) => Err(wire_error(&error)),
        Err(_) => Err(ExchangeError::Timeout),
    }
}

/// Read one admin op's answer data, as the five snapshot reads use it.
async fn read_data(
    stream: &mut LocalStream,
    op: AdminOp,
    timeout_ms: u64,
) -> Result<Value, String> {
    let body = exchange_on(stream, op, timeout_ms).await?;
    if !body.ok {
        return Err(refusal(&body));
    }
    Ok(body.data.unwrap_or(Value::Null))
}

/// One request on an existing connection, answered by its `res` frame.
async fn exchange_on(
    stream: &mut LocalStream,
    op: AdminOp,
    timeout_ms: u64,
) -> Result<ResBody, String> {
    let request = Frame::req(onlyne_proto::new_id(), op);
    send_admin_frame(stream, &request, timeout_ms)
        .await
        .map_err(|error| describe(&error))?;
    match recv_stream_frame(&mut FrameReader::new(), stream, timeout_ms).await {
        Ok(Frame::Res { body, .. }) => Ok(body),
        Ok(other) => Err(format!(
            "expected a res frame, got a {} frame",
            frame_name(&other)
        )),
        Err(error) => Err(describe(&error)),
    }
}

/// Read one admin read's row list out of its answer.
async fn read_list<T: DeserializeOwned>(
    stream: &mut LocalStream,
    op: AdminOp,
    key: &str,
    timeout_ms: u64,
) -> Result<Vec<T>, String> {
    let data = read_data(stream, op, timeout_ms).await?;
    let rows = data
        .get(key)
        .cloned()
        .ok_or_else(|| format!("the answer carries no {key}"))?;
    serde_json::from_value(rows).map_err(|error| format!("{key}: {error}"))
}

fn frame_name(frame: &Frame) -> &'static str {
    match frame {
        Frame::Req { .. } => "req",
        Frame::Res { .. } => "res",
        Frame::Ev { .. } => "ev",
        Frame::Ack { .. } => "ack",
        Frame::Ping { .. } => "ping",
        Frame::Pong { .. } => "pong",
        Frame::Bye { .. } => "bye",
    }
}

fn wire_error(error: &std::io::Error) -> ExchangeError {
    let code = match error.kind() {
        std::io::ErrorKind::InvalidInput => ErrorCode::FrameTooLarge,
        std::io::ErrorKind::InvalidData => ErrorCode::BadFrame,
        _ => ErrorCode::Internal,
    };
    ExchangeError::Wire(code, error.to_string())
}

fn describe(error: &ExchangeError) -> String {
    match error {
        ExchangeError::Timeout => "the socket timed out".to_string(),
        ExchangeError::Closed => "the link closed".to_string(),
        ExchangeError::Wire(_, message) => message.clone(),
    }
}

fn local_error(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            format!("no server at the socket: {error}")
        }
        _ => error.to_string(),
    }
}

/// The role a principal addresses — the board a delivery belongs to.
pub fn principal_role(principal: &Principal) -> Option<&str> {
    principal.role_name()
}
