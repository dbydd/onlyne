use crate::handoff::{self, Denial};
use crate::intent::stamp_op_id;
use crate::runloop::ClientInit;
use anyhow::{Context, Result, anyhow};
use onlyne_adapter::AdapterIo;
use onlyne_layout::RoleWorkspace;
use onlyne_net::conn::{ClientConn, ConnReadiness, dial};
use onlyne_net::{ConnSettings, KeyPair, NetError};
use onlyne_proto::{
    AckArgs, AdapterMsg, AgentPhase, AssignArgs, Body, Capability, Causality, ClientOp, ControlOp,
    DeliveryPhase, Envelope, Frame, Handoff, HandshakeArgs, HostOp, Lifecycle, MsgKind, Outcome,
    PROTOCOL_VERSION, Principal, RecoveryPhase, RecycleArgs, Report, ResBody, ResourcePhase,
    SessionProjection, SessionSyncArgs, Welcome, new_envelope,
};
use onlyne_session::{
    Bridge, IgnoredReason, LifecycleEvent, Observation, SessionBackend, SessionLedger,
    SessionRecord, SessionRef, SpawnSpec, Verdict, Version, apply_persist, feed_created,
    feed_dispatched, feed_ready, feed_resource_closed, settle,
};
use onlyne_store::ClientStore;
use parking_lot::Mutex;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct DispatchState {
    inner: Arc<Mutex<DispatchInner>>,
}
struct DispatchInner {
    pub role: String,
    pub workspace: PathBuf,
    pub command: Vec<String>,
    pub max_sessions: u32,
    pub reuse: bool,
    /// Downstream roles a session of this role owes a handoff to, from the
    /// server's spec slice (`relay_required`). Empty is the default and means
    /// the guard is off.
    pub relay_required: Vec<String>,
    /// The count form of the same policy (`relay_count`).
    pub relay_count: Option<u32>,
    pub backend: Arc<dyn SessionBackend>,
    pub store: ClientStore,
    pub bridge: Bridge,
    pub sessions: HashMap<String, SessionSlot>,
    /// Live link, installed by the runloop while the connection is up.
    pub outbox: Option<Arc<dyn Outbox>>,
    /// Flag the runloop and the dispatcher share while the link is down.
    pub accept_new: Arc<AtomicBool>,
    /// Whether the role holds a ready server link. The runloop owns it, and the
    /// adapter socket reports it to the `status` verb.
    pub link_up: Arc<AtomicBool>,
    /// Aggregate name this role supervises, empty for a plain role.
    pub cluster_ref: String,
    /// The server's topology name, read from `welcome.cluster` (the server's own
    /// `spec.toml [server] name`). A host backend uses it as the address of the
    /// tree it puts sessions into: herdr keeps one workspace per server root,
    /// labelled after this name. Empty until the first welcome arrives.
    pub topology: String,
    /// The adapter connection serving each session of this role, keyed by the
    /// session id the plugin mounted with (`ONLYNE_SESSION_ID`). A plugin the
    /// client spawned names the one session it was spawned for, so a task
    /// never rides the connection of an earlier one.
    pub transports: HashMap<String, (AdapterIo, Vec<Capability>)>,
    /// A plugin that mounted naming no session: an always-running agent
    /// waiting for this role's next assignment (plan §6 line 285).
    pub parked: Option<(AdapterIo, Vec<Capability>)>,
    /// Zero-activity clock for running tasks. Applied persists refresh it.
    pub stall: crate::stall::StallWatch,
    /// A plugin connection that mounted a session a live connection already
    /// serves: the agent that dropped came back after a newer session took the
    /// task. It is served nothing, and what it sends is held rather than sent,
    /// keyed by the name it mounted with. The capabilities that mount came with
    /// are kept beside it, because a session whose live connection goes away
    /// hands itself to the first connection that was holding for it.
    pub revived: Vec<(String, AdapterIo, Vec<Capability>)>,
    /// What those held connections sent, keyed by the task whose completion
    /// carries it. `on_out` drains the key before it routes, so the recipient
    /// reads one relay per downstream role.
    pub held_handoffs: HashMap<String, Vec<Handoff>>,
    /// Connections inside one of their own inbound frames right now.
    ///
    /// A frame handler runs to completion before `adapter_socket` answers the
    /// frame, so a host frame written on the same connection during that handler
    /// leaves first. The bye sweep in `retire_revived` skips these connections.
    pub in_frame: Vec<AdapterIo>,
}

#[derive(Clone)]
pub struct SessionSlot {
    pub session: SessionRef,
    pub family: String,
    pub task_id: Option<String>,
    pub ready: bool,
    /// Payload held until the adapter reports ready, which keeps the ready
    /// barrier of §6 ahead of the `assign` frame.
    pub payload: Option<Envelope>,
    /// Delivery handle, owed back to the server as one `ack`.
    pub msg_id: Option<String>,
    /// Sender of the payload this session serves, kept for its `Completion`.
    pub origin: Option<Principal>,
    /// How deep the task this slot serves sits in its chain, read off the
    /// envelope that arrived with it. A handoff the session reports afterwards
    /// is one hop below this, which is what `onlyne handoff` computes too.
    pub hop: u32,
    /// When the connection serving this session last ended without a `detach`
    /// frame, and `None` while a connection is attached or one never left. The
    /// reconnect grace of `[client] reconnect_grace_secs` reads it: an agent
    /// that comes back inside the window clears it and keeps its session.
    pub dropped_at: Option<Instant>,
    /// Whether this session's task has been taken by a newer session, leaving
    /// this slot served only by a connection that came back for it. A read-only
    /// slot is handed no assignment and no note, and what its agent sends is
    /// held for the completion that merges it.
    pub read_only: bool,
}

/// Write one ack into the durable intent queue.
fn store_ack(inner: &DispatchInner, mut ack: AckArgs) {
    if ack.op_id.is_none() {
        ack.op_id = Some(onlyne_proto::new_op_id());
    }
    let Some(op_id) = ack.op_id.clone() else {
        return;
    };
    match serde_json::to_value(ClientOp::Ack(ack)) {
        Ok(value) => {
            if let Err(error) = inner.store.enqueue_intent(&op_id, &value) {
                tracing::warn!(error = %error, "settled ack was not stored");
            }
        }
        Err(error) => tracing::warn!(error = %error, "settled ack did not serialize"),
    }
}

/// Queue one envelope into the durable intent table while the dispatch lock is
/// already held. The `op_id` rule and the validation are `enqueue_outbound`'s.
fn queue_outbound_locked(inner: &mut DispatchInner, envelope: &Envelope) -> Result<String> {
    let mut stamped = envelope.clone();
    let op_id = stamp_op_id(&mut stamped);
    stamped
        .validate()
        .map_err(|error| anyhow!(error.to_string()))?;
    inner
        .store
        .enqueue_intent(&op_id, &serde_json::to_value(&stamped)?)?;
    Ok(op_id)
}

/// The name one held frame is addressed to.
///
/// A role recipient keeps its own name, which is the role the merged relay is
/// addressed to. Any other recipient keeps the spelling the operator reads in a
/// session listing, and the merged relay addressed to it is refused and recorded
/// rather than quietly dropped: a read-only session cannot answer a conversation
/// it no longer serves.
fn held_recipient(to: &Principal) -> String {
    to.role_name()
        .map(str::to_string)
        .unwrap_or_else(|| to.to_string())
}

/// Whether one session slot is the session an adapter mount named.
///
/// The mount carries the id the client spawned the plugin with
/// (`ONLYNE_SESSION_ID`), which is the slot's key and its stored reference; a
/// slot a later task reused also answers to the task it serves.
fn names_session(key: &str, slot: &SessionSlot, session_id: &str) -> bool {
    key == session_id
        || slot.session.task_id == session_id
        || slot.task_id.as_deref() == Some(session_id)
}

fn has_attached_transport(inner: &DispatchInner, key: &str, slot: &SessionSlot) -> bool {
    inner
        .transports
        .keys()
        .any(|session_id| names_session(key, slot, session_id))
}

/// The task one slot answers for: its current binding, or the task its session
/// was spawned for while no task is bound.
fn slot_task(slot: &SessionSlot) -> String {
    slot.task_id
        .clone()
        .unwrap_or_else(|| slot.session.task_id.clone())
}

/// The slot one adapter mount names, answered as its key.
fn slot_key_named(inner: &DispatchInner, session_id: &str) -> Option<String> {
    inner
        .sessions
        .iter()
        .find(|(key, slot)| names_session(key, slot, session_id))
        .map(|(key, _)| key.clone())
}

/// The slot serving one task, preferring the one that still holds delivery
/// rights.
///
/// Two slots answer to one task only when a session came back for a task a newer
/// session already took, and `HashMap` order decides which one a search reaches
/// first. Every handle that belongs to the task — its delivery `msg_id` above
/// all — has to land on the session that is actually serving it, or the ack the
/// retry earns would be written into a slot that can never answer and the server
/// row would sit in flight.
fn slot_key_serving_task(inner: &DispatchInner, task_id: &str) -> Option<String> {
    let mut bound = None;
    for (key, slot) in inner.sessions.iter() {
        if slot.task_id.as_deref() != Some(task_id) {
            continue;
        }
        if !slot.read_only {
            return Some(key.clone());
        }
        if bound.is_none() {
            bound = Some(key.clone());
        }
    }
    bound
}

/// Whether a connection other than `io` is the one serving one slot.
fn attached_to_other(inner: &DispatchInner, key: &str, slot: &SessionSlot, io: &AdapterIo) -> bool {
    inner.transports.iter().any(|(session_id, (live, _))| {
        !live.same_connection(io) && names_session(key, slot, session_id)
    })
}

/// Whether one connection is held read-only: it mounted a session this client
/// already serves through a different live connection.
fn is_revived_connection(inner: &DispatchInner, io: &AdapterIo) -> bool {
    inner
        .revived
        .iter()
        .any(|(_, revived, _)| revived.same_connection(io))
}

/// Record a returning connection as read-only, once per connection.
fn record_revived_connection(
    inner: &mut DispatchInner,
    session_id: &str,
    io: AdapterIo,
    capabilities: Vec<Capability>,
) {
    if !is_revived_connection(inner, &io) {
        inner
            .revived
            .push((session_id.to_string(), io, capabilities));
    }
}

/// Give one session back to the oldest connection that was held read-only for it.
///
/// A held connection is read-only only while another connection serves its
/// session, so the moment that connection goes is the moment the held one becomes
/// the session's only transport. Without this, a plugin that redials while the
/// client still holds the dead socket behind it is silenced for the rest of the
/// session: the assignment it came for is written to a connection nobody reads,
/// and nothing promotes it later. The demotion lifts with the promotion, so the
/// reconnected agent keeps both its session and its delivery rights.
fn promote_held_connection(inner: &mut DispatchInner, key: &str) {
    let attached = inner
        .sessions
        .get(key)
        .is_some_and(|slot| has_attached_transport(inner, key, slot));
    if attached {
        return;
    }
    let held = inner.revived.iter().position(|(name, _, _)| {
        slot_key_named(inner, name).is_some_and(|held_key| held_key == key)
    });
    let Some(index) = held else { return };
    let (name, io, capabilities) = inner.revived.remove(index);
    if let Some(slot) = inner.sessions.get_mut(key) {
        slot.read_only = false;
    }
    tracing::info!(
        session = %name,
        "a held connection takes the session its predecessor left"
    );
    attach_transport_locked(inner, &name, io, capabilities);
}

/// Settle what one mounting connection means for the session it names.
///
/// A mount that finds nothing serving its session takes it and clears the clock
/// [`DispatchState::release_connection`] started: that is the agent that came
/// back inside the reconnect grace, and the always-running agent serving task
/// after task lives in this path. A mount that finds the session already served
/// takes nothing — either another connection holds that very slot, or the task it
/// names now answers from a slot of its own, which is the case where a newer
/// session was spawned to retry the work while the old agent's process came back.
/// Such a connection is recorded read-only, and the slot it names is demoted too
/// when it owns a slot of its own.
///
/// Every binding path runs this one judgement, including the ready report, which
/// reaches an agent without writing a transport. Concurrency is what decides, not
/// the drop clock: the retry that claims an unclaimed session is served, and the
/// connection that returns to a session already served is held, whichever of the
/// two mounted first. [`DispatchState::release_connection`] promotes a held
/// connection when the live one it waited behind goes away, so a plugin that
/// redials over a socket the client has not yet seen die still gets its session.
fn note_binding_locked(inner: &mut DispatchInner, session_id: &str, io: &AdapterIo) -> bool {
    if is_revived_connection(inner, io) {
        return false;
    }
    let Some(key) = slot_key_named(inner, session_id) else {
        return true;
    };
    let Some(slot) = inner.sessions.get(&key) else {
        return true;
    };
    let task = slot_task(slot);
    let taken = attached_to_other(inner, &key, slot, io);
    let moved_on = !taken
        && inner.sessions.iter().any(|(other, other_slot)| {
            *other != key
                && slot_task(other_slot) == task
                && attached_to_other(inner, other, other_slot, io)
        });
    let revived = taken || moved_on;
    if let Some(slot) = inner.sessions.get_mut(&key) {
        if revived {
            // Only the spelling where this name's own slot is still served by
            // the newer connection leaves a slot of its own to silence; when it
            // is, the slot belongs to that live connection and keeps its rights.
            slot.read_only = moved_on;
        } else {
            slot.dropped_at = None;
            slot.read_only = false;
        }
    }
    if revived {
        tracing::warn!(
            session = %session_id,
            task = %task,
            "a plugin mounted a session this client already serves; it is held read-only"
        );
        return false;
    }
    true
}

/// Attach one plugin connection to the session it names, or hold it read-only.
///
/// This is the only place a mount becomes a transport, so the read-only
/// connection of §1 (b) never lands in `transports` and never steals the
/// assignment, delivery, or note addressed to the connection that serves the
/// session now. Answers whether the connection took the session.
fn attach_transport_locked(
    inner: &mut DispatchInner,
    session_id: &str,
    io: AdapterIo,
    capabilities: Vec<Capability>,
) -> bool {
    if !note_binding_locked(inner, session_id, &io) {
        record_revived_connection(inner, session_id, io, capabilities);
        return false;
    }
    inner
        .transports
        .insert(session_id.to_string(), (io, capabilities));
    true
}

/// One plugin connection held inside an inbound frame it is still being answered.
///
/// The bye sweep in `retire_revived` leaves such a connection alone, which keeps a
/// bye behind the response to the frame. A plugin's bye handler drops the socket and
/// rejects every request awaiting an answer, so a bye that overtakes the response
/// turns work the ledger already holds into a failure the agent reports again.
#[must_use = "the connection stops being held as soon as the guard is dropped"]
pub struct FrameGuard<'a> {
    state: &'a DispatchState,
    io: AdapterIo,
}

impl Drop for FrameGuard<'_> {
    fn drop(&mut self) {
        self.state
            .inner
            .lock()
            .in_frame
            .retain(|held| !held.same_connection(&self.io));
    }
}

impl DispatchState {
    /// Hold `io` for as long as one of its inbound frames is being handled.
    pub fn hold_frame(&self, io: &AdapterIo) -> FrameGuard<'_> {
        self.inner.lock().in_frame.push(io.clone());
        FrameGuard {
            state: self,
            io: io.clone(),
        }
    }

    pub fn new(
        role: impl Into<String>,
        workspace: impl Into<PathBuf>,
        command: Vec<String>,
        max_sessions: u32,
        reuse: bool,
        backend: Arc<dyn SessionBackend>,
        store: ClientStore,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(DispatchInner {
                role: role.into(),
                workspace: workspace.into(),
                command,
                max_sessions,
                reuse,
                relay_required: Vec::new(),
                relay_count: None,
                backend,
                store,
                bridge: Bridge::new(),
                sessions: HashMap::new(),
                outbox: None,
                accept_new: Arc::new(AtomicBool::new(true)),
                link_up: Arc::new(AtomicBool::new(false)),
                cluster_ref: String::new(),
                topology: String::new(),
                transports: HashMap::new(),
                parked: None,
                stall: crate::stall::StallWatch::new(),
                revived: Vec::new(),
                held_handoffs: HashMap::new(),
                in_frame: Vec::new(),
            })),
        }
    }

    pub fn session_count(&self) -> usize {
        self.inner.lock().sessions.len()
    }
    pub fn role(&self) -> String {
        self.inner.lock().role.clone()
    }
    pub fn command(&self) -> Vec<String> {
        self.inner.lock().command.clone()
    }
    pub fn backend_name(&self, task_id: &str) -> Option<String> {
        self.inner
            .lock()
            .sessions
            .values()
            .find(|slot| slot.task_id.as_deref() == Some(task_id))
            .map(|slot| slot.session.backend.clone())
    }
    /// The terminal-fact stream of a backend that drives its own agent.
    pub fn outcome_feed(&self) -> Option<onlyne_session::OutcomeFeed> {
        self.inner.lock().backend.outcomes()
    }
    /// Bind one adapter connection to the session it named.
    ///
    /// The name is the session id the client spawned the plugin with, which is
    /// enough on its own: a plugin that mounts before the client staged its
    /// session is remembered here and takes the payload the moment it is
    /// staged, and a plugin that mounts after finds its session waiting.
    pub fn bind_adapter(&self, session_id: &str, io: AdapterIo, capabilities: Vec<Capability>) {
        attach_transport_locked(&mut self.inner.lock(), session_id, io, capabilities);
    }

    /// Remember the delivery handle for one task.
    ///
    /// The handle goes to the session serving the task, not to a read-only slot
    /// that came back for it, so the ack this earns answers the live delivery.
    pub fn attach_msg_id(&self, task_id: &str, msg_id: &str) {
        let mut inner = self.inner.lock();
        let Some(key) = slot_key_serving_task(&inner, task_id) else {
            return;
        };
        if let Some(slot) = inner.sessions.get_mut(&key) {
            slot.msg_id = Some(msg_id.to_string());
        }
    }
    /// Queue a delivery ack when the plugin refuses an assignment.
    ///
    /// An accepted assignment is not terminal for the server row: the normal
    /// completion path still settles that delivery. A refused assignment is a
    /// terminal local decision, so it uses the same durable ack queue as every
    /// other delivery settlement.
    pub fn push_assign_ack(&self, ack: onlyne_proto::AssignAckArgs) -> bool {
        if ack.accepted {
            return false;
        }
        let mut inner = self.inner.lock();
        let msg_id = slot_key_serving_task(&inner, &ack.task_id)
            .and_then(|key| inner.sessions.get_mut(&key))
            .and_then(|slot| slot.msg_id.take());
        let Some(msg_id) = msg_id else {
            return false;
        };
        store_ack(
            &inner,
            AckArgs {
                msg_id,
                op_id: None,
                accepted: false,
                reason: ack.reason.or_else(|| Some("assign rejected".to_string())),
            },
        );
        true
    }

    /// Queue an ack the client owes the server.
    ///
    /// Record an ack the client owes the server.
    ///
    /// The ack is durable: D11's control plane is at-least-once, and a settled
    /// session whose ack is lost leaves the row in flight forever. The intent
    /// queue carries it across a link that is down, and the flusher is the
    /// sender.
    pub fn push_settled(&self, ack: AckArgs) {
        store_ack(&self.inner.lock(), ack);
    }

    /// The role slice the dispatcher currently runs.
    pub fn role_slice(&self) -> crate::slice::RoleSlice {
        let inner = self.inner.lock();
        crate::slice::RoleSlice {
            command: inner.command.clone(),
            max_sessions: inner.max_sessions,
            reuse: inner.reuse,
            relay_required: inner.relay_required.clone(),
            relay_count: inner.relay_count,
        }
    }

    /// Task ids currently occupying a live slot.
    pub fn live_task_ids(&self) -> std::collections::HashSet<String> {
        let inner = self.inner.lock();
        inner
            .sessions
            .values()
            .filter_map(|slot| slot.task_id.clone())
            .collect()
    }

    /// Sorted live-slot task ids for a hello claim.
    pub fn hello_live_tasks(&self) -> Vec<String> {
        crate::claim::from_slots(self.live_task_ids())
    }

    /// Start the stall clock for a newly assigned task.
    pub fn note_stall_assigned(&self, task_id: &str, now: Instant) {
        self.inner.lock().stall.note_assigned(task_id, now);
    }

    /// Refresh the stall clock after an Applied persist.
    pub fn note_stall_applied(&self, task_id: &str, now: Instant) {
        self.inner.lock().stall.note_applied(task_id, now);
    }

    /// Task ids whose freeze exceeds `threshold_secs` in this episode.
    /// Exited projections retire their remaining progress clocks.
    pub fn stall_due(&self, now: Instant, threshold_secs: u64) -> Vec<String> {
        let mut inner = self.inner.lock();
        let due = inner.stall.due(now, threshold_secs);
        let mut active = Vec::with_capacity(due.len());
        for task_id in due {
            if session_exited(&inner, &task_id) {
                inner.stall.forget(&task_id);
            } else {
                active.push(task_id);
            }
        }
        active
    }

    /// Remember that this freeze episode has been reported.
    pub fn mark_stalled(&self, task_id: &str) {
        self.inner.lock().stall.mark_reported(task_id);
    }

    /// Observation-only stall fault for an active task, carrying the stored
    /// watermark. An Exited projection retires its progress clock before the
    /// send boundary.
    pub fn stall_report(&self, task_id: &str) -> Option<Report> {
        let mut inner = self.inner.lock();
        let row = inner.store.get_session(task_id).ok().flatten();
        if row.as_ref().is_some_and(|row| {
            phase(&row.public_lifecycle, Lifecycle::Created) == Lifecycle::Exited
        }) {
            inner.stall.forget(task_id);
            return None;
        }
        Some(crate::stall::report(
            task_id,
            Some(task_id.to_string()),
            row.as_ref().map(|row| row.generation as u64),
            row.as_ref().map(|row| row.seq as u64),
        ))
    }

    /// Whether any adapter is currently mounted (named or parked).
    pub fn has_mounted_adapter(&self) -> bool {
        let inner = self.inner.lock();
        !inner.transports.is_empty() || inner.parked.is_some()
    }

    /// Adopt a role slice: the one `welcome` carried, or the one a reload's
    /// role row carries.
    pub fn reconfigure(&self, slice: crate::slice::RoleSlice) {
        let mut inner = self.inner.lock();
        inner.command = slice.command;
        inner.max_sessions = slice.max_sessions;
        inner.reuse = slice.reuse;
        inner.relay_required = slice.relay_required;
        inner.relay_count = slice.relay_count;
    }

    /// Role prose last cached from `welcome`.
    pub fn role_prose(&self) -> String {
        let inner = self.inner.lock();
        inner
            .store
            .prose(&inner.role)
            .ok()
            .flatten()
            .map(|(prose, _)| prose)
            .unwrap_or_default()
    }

    /// Queue an outbound envelope before its first write and answer its op_id.
    ///
    /// The queue keys every row by an `op_id`, and the proto requires that key
    /// only for the non-note kinds, so a note that arrives without one gets a
    /// fresh client-minted id here: the row is keyed and what it replays is the
    /// whole stamped envelope. A non-note keeps the id it brought, so a
    /// re-delivered task still dedups on its original one.
    pub fn enqueue_outbound(&self, envelope: &Envelope) -> Result<String> {
        queue_outbound_locked(&mut self.inner.lock(), envelope)
    }

    /// Take one plugin `send` frame and answer what the plugin is told.
    ///
    /// A live connection's envelope goes to the durable outbound queue exactly as
    /// it always has, and the answer keeps the shape the plugin reads. A frame
    /// from a connection this client holds read-only is held instead (§1 (c)): it
    /// leaves as part of the merged handoff its task's completion routes, so the
    /// recipient sees one message per downstream role and can tell which session
    /// wrote which half of it.
    pub fn plugin_send(&self, io: &AdapterIo, envelope: &Envelope) -> Result<serde_json::Value> {
        let mut inner = self.inner.lock();
        let Some(session_id) = inner
            .revived
            .iter()
            .find(|(_, revived, _)| revived.same_connection(io))
            .map(|(session_id, _, _)| session_id.clone())
        else {
            let op_id = queue_outbound_locked(&mut inner, envelope)?;
            return Ok(serde_json::json!({"queued": true, "op_id": op_id}));
        };
        let task = slot_key_named(&inner, &session_id)
            .and_then(|key| inner.sessions.get(&key))
            .map(slot_task)
            .unwrap_or(session_id);
        let held = Handoff {
            to_role: held_recipient(&envelope.to),
            text: Some(envelope.body.text.clone().unwrap_or_default()),
        };
        tracing::warn!(
            task = %task,
            to = %held.to_role,
            "a read-only session's send is held for that task's completion"
        );
        inner.held_handoffs.entry(task).or_default().push(held);
        Ok(serde_json::json!({"queued": true, "held": true}))
    }

    /// The flag the runloop and the dispatcher share.
    pub fn accept_new(&self) -> Arc<AtomicBool> {
        self.inner.lock().accept_new.clone()
    }

    /// Whether the role holds a ready server link.
    pub fn link_up(&self) -> bool {
        self.inner.lock().link_up.load(Ordering::SeqCst)
    }

    /// Record that the server link came up or went down.
    pub fn set_link_up(&self, up: bool) {
        self.inner.lock().link_up.store(up, Ordering::SeqCst);
    }

    /// Aggregate name this role supervises, empty for a plain role.
    pub fn cluster_ref(&self) -> String {
        self.inner.lock().cluster_ref.clone()
    }

    /// Record the server's topology name, read from `welcome.cluster`.
    ///
    /// Each spawned session carries it as `ONLYNE_CLUSTER`, which is how a host
    /// backend (herdr) addresses the tree it splits panes into. The runloop calls
    /// this on every welcome, so a server that reloads under a new name is
    /// followed by the sessions spawned after that point.
    pub fn set_topology(&self, cluster: &str) {
        self.inner.lock().topology = cluster.trim().to_string();
    }

    /// The topology name recorded from `welcome`, empty before the first welcome.
    pub fn topology(&self) -> String {
        self.inner.lock().topology.clone()
    }

    /// Record the aggregate name once, so every report keeps the same value
    /// across a reconnect.
    pub fn set_cluster_ref(&self, aggregate: impl Into<String>) {
        self.inner.lock().cluster_ref = aggregate.into();
    }

    /// Park one plugin connection as this role's waiting agent.
    ///
    /// Only a mount that names no session parks: it is a plugin that attached
    /// before any work existed, so it takes the next session this role stages
    /// (plan §6 line 285).
    pub fn park_transport(&self, io: AdapterIo, capabilities: Vec<Capability>) {
        self.inner.lock().parked = Some((io, capabilities));
    }

    /// Claim this role's waiting agent for one staged session.
    ///
    /// An always-running plugin mounts naming no session, so the park holds the
    /// only connection that can serve the session staged next (plan §6 line 285).
    /// The claim binds that connection to the session it takes, because the
    /// later tasks a `reuse` role hands to the same session ride that connection
    /// too. A claim left unbound strands those tasks: the session has a payload
    /// and this client holds no record of the socket that serves it.
    fn claim_parked_transport(&self, session_id: &str) -> Option<(AdapterIo, Vec<Capability>)> {
        let mut inner = self.inner.lock();
        let (io, capabilities) = inner.parked.take()?;
        if !attach_transport_locked(&mut inner, session_id, io.clone(), capabilities.clone()) {
            // The waiting agent is an older connection returning for a session
            // the role already serves: it holds the socket and takes nothing.
            return None;
        }
        Some((io, capabilities))
    }

    /// The connection that serves one session, when its plugin is attached.
    ///
    /// A plugin names the session it was spawned for, and a later task of a
    /// reused session arrives under its own task id, so the slot's key is the
    /// other spelling worth trying.
    pub fn session_transport(&self, session_id: &str) -> Option<(AdapterIo, Vec<Capability>)> {
        let inner = self.inner.lock();
        if let Some(transport) = inner.transports.get(session_id) {
            return Some(transport.clone());
        }
        let key = inner
            .sessions
            .iter()
            .find(|(key, slot)| names_session(key, slot, session_id))
            .map(|(key, _)| key.clone())?;
        inner.transports.get(&key).cloned()
    }

    /// Whether one task names a session this client holds, in memory or in its
    /// durable session rows.
    pub fn holds_task(&self, task_id: &str) -> bool {
        let inner = self.inner.lock();
        inner
            .sessions
            .values()
            .any(|slot| slot.task_id.as_deref() == Some(task_id))
            || inner.store.get_session(task_id).ok().flatten().is_some()
    }

    /// Tell the plugin serving one session to tear itself down, when that plugin
    /// implements `recycle`. A plugin without the capability is skipped: the
    /// caller's backend close stops the process either way.
    pub async fn recycle_plugin(&self, task_id: &str, reason: &str, outcome: Option<Outcome>) {
        let Some((io, capabilities)) = self.session_transport(task_id) else {
            return;
        };
        if missing_capability(&capabilities, Capability::Recycle) {
            tracing::debug!(
                task = %task_id,
                "plugin does not implement recycle; the host closes the resource"
            );
            return;
        }
        let args = RecycleArgs {
            task_id: task_id.to_string(),
            reason: reason.to_string(),
            outcome,
        };
        if let Err(error) = io.notify(AdapterMsg::Host(HostOp::Recycle(args))).await {
            tracing::warn!(error = %error, task = %task_id, "recycle frame did not reach the plugin");
        }
    }

    /// Ask the plugin serving one session for a fresh observation.
    ///
    /// The plugin answers with a heartbeat report, which is the reducer's
    /// evidence and the projection the operator reads. A session whose plugin is
    /// gone gets no frame, and the republished projection is what says so.
    pub async fn probe_plugin(&self, task_id: &str) {
        let Some((io, _)) = self.session_transport(task_id) else {
            return;
        };
        let request = serde_json::json!({"task_id": task_id});
        if let Err(error) = io.notify(AdapterMsg::Host(HostOp::Probe(request))).await {
            tracing::warn!(error = %error, task = %task_id, "probe frame did not reach the plugin");
        }
    }

    /// Release the bindings served by one plugin connection.
    ///
    /// A graceful detach retires each idle session because the agent that could
    /// reuse its resource has left. An attached transport preserves the idle
    /// resource because reuse remains possible. A connection ending through
    /// another path preserves the slot and resource for an agent reconnection and
    /// starts the reconnect clock on it, which is what bounds how long a session
    /// waits for an agent that is never coming back. Every released binding
    /// retires its task progress clock. A slot carrying work remains under
    /// lifecycle ownership.
    pub fn release_connection(
        &self,
        session_id: Option<&str>,
        io: &AdapterIo,
        graceful_detach: bool,
    ) {
        let mut inner = self.inner.lock();
        // A read-only connection ending is not the session losing its agent: the
        // live connection still serves it, and its drop clock stays untouched.
        let revived_connection = {
            let before = inner.revived.len();
            inner
                .revived
                .retain(|(_, revived, _)| !revived.same_connection(io));
            before != inner.revived.len()
        };
        if inner
            .parked
            .as_ref()
            .is_some_and(|(parked, _)| parked.same_connection(io))
        {
            inner.parked = None;
        }
        let released: Vec<String> = inner
            .transports
            .iter()
            .filter(|(session, (transport, _))| {
                transport.same_connection(io)
                    && session_id.is_none_or(|mounted| mounted == session.as_str())
            })
            .map(|(session, _)| session.clone())
            .collect();
        let served_tasks: Vec<String> = released
            .iter()
            .map(|session| {
                inner
                    .sessions
                    .iter()
                    .find(|(key, slot)| names_session(key, slot, session))
                    .map(|(_, slot)| {
                        slot.task_id
                            .clone()
                            .unwrap_or_else(|| slot.session.task_id.clone())
                    })
                    .unwrap_or_else(|| session.clone())
            })
            .collect();
        for task_id in served_tasks {
            inner.stall.forget(&task_id);
        }
        for session in &released {
            inner.transports.remove(session);
        }
        if !graceful_detach && !revived_connection {
            // The agent left without saying so. Its session keeps its slot and
            // its resource, and the reconnect grace of `[client]
            // reconnect_grace_secs` starts counting from here.
            let now = Instant::now();
            for session in &released {
                if let Some((_, slot)) = inner
                    .sessions
                    .iter_mut()
                    .find(|(key, slot)| names_session(key, slot, session))
                {
                    slot.dropped_at = Some(now);
                }
            }
        }
        for session in &released {
            // Nothing serves this name any more, so the first connection that
            // mounted it read-only behind the one that just went becomes its
            // transport; a session with no such connection keeps waiting out the
            // reconnect grace, which is the sweep's to answer.
            if let Some(key) = slot_key_named(&inner, session) {
                promote_held_connection(&mut inner, &key);
            }
        }
        if graceful_detach {
            let idle: Vec<String> = released
                .iter()
                .filter_map(|session| {
                    inner
                        .sessions
                        .iter()
                        .find(|(key, slot)| {
                            names_session(key, slot, session) && slot.task_id.is_none()
                        })
                        .map(|(key, _)| key.clone())
                })
                .collect();
            for key in idle {
                let reason = inner
                    .sessions
                    .get(&key)
                    .and_then(|slot| stored_close_reason(&inner, &slot.session.task_id))
                    .unwrap_or(onlyne_session::CloseReason::Completed);
                retire_idle_locked(&mut inner, &key, reason);
            }
        }
    }

    /// Retire tracked resources whose stored lifecycle has reached `Exited`.
    ///
    /// The periodic readiness tick calls this after completed work becomes an
    /// idle slot. Task-free sessions with an attached transport retain reuse,
    /// and task-free sessions whose agent has left release their host resource.
    pub fn reclaim_exited_resources(&self) {
        let mut inner = self.inner.lock();
        let candidates: Vec<(String, onlyne_session::CloseReason)> = inner
            .sessions
            .iter()
            .filter(|(key, slot)| {
                slot.task_id.is_none()
                    && session_exited(&inner, &slot.session.task_id)
                    && !has_attached_transport(&inner, key, slot)
            })
            .filter_map(|(key, slot)| {
                stored_close_reason(&inner, &slot.session.task_id)
                    .map(|reason| (key.clone(), reason))
            })
            .collect();
        for (key, reason) in candidates {
            retire_idle_locked(&mut inner, &key, reason);
        }
    }

    /// Retire the idle sessions whose plugin connection dropped and never came
    /// back, and answer how many left.
    ///
    /// A connection that ends without a `detach` frame leaves its session tracked
    /// so an agent that restarts inside `[client] reconnect_grace_secs` finds the
    /// resource it was using. That promise has to expire: a process that is
    /// simply gone would otherwise hold a slot, a projected `idle` row, and a live
    /// host resource forever, and on a role with `max_sessions = 1` it stops every
    /// later delivery. Only a session with no task bound is this sweep's to take —
    /// a session still bound to a task is under lifecycle ownership, and the retry
    /// that answers it ends that session through the merge in `on_out`. A session
    /// whose agent never returns while its task stays in flight is left to the
    /// stall and heartbeat surfaces, which is the boundary the operator set for
    /// this window.
    pub fn retire_dropped_ghosts(&self, now: Instant, grace_secs: u64) -> usize {
        if grace_secs == 0 {
            return 0;
        }
        let window = Duration::from_secs(grace_secs);
        let mut inner = self.inner.lock();
        let due: Vec<String> = inner
            .sessions
            .iter()
            .filter(|(_, slot)| slot.task_id.is_none())
            .filter(|(_, slot)| {
                slot.dropped_at.is_some_and(|dropped| {
                    now.checked_duration_since(dropped)
                        .is_some_and(|away| away >= window)
                })
            })
            .map(|(key, _)| key.clone())
            .collect();
        let mut retired = 0;
        for key in due {
            let reason = inner
                .sessions
                .get(&key)
                .and_then(|slot| stored_close_reason(&inner, &slot.session.task_id))
                .unwrap_or(onlyne_session::CloseReason::Fault);
            if retire_idle_locked(&mut inner, &key, reason) {
                retired += 1;
            }
        }
        retired
    }

    /// Bind a plugin transport to one staged session and hand it the payload.
    ///
    /// Both hand-over paths run through here: a plugin that mounted first, and a
    /// session staged first. The report that marks the session ready leaves
    /// before the assignment, which is the causal order §6 line 285 fixes.
    pub async fn hand_session(
        &self,
        task_id: &str,
        io: AdapterIo,
        capabilities: Vec<Capability>,
    ) -> Result<()> {
        let prose = self.role_prose();
        on_ready(
            self,
            ReadyNotice {
                task_id: task_id.to_string(),
                session_id: task_id.to_string(),
                generation: self.session_generation(task_id).unwrap_or(1),
                io: Some(io),
                capabilities,
            },
            &prose,
        )
        .await
    }

    /// Generation the reducer holds for one task, before any hand-off.
    pub fn session_generation(&self, task_id: &str) -> Option<u64> {
        self.inner
            .lock()
            .sessions
            .values()
            .find(|slot| slot.task_id.as_deref() == Some(task_id))
            .map(|slot| slot.session.generation)
    }

    /// Whether a delivery has somewhere to run.
    ///
    /// §5's `max_sessions` caps concurrency, so a delivery that arrives at the
    /// cap waits on the server: the row stays in flight and the next pull
    /// offers it again once a session frees. An idle session a finished task
    /// handed back is free capacity (§5 `reuse`), and a session the reducer has
    /// ended spends none of it.
    pub fn has_capacity(&self) -> bool {
        let inner = self.inner.lock();
        live_sessions(&inner) < inner.max_sessions as usize
            || inner.sessions.values().any(|slot| slot.task_id.is_none())
    }

    /// Whether this role already finished one task with a terminal `Done`.
    ///
    /// The durable row is the record: `settle` writes the outcome the agent
    /// filed, so a row reading `Done` means this role answered for this task id
    /// once already. A redelivery of that task is not new work — running it
    /// again would stage its payload on whichever session happens to be idle, so
    /// one chain's task executes inside another conversation and the second
    /// answer collides with the ledger row the first one settled.
    ///
    /// Only `Done` counts. A session killed or crashed mid-flight reaches
    /// `Exited` with no `Done`, and the server's requeue, `repair_retry`, and
    /// `control retry` all re-offer that task on purpose, so those deliveries
    /// still run.
    pub fn task_completed_here(&self, task_id: &str) -> bool {
        let inner = self.inner.lock();
        matches!(
            stored_close_reason(&inner, task_id),
            Some(onlyne_session::CloseReason::Completed)
        )
    }

    /// The task of one session that holds a payload with no connection bound.
    ///
    /// A work item that arrives before its always-running agent mounts waits in
    /// exactly this state, and the mount ends the wait. A reused session answers
    /// through the transport its first task claimed, so it stays served.
    pub fn staged_without_transport(&self) -> Option<String> {
        let inner = self.inner.lock();
        inner
            .sessions
            .iter()
            .find(|(_, slot)| slot.payload.is_some())
            .filter(|(key, slot)| {
                !inner
                    .transports
                    .keys()
                    .any(|session| names_session(key, slot, session))
            })
            .and_then(|(_, slot)| slot.task_id.clone())
    }

    /// Route one staged session to the thing that serves it.
    ///
    /// A self-driven backend owns its agent and takes the payload immediately,
    /// without an adapter socket. Otherwise the session's own connection comes
    /// first: a plugin the client spawned mounts with this session's id in
    /// `ONLYNE_SESSION_ID`, and a plugin that reconnected mounts with it again,
    /// so its assignment rides that socket alone. A plugin parked for the role
    /// takes the next staged session, once. A plugin-driven session with neither
    /// waits for its mount. Answers whether the payload had somewhere to go.
    pub async fn hand_staged(&self, session_id: &str) -> Result<bool> {
        let self_driven = self.inner.lock().backend.self_driven();
        if self_driven {
            let prose = self.role_prose();
            on_ready(
                self,
                ReadyNotice {
                    task_id: session_id.to_string(),
                    session_id: session_id.to_string(),
                    generation: self.session_generation(session_id).unwrap_or(1),
                    io: None,
                    capabilities: Vec::new(),
                },
                &prose,
            )
            .await?;
            return Ok(true);
        }
        let transport = self
            .session_transport(session_id)
            .or_else(|| self.claim_parked_transport(session_id));
        let Some((io, capabilities)) = transport else {
            return Ok(false);
        };
        self.hand_session(session_id, io, capabilities).await?;
        Ok(true)
    }

    /// Hand one note to the session already serving this role's work.
    ///
    /// A note carries no task, so it owns no session: §3's note is a message to
    /// an agent that is already running, and the plan refuses one whose role is
    /// offline (`note_queue` off). A role with no running agent has nothing to
    /// answer it, which is what the caller reports. Answers whether an agent
    /// took the note.
    pub async fn inject_note(&self, envelope: &Envelope) -> bool {
        let Some((task_id, session_id)) = self.ready_session() else {
            return false;
        };
        let Some((io, capabilities)) = self.session_transport(&session_id) else {
            return false;
        };
        if missing_capability(&capabilities, Capability::Inject) {
            tracing::debug!(task = %task_id, "plugin takes no message mid-task");
            return false;
        }
        let generation = self.session_generation(&task_id).unwrap_or(1);
        let assign = AssignArgs {
            envelope: Box::new(envelope.clone()),
            prose: self.role_prose(),
            task_id,
            generation,
            parent: None,
        };
        io.notify(AdapterMsg::Host(HostOp::Assign(assign)))
            .await
            .is_ok()
    }

    /// The session a mid-task message can join: a ready slot serving a task,
    /// answered as (task, session key).
    fn ready_session(&self) -> Option<(String, String)> {
        let inner = self.inner.lock();
        inner
            .sessions
            .iter()
            .find(|(_, slot)| slot.ready && slot.task_id.is_some() && !slot.read_only)
            .map(|(key, slot)| {
                (
                    slot.task_id.clone().unwrap_or_else(|| key.clone()),
                    key.clone(),
                )
            })
    }

    /// Ask the server one question over the live link.
    ///
    /// Err means the link is down, never a refusal: a refusal arrives as an
    /// `Ok` body carrying `ok: false`, which is what the local CLI shows.
    pub async fn request(&self, op: ClientOp) -> Result<ResBody, NetError> {
        let outbox = { self.inner.lock().outbox.clone() };
        let Some(outbox) = outbox else {
            return Err(NetError::NotReady);
        };
        outbox.request(op).await
    }

    /// Install the live link as the outbound path.
    pub fn attach_outbox(&self, outbox: Arc<dyn Outbox>) {
        self.inner.lock().outbox = Some(outbox);
    }

    /// Remove the outbound path; lifecycle frames then queue as intents.
    pub fn detach_outbox(&self) {
        self.inner.lock().outbox = None;
    }

    fn outbox(&self) -> Option<Arc<dyn Outbox>> {
        self.inner.lock().outbox.clone()
    }

    /// Queue one client op in the durable intent table and answer its op_id.
    pub fn enqueue_op(&self, op: &ClientOp) -> Result<String> {
        let op_id = onlyne_proto::new_id();
        self.inner
            .lock()
            .store
            .enqueue_intent(&op_id, &serde_json::to_value(op)?)?;
        Ok(op_id)
    }
}

fn family_of(envelope: &Envelope) -> String {
    envelope
        .causality
        .as_ref()
        .and_then(|c| c.parent_task.clone())
        .or_else(|| envelope.task_id().map(str::to_string))
        .unwrap_or_else(|| envelope.id.clone())
}

/// How deep an inbound task sits in its chain. An envelope that names no
/// causality is a root, and a relay born from it takes the hop below this.
fn hop_of(envelope: &Envelope) -> u32 {
    envelope
        .causality
        .as_ref()
        .map(|causality| causality.hop)
        .unwrap_or(0)
}

fn render_tokens(tokens: &[String], session: &str, task: &str) -> Vec<String> {
    tokens
        .iter()
        .map(|token| token.replace("{session}", session).replace("{task}", task))
        .collect()
}

/// Refuse a protocol-speaking session command on a pane backend.
///
/// `herdr`, `orca` and `zellij` hand the agent a terminal and read its screen,
/// so a command that speaks JSON-RPC on its own stdio would print frames into
/// the pane and answer nobody. The fix belongs in the workspace config: swapping
/// the backend at spawn time instead would turn "orca configured, exec
/// running" into a silent drift the operator never sees, so the delivery fails
/// and the reason reaches the ledger.
fn reject_protocol_command_in_pane(backend: &str, command: &[String]) -> Result<()> {
    if !matches!(backend, "herdr" | "orca" | "zellij") {
        return Ok(());
    }
    let token = command.iter().enumerate().find_map(|(index, arg)| {
        if arg == "--acp" || arg == "--mode=rpc" {
            Some(arg.as_str())
        } else if arg == "--mode" && command.get(index + 1).is_some_and(|next| next == "rpc") {
            Some("--mode rpc")
        } else {
            None
        }
    });
    if let Some(token) = token {
        return Err(anyhow!(
            "{backend} backend cannot host a protocol session: {token} speaks JSON-RPC on its own stdio and the pane would print the frames; set backend = \"exec\" or backend = \"acp\" in the workspace config"
        ));
    }
    Ok(())
}

/// The socket a session spawned in `workspace` dials.
///
/// `dispatch` passes this to [`session_env`] and the same tree lands in
/// `SpawnSpec.cwd`, so one resolve answers both halves of the spawn, and a
/// workspace past the unix bound yields the short path the client bound.
fn served_socket(workspace: &Path) -> PathBuf {
    RoleWorkspace::resolve(workspace).socket_path()
}

/// The environment one spawned session process carries.
///
/// The three `ONLYNE_` identity variables are what the plugin mounts with. The
/// relay pair is the guard's policy as the spec wrote it: a list joined by
/// commas, and the count in decimal. A policy the spec does not name injects no
/// variable at all, which is what leaves a hand-written `relay.toml` in charge
/// of a box that never put the policy in its spec.
///
/// `ONLYNE_CLUSTER` names the server's topology and is the address a host
/// backend groups sessions under. No welcome yet means no variable, and herdr
/// then keeps its own default-labelled workspace.
///
/// `ONLYNE_SOCKET` is the path the client is serving: the same accessor the
/// daemon bound, so a short endpoint reaches the session as the served path and
/// the plugin needs no guess of its own. A hand-started pi keeps its own
/// resolution as the fallback, which is the reason an empty path injects no key
/// at all.
fn session_env(
    role: &str,
    session_id: &str,
    task_id: &str,
    relay_required: &[String],
    relay_count: Option<u32>,
    topology: &str,
    adapter_socket: &Path,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("ONLYNE_SESSION_ID".into(), session_id.to_string());
    env.insert("ONLYNE_TASK_ID".into(), task_id.to_string());
    env.insert("ONLYNE_ROLE".into(), role.to_string());
    if !adapter_socket.as_os_str().is_empty() {
        env.insert(
            "ONLYNE_SOCKET".into(),
            adapter_socket.to_string_lossy().into_owned(),
        );
    }
    if !topology.is_empty() {
        env.insert("ONLYNE_CLUSTER".into(), topology.to_string());
    }
    if !relay_required.is_empty() {
        env.insert("ONLYNE_RELAY_REQUIRED".into(), relay_required.join(","));
    }
    if let Some(count) = relay_count {
        env.insert("ONLYNE_RELAY_COUNT".into(), count.to_string());
    }
    env
}

/// Whether the stored lifecycle of one session is the terminal `Exited`.
///
/// `client.db` holds the projection the reducer wrote under the
/// `(generation, seq)` gate, and the row stays readable after the session ends.
/// A session with no row yet is live: it exists as a spawned resource alone.
fn session_exited(inner: &DispatchInner, task_id: &str) -> bool {
    inner
        .store
        .get_session(task_id)
        .ok()
        .flatten()
        .map(|row| phase(&row.public_lifecycle, Lifecycle::Created) == Lifecycle::Exited)
        .unwrap_or(false)
}

/// Sessions that hold the role's concurrency: the staged slots whose stored
/// lifecycle has not reached `Exited`.
///
/// §5's `max_sessions` caps concurrent sessions, and a session the reducer has
/// ended answers no task, so it stops spending capacity the moment its row
/// reads `exited`, whether the exit came from a completion report or from the
/// settled observation a plugin sends as its last heartbeat. Exited rows stay
/// in `client.db` and stay queryable.
fn live_sessions(inner: &DispatchInner) -> usize {
    inner
        .sessions
        .values()
        .filter(|slot| !session_exited(inner, &slot.session.task_id))
        .count()
}

/// Whether an unbound slot can take the next task under `reuse`.
///
/// `reuse` means the next task goes to a session that is still here. A slot that
/// `control recycle` left behind has no resource below it: that arm writes the
/// closed word and closes the backend resource before releasing the binding.
/// Staging onto such a slot writes a payload onto a session with nothing to run
/// it, and the spawn path below stays unreachable, so the server row sits
/// `in_flight` while the role still looks like it has room. `detached` is a
/// different word: it means the client never confirmed the resource, and the
/// arm above leaves it alone, so it stays a candidate.
fn reuse_candidate(inner: &DispatchInner, slot: &SessionSlot) -> bool {
    if slot.task_id.is_some() || slot.read_only {
        return false;
    }
    inner
        .store
        .get_session(&slot.session.task_id)
        .ok()
        .flatten()
        .is_some_and(|row| row.resource_state != "closed")
}

pub fn dispatch(state: &DispatchState, envelope: &Envelope) -> Result<SessionRef> {
    let task_id = envelope
        .task_id()
        .context("task envelope missing causality.task")?
        .to_string();
    let family = family_of(envelope);
    let mut inner = state.inner.lock();
    // A task this role already serves rides its own slot, and the slot that
    // still holds delivery rights is the one that serves it: staging the payload
    // on a read-only revival would hand the work to an agent that may answer for
    // it but may be handed nothing.
    if let Some(session) = slot_key_serving_task(&inner, &task_id)
        .and_then(|key| inner.sessions.get_mut(&key))
        .map(|slot| {
            if slot.payload.is_none() {
                slot.payload = Some(envelope.clone());
                slot.hop = hop_of(envelope);
            }
            slot.session.clone()
        })
    {
        inner.stall.note_assigned(&task_id, Instant::now());
        return Ok(session);
    }
    if inner.reuse {
        // An idle session with no bound task takes the next task, preferring
        // one from the same family; §5's `reuse` is what makes a second task
        // share a session instead of waiting for a new slot. A read-only slot
        // answers for a task another session serves, so it takes no new one.
        let same_family = inner
            .sessions
            .iter()
            .find(|(_, slot)| reuse_candidate(&inner, slot) && slot.family == family)
            .map(|(key, _)| key.clone());
        let idle = same_family.or_else(|| {
            inner
                .sessions
                .iter()
                .find(|(_, slot)| reuse_candidate(&inner, slot))
                .map(|(key, _)| key.clone())
        });
        let reused = idle
            .and_then(|key| inner.sessions.get_mut(&key))
            .map(|slot| {
                slot.task_id = Some(task_id.clone());
                slot.payload = Some(envelope.clone());
                slot.hop = hop_of(envelope);
                slot.ready = false;
                let session = SessionRef {
                    task_id: task_id.clone(),
                    ..slot.session.clone()
                };
                slot.session = session.clone();
                session
            });
        if let Some(session) = reused {
            inner.bridge.track_live(session.clone());
            feed_created(&inner.bridge, &inner.store, &task_id)?;
            feed_dispatched(&inner.bridge, &inner.store, &task_id);
            inner.stall.note_assigned(&task_id, Instant::now());
            return Ok(session);
        }
    }
    if live_sessions(&inner) >= inner.max_sessions as usize {
        return Err(anyhow!("max_sessions reached"));
    }
    let session_id = task_id.clone();
    let command = render_tokens(&inner.command, &session_id, &task_id);
    reject_protocol_command_in_pane(inner.backend.name(), &command)?;
    let env = session_env(
        &inner.role,
        &session_id,
        &task_id,
        &inner.relay_required,
        inner.relay_count,
        &inner.topology,
        // One tree answers both halves of this spawn: the cwd below and the
        // socket the plugin dials, so a session whose workspace resolves to a
        // short endpoint is handed the served path directly.
        &served_socket(&inner.workspace),
    );
    let session = inner.backend.spawn(SpawnSpec {
        cwd: inner.workspace.clone(),
        task_id: task_id.clone(),
        command,
        env,
        focus: None,
        placement: None,
        rename: None,
    })?;
    inner.bridge.track_live(session.clone());
    feed_created(&inner.bridge, &inner.store, &task_id)?;
    feed_dispatched(&inner.bridge, &inner.store, &task_id);
    inner.sessions.insert(
        session_id,
        SessionSlot {
            session: session.clone(),
            family,
            task_id: Some(task_id.clone()),
            ready: false,
            payload: Some(envelope.clone()),
            msg_id: None,
            origin: Some(envelope.from.clone()),
            hop: hop_of(envelope),
            dropped_at: None,
            read_only: false,
        },
    );
    inner.stall.note_assigned(&task_id, Instant::now());
    Ok(session)
}

/// Adapter facts that make a session usable for its task.
pub struct ReadyNotice {
    pub task_id: String,
    pub session_id: String,
    pub generation: u64,
    /// Adapter transport for plugin-driven backends. A self-driven backend owns
    /// its agent and therefore reports ready without a socket.
    pub io: Option<AdapterIo>,
    pub capabilities: Vec<Capability>,
}

/// Report the session ready and hand its held payload to its agent. The `ready`
/// row reaches the ledger before either the backend delivery or adapter frame,
/// which is the causal order §6 requires.
pub async fn on_ready(state: &DispatchState, notice: ReadyNotice, prose: &str) -> Result<()> {
    let ReadyNotice {
        task_id,
        session_id,
        generation,
        io,
        capabilities,
    } = notice;
    let (payload, target, session, backend, version) = {
        let mut inner = state.inner.lock();
        let backend = Arc::clone(&inner.backend);
        // A ready report binds its connection to the session as much as a mount
        // does, so it runs the same judgement §1 (b) hangs on: a connection that
        // returns to a session a newer connection already serves takes nothing,
        // leaves nothing marked ready, and is held for that task's completion.
        if let Some(connection) = io.as_ref() {
            if is_revived_connection(&inner, connection) {
                return Ok(());
            }
            if !note_binding_locked(&mut inner, &session_id, connection) {
                record_revived_connection(
                    &mut inner,
                    &session_id,
                    connection.clone(),
                    capabilities.clone(),
                );
                return Ok(());
            }
        }
        let slot = inner
            .sessions
            .values_mut()
            .find(|slot| {
                slot.session.task_id == task_id
                    || slot
                        .session
                        .backend_ref
                        .get("id")
                        .and_then(|value| value.as_str())
                        == Some(session_id.as_str())
            })
            .ok_or_else(|| anyhow!("unknown session for {task_id}"))?;
        if slot.read_only {
            return Ok(());
        }
        // The hand-off runs once per session: a plugin that reports ready
        // after the assignment already left finds the payload gone.
        let Some(payload) = slot.payload.take() else {
            return Ok(());
        };
        slot.origin = Some(payload.from.clone());
        slot.ready = true;
        let session = slot.session.clone();
        let verdict = feed_ready(&inner.bridge, &inner.store, &task_id)?;
        let version = note_verdict(&verdict, &task_id).unwrap_or(Version::new(generation, 0));
        (payload, io, session, backend, version)
    };
    // The ready report reaches the server before the payload reaches the agent.
    send_frame(
        state,
        ClientOp::Report(Report::Ready {
            task_id: task_id.clone(),
            session_id: session_id.clone(),
            generation: version.generation,
            seq: version.seq,
            cluster_ref: None,
        }),
    )
    .await?;
    sync_session(state, &task_id).await?;
    let text = payload.body.text.clone().unwrap_or_default();
    match (backend.self_driven(), target) {
        (true, None) => backend.deliver(&session, &task_id, &text),
        (true, Some(_)) => Err(anyhow!(
            "self-driven session {session_id} unexpectedly has an adapter transport"
        )),
        (false, Some(target)) if capabilities.contains(&Capability::Inject) => {
            let assign = AssignArgs {
                envelope: Box::new(payload),
                prose: prose.to_string(),
                task_id,
                generation,
                parent: None,
            };
            target
                .notify(AdapterMsg::Host(HostOp::Assign(assign)))
                .await
                .map_err(|e| anyhow!(e))
        }
        (false, Some(target)) => target
            .notify(AdapterMsg::Host(HostOp::ConfigGet(
                onlyne_proto::ConfigGetArgs {
                    key: format!("stdin:{text}"),
                },
            )))
            .await
            .map_err(|e| anyhow!(e)),
        (false, None) => Err(anyhow!(
            "adapter-backed session {session_id} reported ready without a transport"
        )),
    }
}

/// Settle one finished task: relay what its report asked to hand on, publish the
/// verdict, and answer the sender.
///
/// The relay runs first and on purpose. A role that takes the handed-on task
/// must find the chain already pointing at it when the completion receipt
/// arrives, and a handoff that outlives this call has no caller left to record
/// its refusal.
pub async fn on_out(
    state: &DispatchState,
    task_id: &str,
    outcome: Outcome,
    head: Option<String>,
    head_kind: Option<&str>,
    handoffs: &[Handoff],
) -> Result<()> {
    let (verdict, receipt, role, hop, held) = {
        let mut inner = state.inner.lock();
        let verdict = settle(
            &inner.bridge,
            &inner.store,
            task_id,
            match outcome {
                Outcome::Done => onlyne_session::Outcome::Done,
                Outcome::Failed => onlyne_session::Outcome::Failed,
                Outcome::Cancelled => onlyne_session::Outcome::Cancelled,
            },
        )?;
        inner
            .store
            .put_out_head(task_id, head.as_deref().unwrap_or(""))?;
        // The handle and the chain this answer travels on belong to the session
        // serving the task, not to a read-only one that came back for it.
        let slot =
            slot_key_serving_task(&inner, task_id).and_then(|key| inner.sessions.get_mut(&key));
        let origin = slot.as_ref().and_then(|slot| slot.origin.clone());
        let hop = slot.as_ref().map(|slot| slot.hop).unwrap_or(0);
        let msg_id = slot.and_then(|slot| slot.msg_id.take());
        if let Some(msg_id) = msg_id {
            store_ack(
                &inner,
                AckArgs {
                    msg_id,
                    op_id: None,
                    accepted: true,
                    reason: None,
                },
            );
        }
        // A settled session gives its capacity back, so a role at
        // `max_sessions` takes the next row instead of holding finished slots.
        release_locked(&mut inner, task_id, None)?;
        // Whatever a read-only connection held for this task is answered by this
        // completion, so it leaves the buffer here and travels beside the report.
        let held = inner.held_handoffs.remove(task_id);

        (
            verdict,
            completion_envelope(&inner.role, origin, task_id, head.as_deref()),
            inner.role.clone(),
            hop,
            held,
        )
    };
    note_verdict(&verdict, task_id);
    // Every relay is answered before the verdict travels, and none of them
    // moves it: a refused handoff is a record on the settled task, not a
    // different outcome for it. The merge happens on the way in, so a downstream
    // role reads one envelope for this task, not two.
    let routed = merged_handoffs(
        handoffs,
        held.as_deref(),
        head.as_deref().unwrap_or_default(),
    );
    let denied = handoff::route(
        state,
        &role,
        task_id,
        hop,
        head_kind,
        head.as_deref().unwrap_or_default(),
        &routed,
    )
    .await;
    record_denials(state, task_id, &denied)?;
    // The merged relay has left, so the read-only session that wrote its half of
    // it is retired. The settled account above is the whole settlement: nothing
    // here settles or releases this task a second time.
    retire_revived(state, task_id).await;
    // The terminal receipt leaves as its own envelope, so the origin — a role
    // or a gateway conversation — learns the outcome (plan §3 `Completion`).
    // It rides the intent queue, which is what makes a completion survive the
    // disconnect rules of §6 line 289.
    if let Some(envelope) = receipt {
        transport_envelope(state, &envelope).await?;
    }
    sync_session(state, task_id).await
}

/// One relay per downstream role, carrying this completion's own lines and the
/// ones a read-only connection held for the same task.
///
/// Each line keeps the marker of the session that wrote it — `[retry]` for the
/// session that finished and `[zombie]` for the one that came back for the task
/// and was held — so the recipient can tell the two accounts apart inside the one
/// envelope. Line order follows the report's own order, with the held lines of the
/// same role below them. Nothing held is the ordinary case, and it routes the
/// report's lines without copying them.
fn merged_handoffs<'a>(
    own: &'a [Handoff],
    held: Option<&'a [Handoff]>,
    head: &str,
) -> Cow<'a, [Handoff]> {
    let held: &[Handoff] = match held {
        Some(held) if !held.is_empty() => held,
        _ => return Cow::Borrowed(own),
    };
    let mut order: Vec<String> = Vec::new();
    let mut segments: HashMap<String, Vec<String>> = HashMap::new();
    for (marker, group) in [("[retry]", own), ("[zombie]", held)] {
        for handoff in group {
            let line = format!("{marker} {}", handoff.text_or(head));
            if !segments.contains_key(handoff.to_role.as_str()) {
                order.push(handoff.to_role.clone());
            }
            segments
                .entry(handoff.to_role.clone())
                .or_default()
                .push(line);
        }
    }
    Cow::Owned(
        order
            .into_iter()
            .map(|to_role| Handoff {
                text: Some(
                    segments
                        .remove(to_role.as_str())
                        .unwrap_or_default()
                        .join("\n"),
                ),
                to_role,
            })
            .collect(),
    )
}

/// Retire the read-only connections and slots a merged handoff has just answered.
///
/// A connection that came back for a session another connection serves is
/// dropped from that session's record and its agent is told to leave, since what
/// it had to say travelled with the relay above. A slot that lost its task to a
/// newer session has the transport naming it dropped, its task binding released,
/// and is retired as `Replaced`: the resource its agent was holding is this
/// client's to close, and the newer session answers for the task. The account for
/// the task is the settlement above.
///
/// A connection inside its own inbound frame is left alone. `adapter_socket`
/// awaits the handler before it answers the frame, so a bye written here would
/// leave ahead of that connection's own response, and the plugin's bye handler
/// drops the socket and rejects every request awaiting an answer — a completion
/// the ledger already holds would reach the agent as a failure it retries. The
/// entry stays in `revived`: the connection's `detach` frame or its socket end
/// retires it through `release_connection`, and the plugin that just completed
/// ends its own session either way.
async fn retire_revived(state: &DispatchState, task_id: &str) {
    let leaving = {
        let mut inner = state.inner.lock();
        let mut leaving: Vec<AdapterIo> = Vec::new();
        let mut silent: Vec<String> = Vec::new();
        for (session_id, io, _) in inner.revived.iter() {
            if inner.in_frame.iter().any(|busy| busy.same_connection(io)) {
                continue;
            }
            let reaches = slot_key_named(&inner, session_id)
                .and_then(|key| {
                    inner
                        .sessions
                        .get(&key)
                        .map(|slot| slot_task(slot) == task_id)
                })
                .unwrap_or_else(|| session_id == task_id);
            if reaches {
                leaving.push(io.clone());
                silent.push(session_id.clone());
            }
        }
        inner
            .revived
            .retain(|(session_id, _, _)| !silent.contains(session_id));
        let silenced: Vec<String> = inner
            .sessions
            .iter()
            .filter(|(_, slot)| slot.read_only && slot_task(slot) == task_id)
            .map(|(key, _)| key.clone())
            .collect();
        for key in silenced {
            let Some(slot) = inner.sessions.get(&key).cloned() else {
                continue;
            };
            if slot.payload.is_some() {
                tracing::warn!(
                    session = %key,
                    task = %task_id,
                    "a read-only session retires with a payload it was never handed"
                );
            }
            let served: Vec<String> = inner
                .transports
                .keys()
                .filter(|served| names_session(&key, &slot, served))
                .cloned()
                .collect();
            for session_id in served {
                inner.transports.remove(&session_id);
            }
            if let Some(current) = inner.sessions.get_mut(&key) {
                current.task_id = None;
                current.ready = false;
                current.read_only = false;
                current.dropped_at = None;
            }
            retire_idle_locked(&mut inner, &key, onlyne_session::CloseReason::Replaced);
        }
        leaving
    };
    for io in leaving {
        let notice = AdapterMsg::Host(HostOp::Bye(onlyne_proto::ByeNotice {
            reason: "the session that took this task answered for yours".into(),
        }));
        if let Err(error) = io.notify(notice).await {
            tracing::debug!(error = %error, "the read-only connection had already left");
        }
    }
}

/// Write down the relays this role could not send.
///
/// Each refusal gets an event of its own, because that is the plane a supervisor
/// reads to see which handoff line died. The fault queue dedups on
/// `(task, kind, generation)`, so the first refusal of a turn is also the one
/// the task's fault row names; the rest stay in the events.
fn record_denials(state: &DispatchState, task_id: &str, denied: &[Denial]) -> Result<()> {
    if denied.is_empty() {
        return Ok(());
    }
    let inner = state.inner.lock();
    for refusal in denied {
        tracing::warn!(
            task = %task_id,
            to_role = %refusal.to_role,
            error = %refusal.reason,
            "handoff denied"
        );
        inner.store.append_event(
            "handoff_denied",
            &serde_json::json!({
                "task_id": task_id,
                "to_role": refusal.to_role,
                "text": refusal.text,
                "error": refusal.reason,
            }),
        )?;
        onlyne_session::record_fault(
            &inner.store,
            task_id,
            "handoff_denied",
            "acp",
            &format!("{}: {}", refusal.to_role, refusal.reason),
        )?;
    }
    Ok(())
}

/// The receipt for one finished task, or `None` when its sender is unknown.
///
/// Every settled task answers its sender, the role that sent the task included:
/// §3's `Completion` is the durable record that the work ended, and a role
/// reading its own receipt ack is what settles the row.
fn completion_envelope(
    role: &str,
    origin: Option<Principal>,
    task_id: &str,
    head: Option<&str>,
) -> Option<Envelope> {
    let origin = origin?;
    // A turn that left no result line still ends its task, and the sender still
    // gets its answer: an empty body travels as `text: Some("")`, which the
    // validator accepts, where an absent body would drop the receipt and leave
    // the origin waiting on a task this role has already retired.
    let body = Body::text(head.unwrap_or_default());
    let causality = Causality {
        task: task_id.to_string(),
        parent_task: None,
        reply_to: None,
        hop: 0,
        attempt: 0,
    };
    // `new_envelope` validates every protocol rule on the way out, so a receipt
    // that cannot be addressed to its sender is the only one that goes unsent.
    new_envelope(
        MsgKind::Completion,
        Principal::role(role),
        origin,
        body,
        Some(causality),
    )
    .ok()
}

/// Hand one envelope to the live link, or to the intent queue when it is down.
async fn transport_envelope(state: &DispatchState, envelope: &Envelope) -> Result<()> {
    let op = ClientOp::Send(Box::new(envelope.clone()));
    let outbox = { state.inner.lock().outbox.clone() };
    match outbox {
        Some(outbox) => match outbox.send(op).await {
            Ok(()) => Ok(()),
            Err(error) => {
                tracing::warn!(error = %error, "completion fell back to the intent queue");
                state.enqueue_outbound(envelope).map(|_| ())
            }
        },
        None => state.enqueue_outbound(envelope).map(|_| ()),
    }
}

/// Retire one session. The stored tuple decides whether a live resource
/// remains to close, and the caller's reason reaches the backend unchanged, so an
/// operator cancel stops reporting itself as a completion.
pub fn on_recycled(
    state: &DispatchState,
    task_id: &str,
    reason: onlyne_session::CloseReason,
) -> Result<()> {
    let mut inner = state.inner.lock();
    release_locked(&mut inner, task_id, Some(reason))
}

/// Act on one control command that arrived as a delivery.
///
/// `recycle` and `cancel` reach the agent first, so it ends its own turn and
/// sends its terminal report, and the backend close follows whatever the plugin
/// did: a session whose adapter is gone still loses its process, which is the
/// half of §7's recovery ladder the operator drives by hand otherwise. `probe`
/// asks the plugin for a fresh observation and republishes the projection the
/// reducer already holds, `snapshot` republishes alone, and `focus` asks the
/// backend to bring the live session to the front.
///
/// Answers whether the command named a session this client holds. A `false` is
/// the honest answer for a task this role does not own, and the caller still
/// settles the row: re-offering a command no client can act on spends the
/// delivery forever.
pub async fn on_control(state: &DispatchState, op: &ControlOp) -> Result<bool> {
    let task_id = op.task_id();
    let held = state.holds_task(task_id);
    match op {
        ControlOp::Recycle { reason, .. } => {
            state.recycle_plugin(task_id, reason, None).await;
            on_recycled(state, task_id, onlyne_session::CloseReason::Operator)?;
        }
        ControlOp::Cancel { reason, .. } => {
            state
                .recycle_plugin(task_id, reason, Some(Outcome::Cancelled))
                .await;
            on_recycled(state, task_id, onlyne_session::CloseReason::Cancelled)?;
        }
        ControlOp::Probe { .. } => {
            state.probe_plugin(task_id).await;
            sync_session(state, task_id).await?;
        }
        ControlOp::Snapshot { .. } => sync_session(state, task_id).await?,
        ControlOp::Focus { .. } => {
            let (backend, session) = {
                let inner = state.inner.lock();
                let session = inner
                    .sessions
                    .values()
                    .find(|slot| slot.task_id.as_deref() == Some(task_id))
                    .map(|slot| slot.session.clone());
                (inner.backend.clone(), session)
            };
            match session {
                Some(session_ref) => {
                    if let Err(error) = backend.focus(&session_ref) {
                        tracing::warn!(error = %error, task = %task_id, "focus refused");
                        if let Err(fault_error) = on_plugin_report(
                            state,
                            Report::Fault {
                                task_id: Some(task_id.to_string()),
                                session_id: None,
                                generation: None,
                                seq: None,
                                kind: "focus".into(),
                                reason: error.to_string(),
                                desired: None,
                                observed: None,
                            },
                        )
                        .await
                        {
                            tracing::warn!(
                                error = %fault_error,
                                task = %task_id,
                                "focus refused"
                            );
                        }
                    }
                }
                None => {
                    tracing::warn!(task = %task_id, "focus has no live session");
                }
            }
        }
    }
    Ok(held)
}

fn stored_close_reason(
    inner: &DispatchInner,
    task_id: &str,
) -> Option<onlyne_session::CloseReason> {
    let row = inner.store.get_session(task_id).ok().flatten()?;
    match projection_of(&row).outcome? {
        Outcome::Done => Some(onlyne_session::CloseReason::Completed),
        Outcome::Failed => Some(onlyne_session::CloseReason::Fault),
        Outcome::Cancelled => Some(onlyne_session::CloseReason::Cancelled),
    }
}

/// Retire one task-free session after its transport set becomes empty.
///
/// The idle slot releases its backend resource because the agent able to reuse
/// it has left. An attached transport keeps the resource because reuse remains
/// possible. The dispatch lock serializes the final transport check, reference
/// refresh, lifecycle projection, backend close, and slot removal with adapter
/// binding.
fn retire_idle_locked(
    inner: &mut DispatchInner,
    key: &str,
    reason: onlyne_session::CloseReason,
) -> bool {
    let Some(slot) = inner.sessions.get(key) else {
        return false;
    };
    if slot.task_id.is_some() || has_attached_transport(inner, key, slot) {
        return false;
    }

    let original = slot.session.clone();
    let task_id = original.task_id.clone();
    let resource = inner
        .store
        .get_session(&task_id)
        .ok()
        .flatten()
        .map(|row| row.resource_state)
        .unwrap_or_else(|| "detached".to_string());
    if resource != "detached" && resource != "closed" {
        let session = match inner.backend.attach(&original) {
            Ok(refreshed) => {
                if refreshed != original {
                    inner.bridge.track_live(refreshed.clone());
                    if let Some(slot) = inner.sessions.get_mut(key) {
                        slot.session = refreshed.clone();
                    }
                }
                refreshed
            }
            Err(_) => original,
        };
        tracing::info!(
            task = %task_id,
            backend = %session.backend,
            resource = %session.backend_ref,
            ?reason,
            "retiring idle session resource"
        );
        if let Err(error) = feed_resource_closed(&inner.bridge, &inner.store, &task_id) {
            tracing::warn!(
                task = %task_id,
                backend = %session.backend,
                resource = %session.backend_ref,
                error = %error,
                "session resource close projection failed"
            );
        }
        if let Err(error) = inner.backend.close(&session, reason, false) {
            tracing::warn!(
                task = %task_id,
                backend = %session.backend,
                resource = %session.backend_ref,
                error = %error,
                "session resource retirement failed"
            );
        }
    }
    inner.bridge.untrack_live(&task_id);
    inner.sessions.remove(key);
    true
}

/// Give one session's task slot back. Settled tasks enter idle retirement, and
/// explicit reasons drive the control-close path.
fn release_locked(
    inner: &mut DispatchInner,
    task_id: &str,
    reason: Option<onlyne_session::CloseReason>,
) -> Result<()> {
    let resource = inner
        .store
        .get_session(task_id)?
        .map(|row| row.resource_state)
        .unwrap_or_else(|| "detached".to_string());
    if let Some((key, slot)) = slot_key_serving_task(inner, task_id)
        .and_then(|key| inner.sessions.get(&key).map(|slot| (key, slot.clone())))
    {
        if let Some(reason) = reason {
            if resource != "detached" && resource != "closed" {
                feed_resource_closed(&inner.bridge, &inner.store, task_id)?;
                inner.backend.close(&slot.session, reason, false)?;
            }
            inner.bridge.untrack_live(task_id);
            if inner.reuse {
                if let Some(session) = inner.sessions.get_mut(&key) {
                    session.task_id = None;
                    session.ready = false;
                }
            } else {
                inner.sessions.remove(&key);
            }
        } else {
            if let Some(session) = inner.sessions.get_mut(&key) {
                session.task_id = None;
                session.ready = false;
            }
            retire_idle_locked(inner, &key, onlyne_session::CloseReason::Completed);
        }
    }
    inner.stall.forget(task_id);
    if reason.is_some() && resource == "detached" {
        inner
            .store
            .note_alert(format!("session recycled {task_id}"));
    }
    Ok(())
}

/// Whether the session serving `task_id` still has a live resource.
///
/// `attach` runs first because a backend can re-resolve a resource whose
/// stored reference went stale (Orca mints a new terminal handle per PTY
/// incarnation). A refreshed reference is written back to the slot and the
/// bridge, which is what makes the next probe, close, or ledger write target
/// the current resource. An attach the backend cannot answer is not proof of
/// death, so the probe decides.
pub fn session_alive(state: &DispatchState, task_id: &str) -> bool {
    let mut inner = state.inner.lock();
    let Some(key) = inner
        .sessions
        .iter()
        .find(|(_, slot)| slot.task_id.as_deref() == Some(task_id))
        .map(|(key, _)| key.clone())
    else {
        return false;
    };
    let session = inner.sessions[&key].session.clone();
    let session = match inner.backend.attach(&session) {
        Ok(refreshed) => {
            if refreshed != session {
                inner.bridge.track_live(refreshed.clone());
                if let Some(slot) = inner.sessions.get_mut(&key) {
                    slot.session = refreshed.clone();
                }
            }
            refreshed
        }
        Err(_) => session,
    };
    inner
        .backend
        .probe(&session)
        .map(|probe| probe.alive)
        .unwrap_or(false)
}

/// Close every live session's resource with `reason` and forget the slots.
///
/// This is the shutdown path: a stopped client must not leave resources behind
/// that only it can address, and each backend's own record of the resource —
/// the Orca tab map included — ends with the session. `budget` bounds the whole
/// sweep, because an operator's SIGTERM must not turn into a hang while a slow
/// backend CLI exits; whatever the budget cuts off is reported and dropped
/// anyway.
pub fn close_all(state: &DispatchState, reason: onlyne_session::CloseReason, budget: Duration) {
    let started = Instant::now();
    let mut inner = state.inner.lock();
    let sessions: Vec<(String, SessionRef)> = inner
        .sessions
        .iter()
        .map(|(key, slot)| (key.clone(), slot.session.clone()))
        .collect();
    for (key, session) in sessions {
        if started.elapsed() > budget {
            tracing::warn!(
                task = %session.task_id,
                "shutdown close budget reached; the resource is left behind"
            );
        } else if let Err(error) = inner.backend.close(&session, reason, false) {
            tracing::warn!(
                task = %session.task_id,
                error = %error,
                "session close failed during shutdown"
            );
        }
        inner.bridge.untrack_live(&session.task_id);
        inner.sessions.remove(&key);
    }
}

pub async fn on_plugin_report(state: &DispatchState, report: Report) -> Result<()> {
    let subject = task_id_of(&report).to_string();
    let touched = match report {
        Report::Ready { task_id, .. } => {
            let verdict = {
                let inner = state.inner.lock();
                feed_ready(&inner.bridge, &inner.store, &task_id)?
            };
            note_verdict(&verdict, &task_id).is_some()
        }
        Report::Heartbeat {
            task_id,
            generation,
            seq,
            observed,
            ..
        } => {
            // The beat itself is the liveness fact. The server times
            // heartbeats. A quiet, alive session keeps landing fresh rows.
            let mut inner = state.inner.lock();
            let verdict = match serde_json::from_value::<Observation>(observed) {
                Ok(body) => apply_persist(
                    &inner.bridge,
                    &inner.store,
                    &task_id,
                    &LifecycleEvent::Heartbeat {
                        v: Version::new(generation, seq),
                        body,
                    },
                )?,
                Err(error) => {
                    tracing::warn!(task = %task_id, error = %error, "heartbeat carries no readable observation; liveness only");
                    Verdict::Ignored(IgnoredReason::NoOp)
                }
            };
            note_verdict(&verdict, &task_id);
            match &verdict {
                Verdict::Applied(_) => {
                    inner.stall.note_applied(&task_id, Instant::now());
                    true
                }
                Verdict::Ignored(IgnoredReason::NoOp) => inner
                    .store
                    .bump_session_version(&task_id, generation, seq)?,
                Verdict::Ignored(_) | Verdict::Rejected(_) => false,
            }
        }
        Report::Complete {
            task_id,
            outcome,
            head,
            ..
        } => {
            // A plugin reports its own ending, and it hands nothing on: the
            // report file is the only place handoff lines are put down, and that
            // is a route a plugin-backed session does not have.
            on_out(state, &task_id, outcome, head, None, &[]).await?;
            false
        }
        Report::Fault {
            task_id: Some(task_id),
            kind,
            reason,
            ..
        } => {
            let inner = state.inner.lock();
            onlyne_session::record_fault(&inner.store, &task_id, &kind, "plugin", &reason)?;
            false
        }
        Report::Fault { task_id: None, .. } => false,
    };
    if touched {
        sync_session(state, &subject).await?;
    }
    Ok(())
}

/// Task a state-carrying report names, for the projection publish.
fn task_id_of(report: &Report) -> &str {
    match report {
        Report::Ready { task_id, .. } | Report::Heartbeat { task_id, .. } => task_id,
        Report::Complete { task_id, .. } => task_id,
        Report::Fault { .. } => "",
    }
}

pub fn missing_capability(capabilities: &[Capability], capability: Capability) -> bool {
    !capabilities.contains(&capability)
}

pub fn plugin_gap(capabilities: &[Capability]) -> Vec<onlyne_adapter::HostGap> {
    onlyne_adapter::degrade_for(
        &[Capability::Recycle, Capability::Report, Capability::Inject]
            .iter()
            .copied()
            .filter(|cap| missing_capability(capabilities, *cap))
            .collect::<Vec<_>>(),
    )
}

/// Agent name this binary reports during the handshake.
const AGENT: &str = "onlyne-client";
/// Wait bound for one request round trip.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Server heartbeat interval from the observation rules of §4.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// One authenticated server link.
///
/// The first five methods are the whole transport surface the runloop uses, so
/// a change of transport touches this struct alone. The remaining two read the
/// supervision state of the connection the handle keeps across redials.
#[derive(Clone)]
pub struct ClientLink {
    handle: ClientConn,
    welcome: Arc<Welcome>,
    hello: HandshakeArgs,
}

impl ClientLink {
    /// Dial, verify the certificate pin, sign the server challenge, then read
    /// the role slice with `hello`.
    pub async fn connect(init: &ClientInit, live_tasks: Vec<String>) -> Result<Self, NetError> {
        let keypair = KeyPair::load(&init.key_path)?;
        let settings = ConnSettings {
            agent: AGENT.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            ..ConnSettings::new(PROTOCOL_VERSION)
        };
        let handle: ClientConn =
            dial(&init.server, &keypair, &init.cert_pin, &init.role, settings).await?;
        let hello = HandshakeArgs {
            protocol: PROTOCOL_VERSION,
            role: init.role.clone(),
            key: keypair.public_str(),
            signature: String::new(),
            agent: AGENT.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            aggregate: false,
            live_tasks: Vec::new(),
        };
        let body = handle
            .request(
                Frame::req(
                    String::new(),
                    ClientOp::Hello(hello_with_live_tasks(&hello, live_tasks)),
                ),
                REQUEST_TIMEOUT,
            )
            .await?;
        if !body.ok {
            let error = body.error.clone().unwrap_or(onlyne_proto::ErrorPayload {
                code: onlyne_proto::ErrorCode::Internal,
                message: "hello refused".to_string(),
                field: None,
            });
            return Err(NetError::Rejected {
                code: wire_code(error.code),
                message: error.message,
            });
        }
        let data = body.data().cloned().ok_or(NetError::BadFrame)?;
        let welcome: Welcome = serde_json::from_value(data).map_err(|_| NetError::BadFrame)?;
        Ok(Self {
            handle,
            welcome: Arc::new(welcome),
            hello,
        })
    }

    /// Send the routed `hello` again on the connection this link now holds.
    ///
    /// The net layer redials on its own, and a fresh connection carries no role
    /// binding until this frame lands, so a caller replays it whenever readiness
    /// returns (plan §7 line 310).
    pub async fn authenticate(&self, live_tasks: Vec<String>) -> Result<(), NetError> {
        let body = self
            .handle
            .request(
                Frame::req(
                    String::new(),
                    ClientOp::Hello(hello_with_live_tasks(&self.hello, live_tasks)),
                ),
                REQUEST_TIMEOUT,
            )
            .await?;
        if !body.ok {
            let error = body.error.clone().unwrap_or(onlyne_proto::ErrorPayload {
                code: onlyne_proto::ErrorCode::Internal,
                message: "hello refused".to_string(),
                field: None,
            });
            return Err(NetError::Rejected {
                code: wire_code(error.code),
                message: error.message,
            });
        }
        Ok(())
    }

    /// Role slice the server bound this link to.
    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    /// Send one request frame and answer with its body. A refusal from the
    /// server arrives inside the body, which keeps the retry decision in the
    /// intent machine.
    pub async fn request(&self, op: ClientOp) -> Result<ResBody, NetError> {
        self.handle
            .request(Frame::req(String::new(), op), REQUEST_TIMEOUT)
            .await
    }

    /// Clone the server observation stream.
    pub fn events(&self) -> broadcast::Receiver<Frame<ClientOp>> {
        self.handle.events()
    }

    /// Send `bye` and drain in-flight requests.
    pub async fn close(&self) -> Result<(), NetError> {
        self.handle.close().await
    }

    /// Liveness of the connection behind this link.
    pub fn readiness(&self) -> ConnReadiness {
        self.handle.readiness()
    }
    /// Reason the supervisor stopped redialing.
    pub async fn failure(&self) -> Option<NetError> {
        self.handle.failure().await
    }
}

/// Stamp dispatch live-slot task ids onto a hello skeleton.
///
/// Slots exist from assign until release. A fresh process has none, so hello
/// sends an empty list and the server requeues. A live client whose link
/// flaps still holds its slots, so those rows stay in_flight.
pub(crate) fn hello_with_live_tasks(
    hello: &HandshakeArgs,
    live_tasks: Vec<String>,
) -> HandshakeArgs {
    let mut hello = hello.clone();
    hello.live_tasks = live_tasks;
    hello
}

/// Wire code of a refusal, in the snake_case spelling both sides share.
fn wire_code(code: onlyne_proto::ErrorCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "internal".to_string())
}

/// Stamp the origin cluster on the state-carrying report kinds.
///
/// `cluster_ref` names the cluster whose supervisor observed this projection
/// (`docs/v1-PLAN.md` line 248, the `aggregate` annotation of the role's own
/// spec entry, which is `Principal::Cluster` on the wire at line 122). A plain
/// role leaves the field unset, which is the `skip_serializing_if` shape the
/// byte-identical replay rule at line 502 depends on.
pub fn with_cluster(state: &DispatchState, report: Report) -> Report {
    let cluster = state.cluster_ref();
    if cluster.is_empty() {
        return report;
    }
    match report {
        Report::Ready {
            task_id,
            session_id,
            generation,
            seq,
            ..
        } => Report::Ready {
            task_id,
            session_id,
            generation,
            seq,
            cluster_ref: Some(cluster),
        },
        Report::Heartbeat {
            task_id,
            generation,
            seq,
            observed,
            ..
        } => Report::Heartbeat {
            task_id,
            generation,
            seq,
            observed,
            cluster_ref: Some(cluster),
        },
        Report::Complete {
            task_id,
            outcome,
            head,
            reply_to,
            ..
        } => Report::Complete {
            task_id,
            outcome,
            head,
            reply_to,
            cluster_ref: Some(cluster),
        },
        other => other,
    }
}

/// Where lifecycle frames leave the dispatcher.
///
/// `send` completes once the frame is on the wire. The ready report awaits this
/// before the payload reaches the agent, which is the causal order §6 fixes.
pub trait Outbox: Send + Sync {
    fn send(&self, op: ClientOp)
    -> Pin<Box<dyn Future<Output = Result<(), NetError>> + Send + '_>>;

    /// One request round trip, for a caller that needs the server's answer
    /// rather than a queued frame: the local CLI reports the verdict a send got.
    fn request(
        &self,
        op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = Result<ResBody, NetError>> + Send + '_>>;
}

impl Outbox for ClientLink {
    fn send(
        &self,
        op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = Result<(), NetError>> + Send + '_>> {
        Box::pin(async move { self.request(op).await.map(|_| ()) })
    }

    fn request(
        &self,
        op: ClientOp,
    ) -> Pin<Box<dyn Future<Output = Result<ResBody, NetError>> + Send + '_>> {
        Box::pin(async move { ClientLink::request(self, op).await })
    }
}

/// Deliver one lifecycle frame, and queue it durably when the link is down.
///
/// §6 line 289: a running session reaches its terminal state while the outbound
/// work waits in `client.db` intents for the flusher.
pub async fn send_frame(state: &DispatchState, op: ClientOp) -> Result<()> {
    let op = match op {
        ClientOp::Report(report) => ClientOp::Report(with_cluster(state, report)),
        other => other,
    };
    if let Some(outbox) = state.outbox() {
        if outbox.send(op.clone()).await.is_ok() {
            return Ok(());
        }
    }
    state.accept_new().store(false, Ordering::SeqCst);
    state.enqueue_op(&op)?;
    Ok(())
}

/// The wire projection of one stored session row.
pub fn projection_of(row: &SessionRecord) -> SessionProjection {
    let observed: Option<serde_json::Value> = serde_json::from_str(&row.observed_json).ok();
    // The reducer records the terminal outcome inside the observation, and the
    // plan's session read publishes it beside the lifecycle (line 498).
    let outcome = observed
        .as_ref()
        .and_then(|value| value.get("outcome"))
        .and_then(|value| serde_json::from_value::<Outcome>(value.clone()).ok());
    SessionProjection {
        lifecycle: phase(&row.public_lifecycle, Lifecycle::Created),
        agent: phase(&row.agent_state, AgentPhase::Booting),
        delivery: phase(&row.delivery_state, DeliveryPhase::NoIntent),
        resource: phase(&row.resource_state, ResourcePhase::Detached),
        recovery: phase(&row.recovery_substate, RecoveryPhase::NoRecovery),
        outcome,
        observed,
    }
}

/// Decode one stored enum word, falling back to the freshly created phase.
fn phase<T: serde::de::DeserializeOwned>(word: &str, fallback: T) -> T {
    serde_json::from_value(serde_json::Value::String(word.to_string())).unwrap_or(fallback)
}

/// Publish the current projection of one session.
pub async fn sync_session(state: &DispatchState, task_id: &str) -> Result<()> {
    let row = { state.inner.lock().store.get_session(task_id)? };
    let Some(row) = row else { return Ok(()) };
    let args = SessionSyncArgs {
        task_id: row.task_id.clone(),
        session_id: row.task_id.clone(),
        generation: row.generation.max(0) as u64,
        seq: row.seq.max(0) as u64,
        projection: projection_of(&row),
    };
    send_frame(state, ClientOp::SessionSync(args)).await
}

/// Log a reducer verdict and answer the version it advanced to.
pub fn note_verdict(verdict: &Verdict, task_id: &str) -> Option<Version> {
    match verdict {
        Verdict::Applied(observation) => Some(observation.version),
        Verdict::Ignored(reason) => {
            tracing::debug!(task = %task_id, ?reason, "lifecycle event ignored");
            None
        }
        Verdict::Rejected(reason) => {
            tracing::warn!(task = %task_id, ?reason, "lifecycle event rejected; the ledger kept its state");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Arc, ClientStore, DispatchState, Path, PathBuf, RoleWorkspace, SessionRef, SessionSlot,
        SpawnSpec, completion_envelope, served_socket, session_env,
    };
    use onlyne_layout::UNIX_SOCKET_PATH_MAX;
    use onlyne_proto::{MsgKind, Principal, new_task_id};
    use onlyne_session::backend::fake::FakeBackend;
    use tempfile::tempdir;

    /// Every settled task answers its sender, including the turn that left no
    /// result line. The protocol requires a body, so the empty answer travels as
    /// an empty text field, and the receipt survives validation. A dropped
    /// receipt strands the origin: it waits on a task the role has already
    /// retired, which is how a ring stops mid-circle.
    #[test]
    fn a_settled_task_without_a_result_line_still_files_its_receipt() {
        let task = new_task_id();
        let quiet = completion_envelope("planner", Some(Principal::role("reviewer")), &task, None)
            .expect("an answer with nothing to say is still an answer");
        assert_eq!(quiet.kind, MsgKind::Completion);
        assert_eq!(quiet.causality.as_ref().unwrap().task, task);
        assert_eq!(quiet.body.text.as_deref(), Some(""));
        assert_eq!(
            quiet.to,
            Principal::role("reviewer"),
            "the receipt is addressed to the sender"
        );

        // A blank head and an absent one are the same answer to the sender.
        let blank = completion_envelope(
            "planner",
            Some(Principal::role("reviewer")),
            &task,
            Some(""),
        )
        .expect("a blank result line files too");
        assert_eq!(blank.body.text, quiet.body.text);

        let said = completion_envelope(
            "planner",
            Some(Principal::role("reviewer")),
            &task,
            Some("done"),
        )
        .expect("a result line travels verbatim");
        assert_eq!(said.body.text.as_deref(), Some("done"));

        // The one case that stays silent is the one with no sender to answer.
        assert!(
            completion_envelope("planner", None, &task, Some("done")).is_none(),
            "an unaddressed task files no receipt"
        );
    }

    /// The guard reads its policy from the environment before its own
    /// `relay.toml`, so what the client injects is the whole contract between
    /// the spec and a spawned session: the list comma-joined, the count in
    /// decimal, and neither variable at all when the spec names no policy.
    #[test]
    fn the_spawn_environment_carries_the_relay_policy_it_has() {
        let plain = session_env(
            "planner",
            "s-1",
            "t-1",
            &[],
            None,
            "cluster-a",
            Path::new(""),
        );
        assert_eq!(plain["ONLYNE_SESSION_ID"], "s-1");
        assert_eq!(plain["ONLYNE_TASK_ID"], "t-1");
        assert_eq!(plain["ONLYNE_ROLE"], "planner");
        // The topology name is the address a host backend groups sessions under.
        assert_eq!(plain["ONLYNE_CLUSTER"], "cluster-a");
        assert!(
            !plain.contains_key("ONLYNE_RELAY_REQUIRED")
                && !plain.contains_key("ONLYNE_RELAY_COUNT"),
            "no policy injects no key at all: {plain:?}"
        );
        // A surface that answered nothing names nothing: the plugin keeps its own
        // resolution for a hand-started session.
        assert!(!plain.contains_key("ONLYNE_SOCKET"), "{plain:?}");

        let listed = session_env(
            "planner",
            "s-1",
            "t-1",
            &["writer".to_string(), "auditor".to_string()],
            None,
            "",
            Path::new(""),
        );
        assert_eq!(listed["ONLYNE_RELAY_REQUIRED"], "writer,auditor");
        assert!(!listed.contains_key("ONLYNE_RELAY_COUNT"));
        // No welcome yet, so no topology to name: the key stays out rather than
        // arriving empty.
        assert!(!listed.contains_key("ONLYNE_CLUSTER"));

        let counted = session_env("planner", "s-1", "t-1", &[], Some(2), "", Path::new(""));
        assert_eq!(counted["ONLYNE_RELAY_COUNT"], "2");
        assert!(!counted.contains_key("ONLYNE_RELAY_REQUIRED"));

        // Both variables travel when the spec names both forms; the guard's own
        // precedence is what makes the list win.
        let both = session_env(
            "planner",
            "s-1",
            "t-1",
            &["writer".to_string()],
            Some(2),
            "",
            Path::new(""),
        );
        assert_eq!(both["ONLYNE_RELAY_REQUIRED"], "writer");
        assert_eq!(both["ONLYNE_RELAY_COUNT"], "2");
    }

    /// The socket a session is handed is the path the workspace is serving.
    ///
    /// The bind publishes its choice in `<run>/socket` and the accessor reads
    /// that marker, so the value the client injects and the listener the client
    /// opened are one path even when the canonical spelling moved. This case
    /// crosses a real bind, which is the only way the two halves agree by
    /// evidence and by assertion alike.
    #[tokio::test]
    async fn the_spawn_environment_names_the_socket_the_workspace_serves() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();
        let layout = RoleWorkspace::resolve(workspace);
        let (listener, endpoint) =
            onlyne_layout::bind_socket(layout.root(), &layout.run_dir()).unwrap();
        let env = session_env(
            "planner",
            "s-1",
            "t-1",
            &[],
            None,
            "",
            &served_socket(workspace),
        );
        assert_eq!(
            env["ONLYNE_SOCKET"],
            endpoint.actual().to_string_lossy().as_ref(),
            "the plugin dials the path that was bound"
        );
        assert!(
            endpoint.actual().exists(),
            "the injected path is a live socket: {}",
            endpoint.actual().display()
        );
        drop(listener);
    }

    /// A workspace whose canonical socket spelling overflows `sun_path` still
    /// hands the session the short served path, and the directory the session
    /// starts in answers that same socket.
    ///
    /// `SpawnSpec.cwd` is the workspace root and `ONLYNE_SOCKET` is the served
    /// endpoint; both come out of one tree, so a plugin that resolves the socket
    /// from its own cwd reaches the listener the client bound. Windows keeps the
    /// canonical spelling as the bound spelling, so the premise lives on unix.
    #[cfg(unix)]
    #[test]
    fn a_deep_workspace_hands_the_session_the_short_served_socket() {
        let segment = "deep-workspace-segment-aaaaaaaaaaaaaaaaaaaaaaaa";
        let dir = tempdir().unwrap();
        let workspace = dir.path().join(segment).join(segment).join("leaf");
        std::fs::create_dir_all(workspace.join(".onlyne/run")).unwrap();
        let layout = RoleWorkspace::resolve(&workspace);
        let natural = layout.socket_path_natural();
        assert!(
            natural.as_os_str().len() > UNIX_SOCKET_PATH_MAX,
            "the premise: the canonical spelling is over the bound: {} bytes at {}",
            natural.as_os_str().len(),
            natural.display(),
        );
        let served = served_socket(&workspace);
        assert!(
            served.as_os_str().len() <= UNIX_SOCKET_PATH_MAX,
            "the served spelling fits the bound: {} bytes at {}",
            served.as_os_str().len(),
            served.display(),
        );
        assert_ne!(served, natural, "the socket moved off the canonical path");

        let env = session_env("planner", "s-1", "t-1", &[], None, "", &served);
        assert_eq!(env["ONLYNE_SOCKET"], served.to_string_lossy().as_ref());
        let spec = SpawnSpec {
            cwd: workspace.clone(),
            task_id: "t-1".into(),
            command: vec!["pi".into()],
            env,
            focus: None,
            placement: None,
            rename: None,
        };
        assert_eq!(
            RoleWorkspace::resolve(&spec.cwd).socket_path(),
            PathBuf::from(&spec.env["ONLYNE_SOCKET"]),
            "one tree answers both the cwd and the socket"
        );
    }

    /// A task's delivery handle belongs to the session serving it, never to one
    /// that came back for it.
    ///
    /// Two slots can name one task: the session that took the task while the
    /// older one's connection still stands. The lookup the assignment path uses
    /// has to prefer the slot that is not read-only, because the handle it stores
    /// is what settles the server's delivery row. The unfixed lookup took
    /// whichever slot the hash map yielded first, so with eight read-only slots
    /// and one live it named a read-only handle in eight runs of nine — the retry
    /// kept waiting for an ack that had already been written for another session,
    /// and the server re-delivered the task to a role that had finished it. The
    /// fixed rule names the live slot every run.
    #[test]
    fn a_read_only_slot_never_holds_the_handle_of_the_task_it_lost() {
        let dir = tempdir().unwrap();
        let store = ClientStore::open(dir.path().join("client.db")).expect("client store");
        let state = DispatchState::new(
            "planner",
            dir.path(),
            Vec::new(),
            8,
            true,
            Arc::new(FakeBackend::new()),
            store,
        );
        let task = new_task_id();
        let serving = |read_only: bool| SessionSlot {
            session: SessionRef {
                task_id: task.clone(),
                backend: "fake".into(),
                backend_ref: serde_json::Value::Null,
                generation: 1,
            },
            family: "test".into(),
            task_id: Some(task.clone()),
            ready: true,
            payload: None,
            msg_id: None,
            origin: None,
            hop: 0,
            dropped_at: None,
            read_only,
        };
        state
            .inner
            .lock()
            .sessions
            .insert("serving".into(), serving(false));
        for index in 0..8 {
            state
                .inner
                .lock()
                .sessions
                .insert(format!("revived-{index}"), serving(true));
        }

        state.attach_msg_id(&task, "msg-serving");

        let inner = state.inner.lock();
        assert_eq!(
            inner.sessions["serving"].msg_id.as_deref(),
            Some("msg-serving"),
            "the session serving the task carries its delivery handle"
        );
        for index in 0..8 {
            let key = format!("revived-{index}");
            assert_eq!(
                inner.sessions[&key].msg_id, None,
                "the read-only slot {key} is handed no handle for a task it no longer serves"
            );
        }
    }
}
