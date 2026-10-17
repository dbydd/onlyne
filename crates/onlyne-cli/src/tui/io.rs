//! The board's one IO task: it owns the admin connection, and nothing else.
//!
//! This task is the only place in the TUI that opens a socket. It reads the
//! five admin reads into one snapshot, carries slice 4's `subscribe` stream,
//! and performs the ops the screen asks for; everything it learns travels to
//! the board as an [`Event`], and the board folds it with `update`. The
//! renderer therefore draws whatever it is handed, and a page renders from a
//! `View` with no server anywhere — which is the case the contract's last
//! acceptance line names.
//!
//! ## No poll
//!
//! v1 asked the server for the same five reads once a second
//! (`docs/v2-PLAN.md` line 395). Nothing here counts time: the snapshot is read
//! once per connection, and after that every update is an event the
//! subscription pushed. Both waits are one-shot deadlines armed by an event —
//! the reconnect pause, so a server that is down is not dialed in a tight loop,
//! and the heal pause, which fires once to read the causality and terminal
//! state a transition does not carry.
//!
//! ## A gap is an input
//!
//! A lagging subscriber receives the synthetic `resync_lag` fault
//! ([`onlyne_proto::view::is_resync_lag`]). It is not news about the cluster, so
//! it is folded — which marks the view stale — and the link is dropped; the
//! next connect re-reads the snapshot, which is what clears the flag, and
//! re-subscribes from the last good cursor, whose answer page carries what the
//! broadcast dropped. That is the recovery `onlyne watch --follow` makes, and it
//! is why the screen can say it is catching up instead of drawing a state that
//! silently lost events.

use crate::tui::state::{Action, Link, Repair};
use crate::tui::update::Event;
use crate::wire::{self, ExchangeError, Outbound};
use onlyne_proto::view::Snapshot;
use onlyne_proto::{
    AdminControl, AdminOp, AdminReport, AdminSend, Body, Causality, ControlOp, EventRow, EventTier,
    Frame, LedgerEntry, LedgerQuery, MsgKind, Principal, QueryFaultsArgs, QueryRolesArgs,
    QuerySessionsArgs, RepairAck, RepairFail, RepairTarget, Report, ResBody, Subscribe,
    new_envelope, new_id, new_task_id,
};
use onlyne_wire::FrameReader;
use onlyne_wire::socket::LocalStream;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::Instant;

/// How long a board whose link dropped waits before dialing again.
///
/// Long enough that a server which is down is not dialed in a tight loop, short
/// enough that a restart costs a pause. It is the same 250 ms
/// `onlyne watch --follow` waits, because the two recover the same way.
const RECONNECT_PAUSE: Duration = Duration::from_millis(250);

/// How many rows each read of the snapshot keeps.
///
/// The snapshot is the board's first frame and its resync; the subscription is
/// what keeps the board current, so this bounds the first frame rather than the
/// board.
const SNAPSHOT_LIMIT: u32 = 200;

/// How long a transition waits before its row's causality is read off the
/// ledger.
///
/// A burst of transitions settles into one read, and the pause is short enough
/// that a single operator action is traced while the operator watches. The web
/// link waits the same 250 ms for the same read.
const HEAL_PAUSE: Duration = Duration::from_millis(250);

/// The board's two channels to the IO task.
pub struct Task {
    /// The ops the screen asks for.
    pub actions: UnboundedSender<Action>,
    /// What the IO task learned, for the board to fold.
    pub events: UnboundedReceiver<Event>,
}

/// Start the IO task against an admin socket.
///
/// The task ends when the board drops [`Task`], which is what keeps a
/// background process from outliving the terminal it was drawn for.
pub fn spawn(path: PathBuf, timeout_ms: u64) -> Task {
    let (actions, action_rx) = unbounded_channel();
    let (event_tx, events) = unbounded_channel();
    tokio::spawn(run(path, timeout_ms, event_tx, action_rx));
    Task { actions, events }
}

/// Whether this event is one the ledger can have moved further than the stream
/// said.
///
/// A `ledger_state` event is a transition: it carries no causality, so a row
/// this board had never read arrives with its causal columns empty. It is also
/// one transition and not the row's whole life, so a row whose terminal
/// transition the stream dropped stays nonterminal however much of its
/// causality was filled. Both cases are settled by the same ledger read, and
/// the read is safe because the merge only fills a column a row was missing and
/// only adopts a terminal state over a nonterminal one — so every transition is
/// owed the read, whether it arrived live or in a replay page.
fn is_transition_row(event: &onlyne_proto::Event) -> bool {
    matches!(event, onlyne_proto::Event::LedgerState(_))
}

/// Read the five admin reads into one snapshot.
///
/// This is the one place the snapshot is built, and `--once` shares it: the
/// single frame that mode prints is the same read the interactive board opens
/// with.
pub async fn read_snapshot(path: &Path, timeout_ms: u64) -> Result<Snapshot, String> {
    let mut stream = wire::connect(path, timeout_ms)
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
/// A transition carries neither causality nor the terminal state the stream may
/// have dropped, and the ledger owns both, so a heal reads this and nothing
/// else: the other four reads would cost four more round trips to fill nothing.
/// The window is [`SNAPSHOT_LIMIT`], the same one the snapshot's own `ledger`
/// read uses, so a merge sees the rows the snapshot would have shown.
async fn read_ledger(path: &Path, timeout_ms: u64) -> Result<Vec<LedgerEntry>, String> {
    let mut stream = wire::connect(path, timeout_ms)
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

/// The task's whole life: connect, read, subscribe, fold, and act.
async fn run(
    path: PathBuf,
    timeout_ms: u64,
    events: UnboundedSender<Event>,
    mut actions: UnboundedReceiver<Action>,
) {
    let mut cursor: u64 = 0;
    let mut subscription: Option<Subscription> = None;
    let mut ready_at = Instant::now();
    // When a transition the stream carried is due its one ledger read, if one
    // is pending. `None` means idle, and a fresh snapshot has already filled
    // every row it read, so the reconnect clears it with the view.
    let mut heal_at: Option<Instant> = None;
    loop {
        tokio::select! {
            // The board dropping its handle is the shutdown signal.
            action = actions.recv() => match action {
                None => return,
                Some(action) => {
                    // A refresh is a full snapshot, and a snapshot fills every
                    // row it reads: a heal left pending is its work now.
                    if perform(&path, timeout_ms, &events, action).await {
                        heal_at = None;
                    }
                }
            },
            frame = next_frame(&mut subscription, timeout_ms), if subscription.is_some() => {
                match frame {
                    Ok(Frame::Ev { seq, event }) => {
                        // Two events cannot be answered by folding alone, and
                        // both are answered the same way — re-read the
                        // snapshot, because it is the only thing that knows the
                        // registry. A gap lost events; a reload replaced the
                        // spec without naming the roles in it.
                        let gap = onlyne_proto::view::is_resync_lag(&event);
                        let reload =
                            matches!(*event, onlyne_proto::Event::SpecReloaded(_));
                        if !gap {
                            cursor = cursor.max(seq);
                        }
                        // A `ledger_state` event is a transition: it carries no
                        // family, no hop and no clock, and it says nothing about
                        // the transitions the stream dropped. The ledger owns
                        // both, so every transition is owed one read — armed
                        // only when none is pending, so a burst costs one read
                        // rather than one per event.
                        let transition = is_transition_row(&event);
                        let _ = events.send(Event::Stream(event));
                        if transition && heal_at.is_none() {
                            heal_at = Some(Instant::now() + HEAL_PAUSE);
                        }
                        if gap || reload {
                            subscription = None;
                            // The snapshot this reconnect is about to take
                            // fills every row it reads, so a pending heal is the
                            // snapshot's work now, not this loop's.
                            heal_at = None;
                            // The notice's `seq` is a drop count, not a
                            // cursor, so the last good cursor is what the
                            // reconnect asks from: the answer page carries
                            // what the broadcast dropped. A reload owes no such
                            // pause — the spec the operator just changed
                            // should not sit unread while the link sleeps.
                            ready_at = if gap {
                                Instant::now() + RECONNECT_PAUSE
                            } else {
                                Instant::now()
                            };
                        }
                    }
                    Ok(Frame::Bye { .. }) => {
                        let _ = events.send(Event::Link(Link::Offline(
                            "the server closed the stream".to_string(),
                        )));
                        subscription = None;
                        ready_at = Instant::now() + RECONNECT_PAUSE;
                    }
                    // A quiet stream is a quiet stream: the bound on one read
                    // is not a verdict on the link.
                    Ok(_) | Err(ExchangeError::Timeout) => {}
                    Err(error) => {
                        let _ = events.send(Event::Link(Link::Offline(describe(&error))));
                        subscription = None;
                        ready_at = Instant::now() + RECONNECT_PAUSE;
                    }
                }
            }
            _ = tokio::time::sleep_until(ready_at), if subscription.is_none() => {
                ready_at = Instant::now() + RECONNECT_PAUSE;
                // The snapshot this reconnect is about to take fills every row
                // it reads, so whatever heal was pending is its work now.
                heal_at = None;
                // A fresh read before every subscription: it is what the
                // reducer clears `stale` with, and what heals a board that
                // reconnected after a gap.
                let snapshot = match read_snapshot(&path, timeout_ms).await {
                    Ok(snapshot) => snapshot,
                    Err(reason) => {
                        let _ = events.send(Event::Link(Link::Offline(reason)));
                        continue;
                    }
                };
                // The cursor is the server's own head only until this board has
                // seen its first event: after that it is where the board got
                // to, and every reconnect resumes there.
                if cursor == 0 {
                    cursor = event_head(&snapshot);
                }
                let _ = events.send(Event::Snapshot(Box::new(snapshot)));
                match subscribe(&path, cursor, timeout_ms).await {
                    Ok((subscription_link, page)) => {
                        // The page is where a reload most often arrives: the
                        // snapshot above was read a moment before the page, so
                        // an operator's edit that landed in that gap is
                        // delivered here and not on the live stream. A page
                        // that carries one is answered the same way the stream
                        // answers it — by re-reading — because the snapshot
                        // this board was just built from cannot know the
                        // routes the reload brought.
                        let mut reread = false;
                        for row in page {
                            cursor = cursor.max(row.seq);
                            reread |= onlyne_proto::view::is_resync_lag(&row.event)
                                || matches!(row.event, onlyne_proto::Event::SpecReloaded(_));
                            // A page can carry the very events the snapshot did
                            // not include, so a transition replayed here is owed
                            // the same one ledger read a live transition is.
                            let transition = is_transition_row(&row.event);
                            let _ = events.send(Event::Stream(Box::new(row.event)));
                            if transition && heal_at.is_none() {
                                heal_at = Some(Instant::now() + HEAL_PAUSE);
                            }
                        }
                        let _ = events.send(Event::Link(Link::Live));
                        if reread {
                            drop(subscription_link);
                            heal_at = None;
                            ready_at = Instant::now();
                        } else {
                            subscription = Some(subscription_link);
                        }
                    }
                    Err(reason) => {
                        let _ = events.send(Event::Link(Link::Offline(reason)));
                    }
                }
            }
            _ = tokio::time::sleep_until(heal_at.unwrap_or(Instant::now() + HEAL_PAUSE)), if heal_at.is_some() => {
                // The deadline is cleared before the read, so a failed read
                // cannot spin this branch: a heal is not retried by its own
                // timer, it is re-armed by the next ledger transition. Nothing
                // here drops the subscription or replaces the view — the merge
                // only fills a column a row was missing and adopts a terminal
                // state over a nonterminal one.
                heal_at = None;
                if let Ok(rows) = read_ledger(&path, timeout_ms).await {
                    let _ = events.send(Event::Ledger(rows));
                }
            }
        }
    }
}

/// A live subscription: the connection, and the reader that keeps its partial
/// frames across the idle timeouts between them.
struct Subscription {
    reader: FrameReader,
    stream: LocalStream,
}

/// Open the subscription from a cursor and read the page it answers with.
async fn subscribe(
    path: &Path,
    cursor: u64,
    timeout_ms: u64,
) -> Result<(Subscription, Vec<EventRow>), String> {
    let mut stream = wire::connect(path, timeout_ms)
        .await
        .map_err(|error| local_error(&error))?;
    let request = Outbound::admin(
        new_id(),
        AdminOp::Subscribe(Subscribe {
            since_seq: cursor,
            tiers: vec![EventTier::Durable, EventTier::Advisory],
            kinds: Vec::new(),
            roles: Vec::new(),
        }),
    );
    wire::send_frame(&mut stream, &request, timeout_ms)
        .await
        .map_err(|error| describe(&error))?;
    let mut reader = FrameReader::new();
    match wire::recv_stream_frame(&mut reader, &mut stream, timeout_ms).await {
        Ok(Frame::Res { body, .. }) => {
            if !body.ok {
                return Err(answered(&body).expect_err("a refusal"));
            }
            let page = page_rows(&body)?;
            Ok((Subscription { reader, stream }, page))
        }
        Ok(other) => Err(format!(
            "expected the subscription's page, got a {} frame",
            wire::frame_name(&other)
        )),
        Err(error) => Err(describe(&error)),
    }
}

/// Wait for the link's next frame, or for nothing at all while it is down.
///
/// The down case is a future that never resolves, so the arm's own guard is
/// belt and braces: a `select!` arm may not borrow an `Option` it also mutates
/// inside another arm's body, and this keeps the borrow out of the caller.
async fn next_frame(
    subscription: &mut Option<Subscription>,
    timeout_ms: u64,
) -> Result<Frame, ExchangeError> {
    match subscription {
        Some(subscription) => {
            wire::recv_stream_frame(
                &mut subscription.reader,
                &mut subscription.stream,
                timeout_ms,
            )
            .await
        }
        None => std::future::pending().await,
    }
}

/// The rows of a subscription's first page.
fn page_rows(body: &ResBody) -> Result<Vec<EventRow>, String> {
    let events = body
        .data
        .as_ref()
        .and_then(|data| data.get("events"))
        .cloned()
        .ok_or_else(|| "the subscription answer carries no page".to_string())?;
    serde_json::from_value(events).map_err(|error| error.to_string())
}

/// Perform one action the screen asked for, and report what it answered.
///
/// Answers whether the action took a fresh snapshot, which is the one answer
/// that replaces the whole view and so settles a pending heal.
async fn perform(
    path: &Path,
    timeout_ms: u64,
    events: &UnboundedSender<Event>,
    action: Action,
) -> bool {
    if action == Action::Refresh {
        match read_snapshot(path, timeout_ms).await {
            Ok(snapshot) => {
                let _ = events.send(Event::Snapshot(Box::new(snapshot)));
                return true;
            }
            Err(reason) => {
                let _ = events.send(Event::Link(Link::Offline(reason)));
            }
        }
        return false;
    }
    let word = action.word();
    let result = match op(action) {
        Ok(op) => exchange(path, op, timeout_ms).await,
        Err(why) => Err(why),
    };
    let _ = events.send(Event::Answered { op: word, result });
    false
}

/// The admin op one action carries.
fn op(action: Action) -> Result<AdminOp, String> {
    Ok(match action {
        Action::Send { from, to, body } => {
            // A send starts a family: a fresh task id at hop 0, exactly as the
            // client mints one for a fresh piece of work.
            let envelope = new_envelope(
                MsgKind::Task,
                Principal::role(&from),
                Principal::role(&to),
                Body {
                    text: Some(body),
                    head: None,
                    image: None,
                },
                Some(Causality::root(new_task_id())),
            )
            .map_err(|error| error.to_string())?;
            AdminOp::Send(AdminSend {
                from,
                envelope: Box::new(envelope),
            })
        }
        Action::Focus { from, to, task_id } => AdminOp::Control(AdminControl {
            from,
            op: ControlOp::Focus { task_id },
            to: Some(to),
        }),
        Action::Repair(repair) => match repair {
            Repair::Ack { fault_id, reason } => AdminOp::RepairAck(RepairAck { fault_id, reason }),
            Repair::Retry { task_id, reason } => AdminOp::RepairRetry(RepairTarget {
                task_id,
                reason: Some(reason),
            }),
            Repair::Close { task_id, reason } => AdminOp::RepairClose(RepairTarget {
                task_id,
                reason: Some(reason),
            }),
            Repair::Fail { task_id, reason } => AdminOp::RepairFail(RepairFail { task_id, reason }),
            Repair::Inspect { task_id } => AdminOp::RepairInspect(RepairTarget {
                task_id,
                reason: None,
            }),
        },
        Action::Report {
            from,
            task_id,
            outcome,
            head,
        } => AdminOp::Report(AdminReport {
            from,
            report: Box::new(Report::Complete {
                task_id,
                outcome,
                head: Some(head),
                details: None,
                files: Vec::new(),
                reply_to: None,
                cluster_ref: None,
            }),
        }),
        Action::Refresh => return Err("refresh reads the snapshot rather than an op".to_string()),
    })
}

/// Send one op over a connection of its own and read its answer.
///
/// An op the operator asked for does not share the subscription's connection:
/// that connection carries frames the op cannot tell apart from its own answer,
/// so one request per connection is what keeps an answer answerable.
async fn exchange(path: &Path, op: AdminOp, timeout_ms: u64) -> Result<String, String> {
    let mut stream = wire::connect(path, timeout_ms)
        .await
        .map_err(|error| local_error(&error))?;
    let request = Outbound::admin(new_id(), op);
    let body = wire::request_res(&mut stream, &request, timeout_ms)
        .await
        .map_err(|error| describe(&error))?;
    answered(&body)
}

/// One answer as the footer prints it.
fn answered(body: &ResBody) -> Result<String, String> {
    if !body.ok {
        return Err(match &body.error {
            Some(error) => format!("{}: {}", error.code, error.message),
            None => "refused".to_string(),
        });
    }
    let Some(data) = body.data.as_ref() else {
        return Ok("ok".to_string());
    };
    let receipt = data
        .get("receipt")
        .and_then(|receipt| receipt.get("msg_id"))
        .and_then(Value::as_str);
    match receipt {
        Some(msg_id) => Ok(format!("msg_id {}", short(msg_id))),
        None => Ok(crate::tui::render::one_line(
            &serde_json::to_string(data).unwrap_or_else(|_| "ok".to_string()),
            120,
        )),
    }
}

/// Read one admin op's answer data.
async fn read_data(
    stream: &mut LocalStream,
    op: AdminOp,
    timeout_ms: u64,
) -> Result<Value, String> {
    let request = Outbound::admin(new_id(), op);
    let body = wire::request_res(stream, &request, timeout_ms)
        .await
        .map_err(|error| describe(&error))?;
    if !body.ok {
        return Err(answered(&body).expect_err("a refusal"));
    }
    Ok(body.data.unwrap_or(Value::Null))
}

/// The event cursor a snapshot's own `status` read names.
///
/// The head is the server's newest event `seq`, which is where a subscription
/// that starts now has to resume from: subscribing from zero would ask for the
/// current head anyway, and asking from the snapshot's own head is what closes
/// the window between the five reads and the subscribe.
fn event_head(snapshot: &Snapshot) -> u64 {
    snapshot
        .status
        .as_ref()
        .map(onlyne_proto::view::ClusterSummary::from_status)
        .and_then(|summary| summary.event_head)
        .unwrap_or_default()
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

/// The first eight characters of an id, for the one line a notice has.
fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Why one exchange ended, in the words the CLI's follow uses for the same
/// three outcomes.
fn describe(error: &ExchangeError) -> String {
    match error {
        ExchangeError::Timeout => "the socket timed out".to_string(),
        ExchangeError::Closed => "the link closed".to_string(),
        ExchangeError::Wire(_, message) => message.clone(),
    }
}

/// Why a connect or a write failed locally.
fn local_error(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            format!("no server at the socket: {error}")
        }
        _ => error.to_string(),
    }
}
