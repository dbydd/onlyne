use super::*;

use super::outbound::Outbox;
use super::projection::{projection_of, stored_task_state};
use super::transport::names_session;

#[derive(Clone)]
pub struct DispatchState {
    pub(super) inner: Arc<Mutex<DispatchInner>>,
}
pub(super) struct DispatchInner {
    pub(super) role: String,
    pub(super) workspace: PathBuf,
    pub(super) command: Vec<String>,
    pub(super) max_sessions: u32,
    /// Downstream roles a session of this role owes a handoff to, from the
    /// server's spec slice (`relay_required`). Empty is the default and means
    /// the guard is off.
    pub(super) relay_required: Vec<String>,
    /// The count form of the same policy (`relay_count`).
    pub(super) relay_count: Option<u32>,
    pub(super) backend: Arc<dyn SessionBackend>,
    pub(super) store: ClientStore,
    pub(super) bridge: Bridge,
    pub(super) sessions: HashMap<String, SessionSlot>,
    /// Live link, installed by the runloop while the connection is up.
    pub(super) outbox: Option<Arc<dyn Outbox>>,
    /// Flag the runloop and the dispatcher share while the link is down.
    pub(super) accept_new: Arc<AtomicBool>,
    /// Whether the role holds a ready server link. The runloop owns it, and the
    /// adapter socket reports it to the `status` verb.
    pub(super) link_up: Arc<AtomicBool>,
    /// Aggregate name this role supervises, empty for a plain role.
    pub(super) cluster_ref: String,
    /// The server's topology name, read from `welcome.cluster` (the server's own
    /// `spec.toml [server] name`). A host backend uses it as the address of the
    /// tree it puts sessions into: herdr keeps one workspace per server root,
    /// labelled after this name. Empty until the first welcome arrives.
    pub(super) topology: String,
    /// The adapter connection serving each session of this role, keyed by the
    /// session id the plugin mounted with (`ONLYNE_SESSION_ID`). A plugin the
    /// client spawned names the one session it was spawned for, so a task
    /// never rides the connection of an earlier one.
    pub(super) transports: HashMap<String, (AdapterIo, Vec<Capability>)>,
    /// A plugin that mounted naming no session: an always-running agent
    /// waiting for this role's next assignment (plan §6 line 285).
    pub(super) parked: Option<(AdapterIo, Vec<Capability>)>,
    /// Zero-activity clock for running tasks. Applied persists refresh it.
    pub(super) stall: crate::session::stall::StallWatch,
    /// A plugin connection that mounted a session a live connection already
    /// serves: the agent that dropped came back after a newer session took the
    /// task. It is served nothing, and what it sends is held rather than sent,
    /// keyed by the name it mounted with. The capabilities that mount came with
    /// are kept beside it, because a session whose live connection goes away
    /// hands itself to the first connection that was holding for it.
    pub(super) revived: Vec<(String, AdapterIo, Vec<Capability>)>,
    /// What those held connections sent, keyed by the task whose completion
    /// carries it. `on_out` drains the key before it routes, so the recipient
    /// reads one relay per downstream role.
    pub(super) held_handoffs: HashMap<String, Vec<Handoff>>,
    /// Connections inside one of their own inbound frames right now.
    ///
    /// A frame handler runs to completion before `adapter_socket` answers the
    /// frame, so a host frame written on the same connection during that handler
    /// leaves first. The bye sweep in `retire_revived` skips these connections.
    pub(super) in_frame: Vec<AdapterIo>,
}

#[derive(Clone)]
pub struct SessionSlot {
    pub(super) session: SessionRef,
    pub(super) task_id: Option<String>,
    pub(super) ready: bool,
    /// Payload held until the adapter reports ready, which keeps the ready
    /// barrier of §6 ahead of the `assign` frame.
    pub(super) payload: Option<Envelope>,
    /// Delivery handle, owed back to the server as one `ack`.
    pub(super) msg_id: Option<String>,
    /// Sender of the payload this session serves, kept for its `Completion`.
    pub(super) origin: Option<Principal>,
    /// How deep the task this slot serves sits in its chain, read off the
    /// envelope that arrived with it. A handoff the session reports afterwards
    /// is one hop below this, which is what `onlyne handoff` computes too.
    pub(super) hop: u32,
    /// When the connection that would have sent this session's next heartbeat
    /// last left it: at birth for a session a plugin still has to mount, and
    /// again each time a connection ends without a `detach` frame or says
    /// goodbye while the session still owes a task. `None` while a connection is
    /// attached, which is what clears it, and for a self-driven session, which
    /// answers no adapter socket at all. The reconnect grace of `[client]
    /// reconnect_grace_secs` reads it: an agent that comes back inside the window
    /// clears it and keeps its session.
    pub(super) dropped_at: Option<Instant>,
    /// When a frame this session sent was last accepted.
    ///
    /// The other half of the same death window, and the half a socket cannot
    /// answer for: a plugin whose event loop is blocked keeps its connection and
    /// stops beating, so `dropped_at` stays `None` and no socket ever ends. Set
    /// when the session is staged and refreshed wherever a frame of its own is
    /// accepted, so it reads as the moment the agent last proved it was alive.
    /// The reconnect sweep compares it against the protocol's heartbeat cadence.
    pub(super) last_beat: Option<Instant>,
    /// Whether this session's task has been taken by a newer session, leaving
    /// this slot served only by a connection that came back for it. A read-only
    /// slot is handed no assignment and no note, and what its agent sends is
    /// held for the completion that merges it.
    pub(super) read_only: bool,
}

/// Stamp the moment one session last had a frame of its own accepted.
///
/// The stamp is the liveness half of the reconnect sweep: a socket that is still
/// up and still attached proves the connection survived, not that the agent
/// behind it did. A plugin whose event loop is blocked keeps its socket and
/// stops beating, and nothing but this stamp says so.
///
/// Called where a frame is accepted rather than where one arrives — the mount
/// that binds the connection, the ready barrier, and each beat the reducer took
/// — because a frame the client refused moved no state and is the evidence of
/// nothing. It is deliberately not called for a read-only connection's frames:
/// an agent whose session was taken by a newer one is not the agent this stamp
/// is about.
pub(super) fn note_beat(inner: &mut DispatchInner, task_id: &str, now: Instant) {
    let Some(key) = slot_key_named(inner, task_id) else {
        return;
    };
    if let Some(slot) = inner.sessions.get_mut(&key) {
        slot.last_beat = Some(now);
    }
}

pub(super) fn has_attached_transport(inner: &DispatchInner, key: &str, slot: &SessionSlot) -> bool {
    inner
        .transports
        .keys()
        .any(|session_id| names_session(key, slot, session_id))
}

/// The task one slot answers for: its current binding, or the task its session
/// was spawned for while no task is bound.
pub(super) fn slot_task(slot: &SessionSlot) -> String {
    slot.task_id
        .clone()
        .unwrap_or_else(|| slot.session.task_id.clone())
}

/// The slot one adapter mount names, answered as its key.
pub(super) fn slot_key_named(inner: &DispatchInner, session_id: &str) -> Option<String> {
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
pub(super) fn slot_key_serving_task(inner: &DispatchInner, task_id: &str) -> Option<String> {
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

/// One plugin connection held inside an inbound frame it is still being answered.
///
/// The bye sweep in `retire_revived` leaves such a connection alone, which keeps a
/// bye behind the response to the frame. A plugin's bye handler drops the socket and
/// rejects every request awaiting an answer, so a bye that overtakes the response
/// turns work the ledger already holds into a failure the agent reports again.
#[must_use = "the connection stops being held as soon as the guard is dropped"]
pub struct FrameGuard<'a> {
    pub(super) state: &'a DispatchState,
    pub(super) io: AdapterIo,
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

/// How deep an inbound task sits in its chain. An envelope that names no
/// causality is a root, and a relay born from it takes the hop below this.
pub(super) fn hop_of(envelope: &Envelope) -> u32 {
    envelope
        .causality
        .as_ref()
        .map(|causality| causality.hop)
        .unwrap_or(0)
}

pub(super) fn render_tokens(tokens: &[String], session: &str, task: &str) -> Vec<String> {
    tokens
        .iter()
        .map(|token| token.replace("{session}", session).replace("{task}", task))
        .collect()
}

/// Whether the session of one task is over.
///
/// The answer is derived, never read: `client.db` holds the session tuple the
/// reducer wrote under the `(generation, seq)` gate, the task table holds the
/// verdict the agent filed, and `project` is the only thing that says whether
/// that pair is `exited` — so a session whose work finished cleanly leaves here,
/// and one whose agent went away leaves here too. The rows stay readable after
/// the session ends. A session with no row yet is live: it exists as a spawned
/// resource alone, with nothing derived from it.
pub(super) fn session_exited(inner: &DispatchInner, task_id: &str) -> bool {
    let Some(row) = inner.store.get_session(task_id).ok().flatten() else {
        return false;
    };
    projection_of(&row, stored_task_state(inner, task_id)).lifecycle == Lifecycle::Exited
}

/// Sessions that hold the role's concurrency: the staged slots whose tuple and
/// task verdict have not projected to `Exited`.
///
/// §5's `max_sessions` caps concurrent sessions, and a session the reducer has
/// ended answers no task, so it stops spending capacity the moment its tuple and
/// verdict derive `exited` — whether that came from a completion report the task
/// table settled or from the observation a plugin sends as its last heartbeat.
/// The rows stay in `client.db` and stay queryable; a session with no row at all
/// is live.
pub(super) fn live_sessions(inner: &DispatchInner) -> usize {
    inner
        .sessions
        .values()
        .filter(|slot| !session_exited(inner, &slot.session.task_id))
        .count()
}
