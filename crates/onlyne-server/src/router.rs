//! Frame router: one `match` per vocabulary (§6, §8).
//!
//! Every arm calls a landed handler. A frame that arrives before the handshake
//! is refused with `HELLO_REQUIRED_MESSAGE` under `ErrorCode::Invalid`, which is
//! the plan's spelling at §7 line 310, and an op outside a connection's
//! vocabulary answers `ErrorCode::UnknownOp`.

use crate::events;
use crate::faults::{self, FaultDraft};
use crate::gateway_host;
use crate::ghosts;
use crate::projection;
use crate::relay::{self, RelayReply};
use crate::spec_edits;
use crate::state::{self as server_state, State};
use chrono::Utc;
use onlyne_config::layout::ServerRoot;
use onlyne_config::{Spec, SpecDiff};
use onlyne_proto::{
    AdminOp, Body, Causality, ClientOp, ControlOp, ErrorCode, Event, Frame, GatewayOp,
    HELLO_REQUIRED_MESSAGE, LedgerEntry, MsgKind, Presence, Principal, PullArgs, PullReply,
    QueryRolesArgs, ResBody, RoleInfo, RolePresence, SpecReloaded,
};
use onlyne_store::RoleRow;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

/// Per-connection state carried by the router.
#[derive(Debug, Default)]
pub struct Session {
    pub role: Option<String>,
    /// The role the transport handshake verified against the presented key.
    /// `onlyne_net::handshake` checks the key against the registered key of the
    /// claimed role, so this name is the sole authority for role identity on a
    /// TLS connection.
    pub authorised: Option<String>,
    pub admin: bool,
    pub authenticated: bool,
    pub session_id: Option<String>,
    pub generation: u64,
    pub sender: Option<mpsc::Sender<Frame>>,
    pub gateway: Option<String>,
    /// True while a task this connection spawned owns a held `pull`'s answer.
    ///
    /// One connection holds at most one pull at a time: a second held request
    /// would park a second task beside the first, and both would read the same
    /// role's queue.
    pub held_pull: Arc<AtomicBool>,
}

impl Session {
    /// A client session whose role identity the transport already settled.
    pub fn with_sender(sender: mpsc::Sender<Frame>, authorised: &str) -> Self {
        Session {
            authorised: Some(authorised.to_string()),
            sender: Some(sender),
            ..Session::default()
        }
    }

    fn role_or_reject(&self) -> Result<&str, ResBody> {
        self.role.as_deref().ok_or_else(hello_required)
    }

    fn welcome(&mut self, role: &str, admin: bool) {
        self.role = Some(role.to_string());
        self.admin = admin;
        self.authenticated = true;
    }
}

/// The refusal a frame earns when it arrives before its `hello`.
///
/// §7 line 310 spells this `invalid` with the message below, and `invalid` is
/// the honest code here: a frame that arrived before its `hello` is a fault in
/// the order of requests, not a claim about identity that a person has to fix.
/// `unauthorized` would say the second thing, and it is the right answer for the
/// other case — a key the server does not know, which waits on a re-key.
///
/// This used to be justified the other way round, by a classification that
/// called `unauthorized` retryable and so could not be used. One vocabulary
/// with three answers — `Retry::Never`, `Retry::AfterHuman`,
/// `Retry::UnderBackoff` — makes the workaround unnecessary and the reasoning
/// say what it is actually about.
pub(crate) fn hello_required() -> ResBody {
    ResBody::err(
        ErrorCode::Invalid,
        HELLO_REQUIRED_MESSAGE,
        Some("op".to_string()),
    )
}

/// How one answered request reaches the wire.
///
/// Almost every request settles inside the read loop and is written there. A
/// held `pull` cannot: its window belongs to the server, and blocking the loop
/// for it would stop this connection's reports and heartbeats. Its answer
/// therefore leaves on the task that waited, through the session's channel.
pub enum Answer {
    Immediate(ResBody),
    /// The reply is written by the task this arm spawned, as
    /// `Frame::res(id, body)` sent through the session's channel.
    Deferred,
}

/// The server's ceiling on one held `pull`, so a client cannot park a task for
/// an arbitrarily long window.
pub const MAX_HOLD_MS: u64 = 30_000;

/// Route one client frame body.
///
/// `id` is the request's frame id. Only a held `pull` needs it: that arm
/// answers on a task of its own and builds the `res` frame there, because
/// waiting out its window inside this function would stop every other frame on
/// the connection (design §4.3).
pub async fn dispatch_client(
    state: &Arc<State>,
    session: &mut Session,
    id: &str,
    op: ClientOp,
) -> Answer {
    let pre_auth = matches!(op, ClientOp::Hello(_));
    if !session.authenticated && !pre_auth {
        return Answer::Immediate(hello_required());
    }
    match op {
        ClientOp::Hello(args) => Answer::Immediate(hello(state, session, args)),
        ClientOp::Send(envelope) => {
            let role = match session.role_or_reject() {
                Ok(role) => role.to_string(),
                Err(body) => return Answer::Immediate(body),
            };
            let mut envelope = *envelope;
            envelope.from = Principal::role(role.clone());
            envelope.admin = session.admin;
            let owner = envelope
                .task_id()
                .and_then(|task| relay::task_owner(state, task));
            Answer::Immediate(reply_body(
                state,
                relay::send(state, &envelope, session.admin, owner.as_deref()),
            ))
        }
        ClientOp::Pull(args) => {
            let role = match session.role_or_reject() {
                Ok(role) => role.to_string(),
                Err(body) => return Answer::Immediate(body),
            };
            // One connection reads its queue once per wait. A second pull
            // while a held one owns the connection would read the same queue
            // and arm a second ticket for rows the first is about to be
            // handed, so it is refused rather than answered — and an immediate
            // pull is refused by the same rule, because it is the same read.
            let held = session.held_pull.clone();
            if held.load(Ordering::SeqCst) {
                return Answer::Immediate(held_pull_refusal());
            }
            // The registry is what names this connection's role, so a link it
            // has already replaced claims no row at all: every ticket it armed
            // would outlive it, and a role-level pull carries no session to
            // tell the two links apart.
            if !holds_role(state, &role, session.generation) {
                return Answer::Immediate(departed_link_refusal(&role));
            }
            // A session with no channel has no way back to the wire on its
            // own, so its answer travels the read loop like every other one.
            let Some(sender) = session.sender.clone() else {
                return Answer::Immediate(immediate_pull(state, session, &role, &args));
            };
            let Some(window) = hold_window(args.hold_ms) else {
                return Answer::Immediate(owned_pull(
                    state,
                    session,
                    &role,
                    &args,
                    session.generation,
                    &sender,
                ));
            };
            held.store(true, Ordering::SeqCst);
            // The subscription is taken before the first read of the queue: a
            // delivery appended while that read runs is then both in the
            // ledger and on the receiver, so the wait below cannot miss the
            // wake a `send` published in between.
            let frames = state.subscribe_frames();
            let first = match relay::pull(state, &role, session.session_id.as_deref(), &args) {
                Ok(reply) => reply,
                Err(error) => {
                    held.store(false, Ordering::SeqCst);
                    return Answer::Immediate(internal(error));
                }
            };
            if !first.deliveries.is_empty() {
                held.store(false, Ordering::SeqCst);
                let body = match keep_claim(state, &role, session.generation, &sender, first) {
                    Ok(answer) => answer.body,
                    Err(body) => body,
                };
                return Answer::Immediate(body);
            }
            let hold = HeldPull {
                state: Arc::clone(state),
                role,
                session_id: session.session_id.clone(),
                generation: session.generation,
                args,
                frames,
                window,
            };
            let id = id.to_string();
            let owner = Arc::clone(&hold.state);
            tokio::spawn(async move {
                // A dead link, a replaced registry entry, or a shutdown request
                // ends the wait with no answer: none of the three may claim a
                // row for a connection that is gone.
                if let Some(answer) = hold_pull(hold, &sender).await {
                    if sender.send(Frame::res(id, answer.body)).await.is_err() {
                        // The answer never reached the client, so the rows it
                        // hands over go back: an in-flight row whose ticket
                        // names this link is one no later pull will take.
                        release_claims(&owner, &answer.claimed);
                    }
                }
                held.store(false, Ordering::SeqCst);
            });
            Answer::Deferred
        }
        ClientOp::Ack(args) => Answer::Immediate(match relay::ack(state, &args) {
            Ok(Ok(event)) => ResBody::ok(serde_json::to_value(event).unwrap_or_default()),
            Ok(Err(reject)) => reject.body(),
            Err(error) => internal(error),
        }),
        ClientOp::Report(report) => {
            let role = match session.role_or_reject() {
                Ok(role) => role.to_string(),
                Err(body) => return Answer::Immediate(body),
            };
            Answer::Immediate(report_body(
                projection::report(state, &role, &report),
                &report,
            ))
        }
        ClientOp::Subscribe(subscribe) => {
            Answer::Immediate(match events::page_for(state, &subscribe) {
                Ok(page) => {
                    if let Some(sender) = session.sender.clone() {
                        events::spawn_forwarder(
                            state,
                            events::EventFilter::from_subscribe(&subscribe),
                            sender,
                        );
                    }
                    ResBody::ok(events::page_json(&page))
                }
                Err(error) => internal(error),
            })
        }
        ClientOp::QueryLedger(query) => Answer::Immediate(match state.ledger.ledger_query(query) {
            Ok(rows) => ResBody::ok(json!({
                "ledger": rows.iter().map(relay::entry_from_row).collect::<Vec<LedgerEntry>>(),
            })),
            Err(error) => internal(error.into()),
        }),
        ClientOp::QuerySessions(query) => {
            let admin = session.admin;
            Answer::Immediate(match projection::sessions_read(state, query, admin).await {
                Ok(rows) => ResBody::ok(json!({ "sessions": rows })),
                Err(error) => internal(error),
            })
        }
        // The one question a scoped session asks of the ledger: does the family
        // it holds a conversation for still owe work? The row count is the whole
        // answer, and the client reads it back as `open`.
        ClientOp::QueryFamilyOpen(args) => {
            Answer::Immediate(match state.ledger.family_open_count(&args.family) {
                Ok(open) => ResBody::ok(json!({ "open": open })),
                Err(error) => internal(error.into()),
            })
        }
        ClientOp::QueryRoles(query) => Answer::Immediate(match roles(state, &query) {
            Ok(rows) => ResBody::ok(json!({ "roles": rows })),
            Err(error) => internal(error),
        }),
        ClientOp::QueryFaults(query) => Answer::Immediate(match faults::query(state, &query) {
            Ok(rows) => ResBody::ok(json!({ "faults": rows })),
            Err(error) => internal(error),
        }),
        ClientOp::Control(args) => {
            let role = match session.role_or_reject() {
                Ok(role) => role.to_string(),
                Err(body) => return Answer::Immediate(body),
            };
            let envelope = control_envelope(&role, &args.op, args.to.as_deref());
            let owner = relay::task_owner(state, args.op.task_id());
            // A control word settles the same way whether an operator's socket
            // or a supervisor role's link carried it: one rule, two doors.
            crate::faults::settle_controlled(state, &args.op);
            Answer::Immediate(reply_body(
                state,
                relay::send(state, &envelope, session.admin, owner.as_deref()),
            ))
        }
        // §3c's turn-end family reaches the server's stream through this one
        // verb and nothing else: it carries no `(generation, seq)`, passes
        // through none of the report gate, and changes no session state. The
        // server appends the event and answers nothing, because the class is
        // the client's own word and an answer would be a second spelling of
        // it. A class outside the closed set is refused by name, so a peer
        // cannot invent an event name and a hook cannot bind to a spelling
        // nobody publishes (`docs/v2-CONTRACT.md` §"Slice 7").
        ClientOp::PublishEvent(args) => {
            if session.role_or_reject().is_err() {
                return Answer::Immediate(hello_required());
            }
            let Some(event) = onlyne_proto::client_event(&args.class, args.payload) else {
                return Answer::Immediate(ResBody::err(
                    ErrorCode::Invalid,
                    format!(
                        "unknown event class '{}'; a client may publish only {}",
                        args.class,
                        onlyne_proto::CLIENT_EVENT_CLASSES.join(", ")
                    ),
                    Some("class".to_string()),
                ));
            };
            Answer::Immediate(match state.emit(event) {
                Ok(_) => ResBody::ok(Value::Null),
                Err(error) => internal(error),
            })
        }
        ClientOp::Bye(args) => {
            if let Some(role) = session.role.clone() {
                relay::disconnect(state, &role).ok();
            }
            session.authenticated = false;
            Answer::Immediate(ResBody::ok(json!({ "bye": args.reason })))
        }
    }
}

/// The window a `pull` asks the server to hold, or `None` when it wants its
/// answer now.
///
/// `hold_ms` absent or zero is the immediate read every client used before the
/// window existed; a request above [`MAX_HOLD_MS`] is capped to it.
fn hold_window(hold_ms: Option<u64>) -> Option<Duration> {
    match hold_ms {
        Some(ms) if ms > 0 => Some(Duration::from_millis(ms.min(MAX_HOLD_MS))),
        _ => None,
    }
}

/// The one queue read every `pull` answered inside the read loop performs.
fn immediate_pull(state: &Arc<State>, session: &Session, role: &str, args: &PullArgs) -> ResBody {
    match relay::pull(state, role, session.session_id.as_deref(), args) {
        Ok(reply) => pull_body(&reply),
        Err(error) => internal(error),
    }
}

/// The queue read of a link that owns its role.
///
/// A row this read hands over belongs to the link, and the loop writes the
/// answer that says so; a link the registry replaced in the same instant gets
/// the rows put back and a refusal instead.
fn owned_pull(
    state: &Arc<State>,
    session: &Session,
    role: &str,
    args: &PullArgs,
    generation: u64,
    sender: &mpsc::Sender<Frame>,
) -> ResBody {
    let reply = match relay::pull(state, role, session.session_id.as_deref(), args) {
        Ok(reply) => reply,
        Err(error) => return internal(error),
    };
    match keep_claim(state, role, generation, sender, reply) {
        Ok(answer) => answer.body,
        Err(body) => body,
    }
}

/// The refusal a `pull` earns while this connection already holds one.
fn held_pull_refusal() -> ResBody {
    ResBody::err(
        ErrorCode::Conflict,
        "this connection already holds a pull; one connection reads its queue once per wait",
        Some("op".to_string()),
    )
}

/// The refusal a `pull` earns on a link the registry no longer names.
fn departed_link_refusal(role: &str) -> ResBody {
    ResBody::err(
        ErrorCode::Conflict,
        format!("another connection holds role {role}; this link reads no queue"),
        Some("role".to_string()),
    )
}

/// Everything one held `pull` needs to answer after its window.
struct HeldPull {
    state: Arc<State>,
    role: String,
    session_id: Option<String>,
    /// The registry generation this connection registered at `hello`.
    generation: u64,
    args: PullArgs,
    frames: broadcast::Receiver<Arc<Frame>>,
    window: Duration,
}

/// One held pull's answer, with the rows it hands over.
struct HeldAnswer {
    body: ResBody,
    /// The rows this answer moves to this connection. An answer that never
    /// reaches the client must put them back.
    claimed: Vec<String>,
}

/// An answer that hands over no row.
fn answered(body: ResBody) -> HeldAnswer {
    HeldAnswer {
        body,
        claimed: Vec::new(),
    }
}

/// Wait out one held `pull` and build the answer the connection will write.
///
/// Each wake re-reads the queue, and only a frame that can change that read is
/// a wake: a ledger row reaches this role through its target, the eligibility
/// a note needs lands as a session row of this role, a reload can change what
/// the role may be handed, and a lag drops events whose read this loop owes
/// itself. Everything else — a fault, a gateway's health, another role's
/// traffic — leaves the queue as it was. The deadline is fixed when the wait
/// starts and a wake never moves it.
///
/// `None` is a link that can no longer receive: its channel has closed, its
/// role's registry entry has moved to another connection, or the server is
/// shutting down. None of those may claim a row, so the wait ends without a
/// further read.
async fn hold_pull(hold: HeldPull, sender: &mpsc::Sender<Frame>) -> Option<HeldAnswer> {
    let HeldPull {
        state,
        role,
        session_id,
        generation,
        args,
        mut frames,
        window,
    } = hold;
    let shutdown = Arc::clone(&state.shutdown);
    // One `notified` future for the whole wait, pinned: a waiter registered
    // once is still registered between two reads of the queue, so a shutdown
    // request that lands while this loop is pulling a row is not lost.
    let shutdown_requested = shutdown.notified();
    tokio::pin!(shutdown_requested);
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::select! {
            _ = sender.closed() => return None,
            _ = &mut shutdown_requested => return None,
            waited = tokio::time::timeout(remaining, frames.recv()) => {
                match waited {
                    // A lag is a wake: what the broadcast dropped is still in
                    // the ledger, so the read below owes the queue a look.
                    Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
                    Ok(Ok(frame)) if wakes(frame.as_ref(), &role) => {}
                    Ok(Ok(_)) => continue,
                    // The broadcast closed and the window elapsed, alike.
                    Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => break,
                }
            }
        }
        if !owns_link(&state, &role, generation, sender) {
            return None;
        }
        let reply = match relay::pull(&state, &role, session_id.as_deref(), &args) {
            Ok(reply) => reply,
            Err(error) => return Some(answered(internal(error))),
        };
        if reply.deliveries.is_empty() {
            // Nothing to hand over, so the wait goes on to its window — on a
            // link that still holds the role, which every wake re-asks.
            if !owns_link(&state, &role, generation, sender) {
                return None;
            }
            continue;
        }
        return Some(match keep_claim(&state, &role, generation, sender, reply) {
            Ok(answer) => answer,
            Err(body) => answered(body),
        });
    }
    // Expiry reads once more, so the empty answer carries the head the
    // connection resumes from and a delivery that landed exactly at the
    // deadline is handed over instead of waiting for the client's next round.
    if !owns_link(&state, &role, generation, sender) {
        return None;
    }
    let reply = match relay::pull(&state, &role, session_id.as_deref(), &args) {
        Ok(reply) => reply,
        Err(error) => return Some(answered(internal(error))),
    };
    Some(match keep_claim(&state, &role, generation, sender, reply) {
        Ok(answer) => answer,
        Err(body) => answered(body),
    })
}

/// Answer one read that found rows, or put them back when the registry moved
/// on between the ownership check and the read.
///
/// Checking ownership and reading the queue are two separate reads of the
/// registry, so a successor that registers the role in between leaves this
/// connection holding rows no later pull will take: `pull` passes by an
/// in-flight row whose ticket names the puller, and a role-level pull carries
/// no session to tell two links of one role apart. The rows go back to the
/// queue, and the connection that is gone is told it holds no queue.
fn keep_claim(
    state: &State,
    role: &str,
    generation: u64,
    sender: &mpsc::Sender<Frame>,
    reply: PullReply,
) -> Result<HeldAnswer, ResBody> {
    let claimed: Vec<String> = reply
        .deliveries
        .iter()
        .map(|delivery| delivery.msg_id.clone())
        .collect();
    if !claimed.is_empty() && !owns_link(state, role, generation, sender) {
        release_claims(state, &claimed);
        return Err(departed_link_refusal(role));
    }
    Ok(HeldAnswer {
        body: pull_body(&reply),
        claimed,
    })
}

/// Put rows this connection claimed back where they were.
///
/// The ledger write is what makes a row pullable again, and the ticket is what
/// stops one: `pull` passes by an in-flight row whose ticket is still armed,
/// so the ticket goes with the same move. A row that settled meanwhile is left
/// alone.
fn release_claims(state: &State, msg_ids: &[String]) {
    for msg_id in msg_ids {
        state.take_delivery(msg_id);
        if let Err(error) = state.ledger.requeue_one(msg_id) {
            tracing::warn!(
                error = %error,
                msg_id = %msg_id,
                "a row claimed by a departed link was left in flight"
            );
        }
    }
}

/// Whether one published frame can change what this role's pull is handed.
///
/// A held pull re-reads the queue on every wake, so a frame that cannot change
/// that read must not wake it: a busy cluster publishes for every role, and
/// waking every waiter on all of them turns one read per event into a query
/// per event for every waiting link.
fn wakes(frame: &Frame, role: &str) -> bool {
    let Frame::Ev { event, .. } = frame else {
        return false;
    };
    match &**event {
        Event::LedgerState(update) => update.to.role_name() == Some(role),
        Event::SessionState(update) => update.role == role,
        Event::RolePresence(presence) => presence.role == role,
        // A reload can change what this role may be handed, and the roles
        // table with it, so it wakes every waiter.
        Event::SpecReloaded(_) => true,
        _ => false,
    }
}

/// Whether this connection can still carry an answer.
///
/// The two halves are the two ways a role link ends: the read loop returns and
/// drops its channel, or a successor connection registers the role and takes
/// the registry entry (and with it every ticket the role holds).
fn owns_link(state: &State, role: &str, generation: u64, sender: &mpsc::Sender<Frame>) -> bool {
    !sender.is_closed() && holds_role(state, role, generation)
}

/// Whether the registry still names this connection for the role.
///
/// A connection registers at `hello` and the entry is replaced by whoever
/// registers next, so a generation that moved on is another link's queue.
fn holds_role(state: &State, role: &str, generation: u64) -> bool {
    state
        .roles
        .read()
        .ok()
        .and_then(|roles| roles.current_generation(role))
        == Some(generation)
}

/// Map one pull reply onto a response body.
fn pull_body(reply: &PullReply) -> ResBody {
    ResBody::ok(serde_json::to_value(reply).unwrap_or_default())
}

/// Route one admin frame body.
pub async fn dispatch_admin(state: &Arc<State>, session: &mut Session, op: AdminOp) -> ResBody {
    match op {
        AdminOp::Status(_) => status(state),
        AdminOp::Roles(query) => match roles(state, &query) {
            Ok(rows) => ResBody::ok(json!({ "roles": rows })),
            Err(error) => internal(error),
        },
        AdminOp::Sessions(query) => match projection::sessions_read(state, query, true).await {
            Ok(rows) => ResBody::ok(json!({ "sessions": rows })),
            Err(error) => internal(error),
        },
        AdminOp::Ledger(query) => match state.ledger.ledger_query(query) {
            Ok(rows) => ResBody::ok(json!({
                "ledger": rows.iter().map(relay::entry_from_row).collect::<Vec<LedgerEntry>>(),
            })),
            Err(error) => internal(error.into()),
        },
        AdminOp::Faults(query) => match faults::query(state, &query) {
            Ok(rows) => ResBody::ok(json!({ "faults": rows })),
            Err(error) => internal(error),
        },
        AdminOp::QueryGhostSweeps(limit) => {
            match state
                .ledger
                .list_ghost_sweeps(limit.min(u32::MAX as usize) as u32)
            {
                Ok(rows) => ResBody::ok(json!({
                    "ghost_sweeps": rows.iter().map(ghosts::entry_from_row).collect::<Vec<_>>(),
                })),
                Err(error) => internal(error.into()),
            }
        }
        AdminOp::Watch(subscribe) => match events::page_for(state, &subscribe) {
            Ok(page) => ResBody::ok(events::page_json(&page)),
            Err(error) => internal(error),
        },
        AdminOp::History(args) => match events::history(state, &args) {
            Ok(page) => ResBody::ok(events::page_json(&page)),
            Err(error) => internal(error),
        },
        AdminOp::SpecDiff(_) => match spec_diff(state) {
            Ok((diff, _hash)) => ResBody::ok(json!({
                "render": diff.render(),
                "empty": diff.is_empty(),
            })),
            Err(error) => internal(error),
        },
        AdminOp::Reload(_) => reload(state),
        AdminOp::SpecGet(_) => spec_edits::get(state),
        AdminOp::SpecApply(edit) => spec_edits::apply(state, &edit),
        // The one continuous stream on this surface. `watch` answers a page and
        // stops there; this answers the page and keeps the connection carrying
        // what follows it, so an operator's `watch --follow` has something to
        // follow and a board does not have to poll.
        AdminOp::Subscribe(subscribe) => match events::page_for(state, &subscribe) {
            Ok(page) => {
                if let Some(sender) = session.sender.clone() {
                    events::spawn_stream(state, &subscribe, &page, sender);
                }
                ResBody::ok(events::page_json(&page))
            }
            Err(error) => internal(error),
        },
        AdminOp::Send(admin_send) => {
            let mut envelope = *admin_send.envelope;
            envelope.from = Principal::role(admin_send.from.clone());
            envelope.admin = true;
            let owner = envelope
                .task_id()
                .and_then(|task| relay::task_owner(state, task));
            reply_body(state, relay::send(state, &envelope, true, owner.as_deref()))
        }
        AdminOp::Control(admin_control) => {
            let envelope = control_envelope(
                &admin_control.from,
                &admin_control.op,
                admin_control.to.as_deref(),
            );
            let owner = relay::task_owner(state, admin_control.op.task_id());
            // The operator's word settles before the command that stops the
            // process is issued: `settle_task` rejects the task's outstanding
            // rows, and the row this sends is created after, so the client
            // still receives the instruction to close the host resource.
            crate::faults::settle_controlled(state, &admin_control.op);
            reply_body(state, relay::send(state, &envelope, true, owner.as_deref()))
        }
        AdminOp::Report(admin_report) => report_body(
            projection::report_as_admin(state, &admin_report.from, &admin_report.report),
            &admin_report.report,
        ),
        // The same settle the role's own connection runs, reachable once that
        // connection is gone: a dead client cannot be asked to clear its rows.
        AdminOp::Ack(args) => match relay::ack(state, &args) {
            Ok(Ok(event)) => ResBody::ok(serde_json::to_value(event).unwrap_or_default()),
            Ok(Err(reject)) => reject.body(),
            Err(error) => internal(error),
        },
        AdminOp::RepairInspect(_)
        | AdminOp::RepairAdopt(_)
        | AdminOp::RepairRebind(_)
        | AdminOp::RepairRetry(_)
        | AdminOp::RepairFail(_)
        | AdminOp::RepairClose(_)
        | AdminOp::RepairAck(_)
        | AdminOp::RepairAckMany(_) => match faults::repair(state, &op) {
            Ok(Ok(value)) => ResBody::ok(value),
            Ok(Err(reject)) => reject.body(),
            Err(error) => internal(error),
        },
        AdminOp::Shutdown(args) => {
            state.request_shutdown();
            ResBody::ok(json!({ "shutdown": args.reason }))
        }
    }
}

/// The answer to one settled report, shared by a session's own report and the
/// one an operator files for it.
fn report_body(
    outcome: anyhow::Result<projection::ProjectionOutcome>,
    report: &onlyne_proto::Report,
) -> ResBody {
    match outcome {
        Ok(outcome) => ResBody::ok(json!({
            "applied": outcome.applied,
            "kind": report.kind_name(),
            "task_id": report.task_id(),
        })),
        Err(error) => internal(error),
    }
}

/// Route one gateway frame body.
pub async fn dispatch_gateway(state: &Arc<State>, session: &mut Session, op: GatewayOp) -> ResBody {
    if let GatewayOp::Hello(args) = &op {
        session.gateway = Some(args.role.clone());
    }
    let body = gateway_host::handle_gateway_op(state, session.gateway.as_deref(), op).await;
    if !body.ok {
        session.gateway = None;
    }
    body
}

fn hello(state: &Arc<State>, session: &mut Session, args: onlyne_proto::HandshakeArgs) -> ResBody {
    let Some(spec) = state.spec_snapshot() else {
        return internal(anyhow::anyhow!("the spec is unavailable"));
    };
    // The transport verdict is the sole source of role identity on a role
    // connection: `onlyne_net::handshake` admitted this socket because the
    // presented key equals the registered key of `authorised`, so a typed
    // `hello` naming a second role is refused here, and the welcome below
    // records the authorised name.
    let authorised = session
        .authorised
        .clone()
        .unwrap_or_else(|| args.role.clone());
    if authorised != args.role {
        return ResBody::err(
            ErrorCode::Unauthorized,
            format!(
                "the transport authenticated role {authorised}; this hello claims {}",
                args.role
            ),
            Some("role".to_string()),
        );
    }
    let Some(entry) = spec
        .client
        .iter()
        .find(|entry| entry.role == authorised)
        .cloned()
    else {
        return ResBody::err(
            ErrorCode::Unauthorized,
            format!("unregistered role {authorised}"),
            Some("role".to_string()),
        );
    };
    session.welcome(&authorised, entry.admin);
    if let Some(sender) = session.sender.clone() {
        session.generation = state.register_role(crate::state::RoleConnection {
            role: entry.role.clone(),
            direct_notes: args.direct_notes,
            sender,
            last_seq: state.event_head().max(0) as u64,
            connected_at: Utc::now(),
            draining: false,
            generation: 0,
        });
        // The registry holds one link per role, so this hello says the link that
        // carried any in-flight row of this role is gone. `live_sessions` is
        // the live pane's declaration: the row of every delivery those sessions
        // are bound to stays in_flight and its ticket is rebound to this link's
        // generation and to the session that holds it. A re-delivery of a live
        // task would open a second session, and the released row of a session
        // that dies later is the one its ticket names. Every other
        // unacknowledged row is requeued and its ticket dropped. A push marks
        // its row `in_flight` before the frame reaches a socket, and a push
        // issued while the role's death is still unprocessed leaves a row whose
        // ticket names a connection that no longer exists. The departed link's
        // teardown skips its requeue once the registry generation has moved on,
        // and `pull` passes by a row whose ticket is still armed. This is the
        // row half of what a clean `bye` does; the link that is registering now
        // keeps its registry entry and its `Online` presence below. An omitted
        // `live_sessions` field is an empty list, which requeues every
        // unacknowledged row: a session bound to no delivery has no row to keep
        // in flight.
        let claim = relay::Claim::from_sessions(&args.live_sessions);
        match relay::requeue_role_rows(state, &entry.role, &claim) {
            Ok(requeued) if requeued > 0 => {
                tracing::info!(
                    role = %entry.role,
                    requeued,
                    "a new link took over rows its predecessor never delivered"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(error = %error, role = %entry.role, "handoff requeue failed")
            }
        }
        let _ = state.emit(Event::RolePresence(RolePresence {
            role: entry.role.clone(),
            state: Presence::Online,
            aggregate: if entry.aggregate.is_empty() {
                None
            } else {
                Some(entry.aggregate.clone())
            },
            sessions: 0,
            detail: Some(args.agent.clone()),
        }));
    }
    let welcome = onlyne_proto::Welcome {
        cluster: spec.server.name.clone(),
        server: crate::version().to_string(),
        role: entry.role.clone(),
        admin: entry.admin,
        // The label the plan puts on a supervisor's `[[client]]` entry (plan
        // §5 line 248): a pure annotation. The core delivery path carries no
        // aggregate branch (plan §5 line 274), so no ACL, routing, or ledger
        // decision reads it, and `spec.toml` stays the only truth about which
        // role represents which child cluster (plan §5 line 243).
        aggregate: (!entry.aggregate.is_empty()).then(|| entry.aggregate.clone()),
        max_sessions: entry.max_sessions,
        prose: entry.prose.clone(),
        spec_hash: spec.semantic_hash(),
        allowed_targets: entry.allowed_targets.clone(),
        owes_targets: entry.owes_targets.clone(),
        allowed_senders: entry.allowed_senders.clone(),
        runtime: Some(role_runtime(&entry.runtime)),
        timeout_ready_ms: Some(entry.timeout.ready_ms),
        timeout_idle_ms: Some(entry.timeout.idle_ms),
        intent_attempts: Some(entry.intent.attempts),
        intent_backoff_ms: Some(entry.intent.backoff_ms.clone()),
        seq: state.event_head().max(0) as u64,
    };
    ResBody::ok(serde_json::to_value(welcome).unwrap_or_default())
}

/// The spec's `[client.runtime]` table as the wire carries it.
///
/// The two vocabularies stay separate on purpose: `onlyne-config` owns the
/// spelling a file uses and `onlyne-proto` owns the spelling a frame uses, so
/// neither crate takes a dependency on the other's parsing — the same boundary
/// `MsgKindClass` keeps. `drive` and the two argv lists are the fields of the
/// table that reach a client, because a client is what starts (and revives) a
/// runtime; the placement is the machine's and never travels.
fn role_runtime(runtime: &onlyne_config::RuntimeSection) -> onlyne_proto::RoleRuntime {
    use onlyne_proto::Drive as WireDrive;
    onlyne_proto::RoleRuntime {
        drive: match runtime.drive {
            onlyne_config::Drive::Plugin => WireDrive::Plugin,
            onlyne_config::Drive::Acp => WireDrive::Acp,
            onlyne_config::Drive::Exec => WireDrive::Exec,
        },
        command: runtime.command.clone(),
        resume: runtime.resume.clone(),
    }
}

/// The cluster summary `wait-ready` polls.
pub fn status(state: &Arc<State>) -> ResBody {
    let Some(spec) = state.spec_snapshot() else {
        return internal(anyhow::anyhow!("the spec is unavailable"));
    };
    let connected_roles = state.roles.read().map(|table| table.len()).unwrap_or(0);
    let gateways = gateway_rows(state);
    let connected_gateways = gateways
        .iter()
        .filter(|row| row["state"] != json!("offline"))
        .count();
    ResBody::ok(json!({
        "ok": true,
        "cluster": spec.server.name,
        "version": crate::version(),
        "spec_hash": spec.semantic_hash(),
        "roles": spec.client.len(),
        "role_count": spec.client.len(),
        "gateway_count": spec.gateway.len(),
        "gateways": gateways,
        "routes": spec.route.len(),
        "channels": state.channel_count(),
        "connected_roles": connected_roles,
        "connected_gateways": connected_gateways,
        "event_head": state.event_head(),
        "uptime_s": (Utc::now() - state.start_at).num_seconds().max(0),
    }))
}

/// One row per configured gateway with its live state and capabilities.
pub fn gateway_rows(state: &Arc<State>) -> Vec<Value> {
    let spec = match state.spec_snapshot() {
        Some(spec) => spec,
        None => return Vec::new(),
    };
    let links = state.gateways.read().ok();
    spec.gateway
        .iter()
        .map(|entry| {
            let link = links.as_ref().and_then(|table| table.get(&entry.id));
            json!({
                "id": entry.id,
                "platform": entry.platform,
                "enabled": entry.enabled,
                "state": link
                    .map(|link| link.health.as_str())
                    .unwrap_or("offline"),
                "capabilities": link
                    .map(|link| link.capabilities.iter().map(|cap| cap.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default(),
                "channels": state
                    .channels_for(&entry.id)
                    .iter()
                    .map(|binding| binding.channel.clone())
                    .collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// Role registry rows with live presence.
pub fn roles(state: &Arc<State>, query: &QueryRolesArgs) -> anyhow::Result<Vec<RoleInfo>> {
    let spec = state
        .spec_snapshot()
        .ok_or_else(|| anyhow::anyhow!("the spec is unavailable"))?;
    let stored = state.ledger.list_roles()?;
    let mut rows = Vec::new();
    for entry in &spec.client {
        if let Some(wanted) = &query.role {
            if wanted != &entry.role {
                continue;
            }
        }
        let (presence, detail) = match state.roles.read().ok().and_then(|table| {
            table
                .get(&entry.role)
                .map(|link| (link.draining, link.connected_at))
        }) {
            Some((true, _)) => (Presence::Draining, None),
            Some((false, _)) => (Presence::Online, None),
            None => (Presence::Offline, None),
        };
        let sessions = state
            .ledger
            .list_sessions(onlyne_proto::QuerySessionsArgs {
                role: Some(entry.role.clone()),
                limit: 500,
                ..onlyne_proto::QuerySessionsArgs::default()
            })?
            .len() as u32;
        // The role's inbox depth, counted exactly rather than read off a capped
        // page: a display that reports the cap as the depth tells an operator a
        // saturated role looks the same as a busy one.
        let queued = state.ledger.queued_count_for(&entry.role)?;
        rows.push(RoleInfo {
            name: entry.role.clone(),
            admin: entry.admin,
            max_sessions: entry.max_sessions,
            runtime: role_runtime(&entry.runtime),
            spec_hash: stored
                .iter()
                .find(|row| row.name == entry.role)
                .map(|row| row.spec_hash.clone())
                .unwrap_or_else(|| spec.semantic_hash()),
            prose: Some(entry.prose.clone()),
            state: presence,
            sessions,
            queued,
            detail,
            edges: entry.allowed_targets.clone(),
            owes_targets: entry.owes_targets.clone(),
            aggregate: (!entry.aggregate.is_empty()).then(|| entry.aggregate.clone()),
        });
    }
    Ok(rows)
}

/// Diff the in-memory spec against the file on disk.
pub fn spec_diff(state: &Arc<State>) -> anyhow::Result<(onlyne_config::SpecDiff, String)> {
    let spec = state
        .spec_snapshot()
        .ok_or_else(|| anyhow::anyhow!("the spec is unavailable"))?;
    let layout = ServerRoot::resolve(&state.root);
    let diff = spec.load_validate(layout.spec_path())?;
    Ok((diff, spec.semantic_hash()))
}

/// What one successful spec reload produced.
#[derive(Debug, Clone)]
pub struct ReloadOutcome {
    /// `SpecDiff::render()` text for the operator log.
    pub render: String,
    pub spec_hash: String,
    pub roles: usize,
    pub gateways: usize,
    pub routes: usize,
}

/// Re-read `spec.toml`, rebuild the ACL, and announce the reload.
///
/// Both `AdminOp::Reload` and the SIGHUP listener call this one body. A failed
/// reload keeps the in-memory spec and records `fault{kind:"spec_reload_failed"}`.
pub fn reload_spec(state: &Arc<State>) -> Result<ReloadOutcome, String> {
    let layout = ServerRoot::resolve(&state.root);
    let Some(current) = state.spec_snapshot() else {
        return Err("the spec is unavailable".to_string());
    };
    let path = layout.spec_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) => {
            return Err(reload_failure(
                state,
                &format!("{}: {source}", path.display()),
            ));
        }
    };
    let file = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("spec.toml");
    let next = match Spec::parse_named(&text, file) {
        Ok(next) => next,
        Err(error) => return Err(reload_failure(state, &error.to_string())),
    };
    let diff = SpecDiff::between(&current, &next);
    let acl = match server_state::acl_from_spec(&next) {
        Ok(acl) => acl,
        Err(error) => return Err(reload_failure(state, &error.to_string())),
    };
    let hash = next.semantic_hash();
    let names: Vec<String> = next.client.iter().map(|entry| entry.role.clone()).collect();
    let updated_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    for entry in &next.client {
        if let Err(error) = state.ledger.upsert_role(&RoleRow {
            name: entry.role.clone(),
            key: entry.key.clone(),
            admin: entry.admin,
            max_sessions: i64::from(entry.max_sessions),
            spec_hash: hash.clone(),
            updated_at: updated_at.clone(),
        }) {
            return Err(reload_failure(state, &error.to_string()));
        }
    }
    if let Err(error) = state.ledger.remove_role_missing_from(&names) {
        return Err(reload_failure(state, &error.to_string()));
    }
    if let Some(previous) = state.replace_spec(next.clone())
        && previous.hook != next.hook
    {
        // Hook workers are bound when the server starts, so a reload cannot
        // rebind them in place. Saying so is the honest answer; silently
        // running the old policy would be a hook nobody can see is stale
        // (`docs/v2-CONTRACT.md` §"Slice 7").
        tracing::warn!(
            "the [[hook]] table changed; hooks are bound at server start, so restart the server to apply it"
        );
    }
    state.replace_acl(acl);
    let event = Event::SpecReloaded(SpecReloaded {
        spec_hash: hash.clone(),
        roles: next.client.len() as u32,
        gateways: next.gateway.len() as u32,
        routes: next.route.len() as u32,
    });
    if let Err(error) = state.emit(event) {
        return Err(reload_failure(state, &error.to_string()));
    }
    Ok(ReloadOutcome {
        render: diff.render(),
        spec_hash: hash,
        roles: next.client.len(),
        gateways: next.gateway.len(),
        routes: next.route.len(),
    })
}

/// Record the failed reload and hand the message back.
fn reload_failure(state: &State, message: &str) -> String {
    let _ = faults::record(state, FaultDraft::spec_reload_failed(message));
    message.to_string()
}

/// The `AdminOp::Reload` answer.
pub fn reload(state: &Arc<State>) -> ResBody {
    match reload_spec(state) {
        Ok(outcome) => ResBody::ok(json!({
            "render": outcome.render,
            "spec_hash": outcome.spec_hash,
            "roles": outcome.roles,
            "gateways": outcome.gateways,
            "routes": outcome.routes,
        })),
        Err(message) => ResBody::err(ErrorCode::Invalid, message, Some("spec.toml".to_string())),
    }
}

/// Build one control envelope from a role.
pub fn control_envelope(from: &str, op: &ControlOp, to: Option<&str>) -> onlyne_proto::Envelope {
    let task_id = op.task_id().to_string();
    let target = to.unwrap_or(from).to_string();
    let mut envelope = onlyne_proto::new_envelope(
        MsgKind::Control,
        Principal::role(from),
        Principal::role(target.clone()),
        Body::text(op.name()),
        Some(Causality::root(task_id.clone())),
    )
    .unwrap_or_else(|_| onlyne_proto::Envelope {
        protocol: onlyne_proto::PROTOCOL_VERSION,
        id: onlyne_proto::new_id(),
        op_id: Some(onlyne_proto::new_op_id()),
        kind: MsgKind::Control,
        from: Principal::role(from),
        to: Principal::role(target.clone()),
        control: None,
        causality: Some(Causality::root(task_id.clone())),
        body: Body::text(op.name()),
        ts: Utc::now(),
        ttl_ms: None,
        admin: false,
    });
    envelope.control = Some(op.clone());
    envelope
}

/// Map one relay reply onto a response body.
///
/// A reply whose row targets a gateway conversation also schedules the outbound
/// pump: the row is durable by then, and a push to a mounted gateway is the only
/// delivery a platform conversation can take (plan §7 line 293).
pub fn reply_body(
    state: &std::sync::Arc<crate::state::State>,
    reply: anyhow::Result<RelayReply>,
) -> ResBody {
    match reply {
        Ok(RelayReply::Accepted(outcome)) => {
            if outcome.pump_outbound {
                crate::gateway_host::schedule_pump(state);
            }
            ResBody::ok(relay::receipt_json(&outcome.receipt))
        }
        Ok(RelayReply::Duplicate(outcome)) => {
            if outcome.pump_outbound {
                crate::gateway_host::schedule_pump(state);
            }
            ResBody::err_with_data(
                ErrorCode::Duplicate,
                format!("duplicate op_id: replaying {}", outcome.receipt.msg_id),
                Some("op_id".to_string()),
                relay::receipt_json(&outcome.receipt),
            )
        }
        Ok(RelayReply::Rejected(reject)) => reject.body(),
        Err(error) => internal(error),
    }
}

/// The response for an internal failure.
pub fn internal(error: anyhow::Error) -> ResBody {
    ResBody::err(ErrorCode::Internal, error.to_string(), None)
}
