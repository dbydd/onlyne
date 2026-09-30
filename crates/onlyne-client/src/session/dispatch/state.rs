use super::*;

use super::outbound::Outbox;
use super::projection::{projection_of, stored_task_state};
use super::transport::names_session;
use super::turn_end::TurnEndWatch;

#[derive(Clone)]
pub struct DispatchState {
    pub(super) inner: Arc<Mutex<DispatchInner>>,
}
pub(super) struct DispatchInner {
    pub(super) role: String,
    pub(super) workspace: PathBuf,
    pub(super) command: Vec<String>,
    /// The drive the role's spec declares (`[client.runtime] drive`). `None`
    /// until the first `welcome` names it.
    pub(super) drive: Option<onlyne_config::Drive>,
    /// The placement this machine resolved, the other half of the drive rule.
    /// `None` until the run resolves one; a pair the rule refuses is what
    /// [`super::env::reject_unpaired_runtime`] refuses a session over.
    pub(super) placement: Option<crate::backend::SessionPlacement>,
    /// Why a session may not open under this role's drive on this machine, when
    /// the pair is one the rule refuses. Every delivery the role is offered is
    /// refused with it: a backend the previous drive left behind must not serve
    /// work under a policy nobody set.
    pub(super) runtime_refusal: Option<String>,
    pub(super) max_sessions: u32,
    /// The roles a session of this role owes a delivery to, read off the
    /// server's spec slice (`allowed_targets`): the same list the server gates
    /// the ACL on is the obligation the completion guard measures a session
    /// against. Empty is the default and means the role owes nothing.
    pub(super) required_targets: Vec<String>,
    /// The role's workspace session policy: which deliveries one session serves,
    /// and how long an idle one may wait before its process is released (§10).
    pub(super) session_policy: onlyne_config::SessionPolicy,
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
    /// waiting for this role's next assignment (plan §6 line 285), oldest mount
    /// first.
    ///
    /// A queue and not a single slot, because a role can hold more than one
    /// always-running agent and the second mount that arrived naming no session
    /// has an open socket either way. Overwriting the first dropped its
    /// `AdapterIo` with no accounting of any kind: no release, no log, and no
    /// bye, so the role silently lost a worker that was waiting to be told.
    /// `revived` has held several connections the same way from the start.
    pub(super) parked: Vec<(AdapterIo, Vec<Capability>)>,
    /// Connections from **hosting** runtimes: ones that declared `open`,
    /// `suspend` or `close` and own their sessions rather than serving the one
    /// the client started them for.
    ///
    /// This is not the park with a different name. A parked connection is a
    /// *worker* waiting for one job — `claim_parked_transport` takes the oldest
    /// off the queue and that connection is spent, which is right for an agent
    /// the client spawned for a single session. A hosting connection is a shared
    /// resource: it takes no job off a queue because it is not waiting for one,
    /// and it serves whatever sessions the role opens. Putting one in `parked`
    /// would let the first staged session consume it, and the role's other three
    /// sessions would have nothing to run on.
    ///
    /// A role can hold both, and the order they are consulted in is
    /// `hand_staged`'s: a parked agent was spawned for the work in hand, so it
    /// is the more specific match, and a hosting connection is the fallback.
    pub(super) standing: Vec<(AdapterIo, Vec<Capability>)>,
    /// Zero-activity clock for running tasks. Applied persists refresh it.
    pub(super) stall: crate::session::stall::StallWatch,
    /// A plugin connection that mounted a session a live connection already
    /// serves: the agent that dropped came back after a newer session took the
    /// task. It is served no state, and the ending it reports is answered like
    /// any other connection's; the name it mounted with is what that judgement
    /// reads. The capabilities that mount came with are kept beside it, because
    /// a session whose live connection goes away hands itself to the first
    /// connection that was holding for it.
    pub(super) revived: Vec<(String, AdapterIo, Vec<Capability>)>,
    /// Tasks whose ending this client asked for with a `control` command.
    ///
    /// `recycle` and `cancel` reach the agent as a `notify`, so the completion
    /// that answers the command races the retirement the same command runs, and
    /// the row's phase at the moment the frame lands is that race's answer. The
    /// note is written before the frame leaves, so both orders of the race read
    /// one authority: the operator asked for this ending, and the settle door
    /// (`settle.rs`) takes the note as its `SettleAuthority::ControlDriven`.
    /// `on_out` consumes it, and the reconnect sweep drops the note of every task
    /// it settles — the other way a command's answer stops coming.
    ///
    /// A note nothing answers is the watchdog's, and it is the same note: the
    /// record of the word, held to [`CONTROL_SETTLE_BOUND`] and settled by the
    /// tick's own sweep when no report has come to settle it instead.
    pub(super) control_settles: Vec<ControlNote>,
    /// 3c's turn-end bookkeeping, keyed by the task whose delivery it belongs to.
    ///
    /// Which delivery already spent its one nudge, and which settled at that
    /// door. It stays here: no column holds it, no frame carries it, and no log
    /// line reads it, because the ending it remembers is one this client
    /// witnessed for itself (`docs/v2-CONTRACT.md` §3c).
    pub(super) turn_end: TurnEndWatch,
    /// Live `tools` mounts, one binding per session they speak for.
    ///
    /// A tools mount holds no process, so it is no transport and no parked
    /// agent: it lands in neither of those tables. This one exists to answer
    /// which session a connection speaks for, and nothing else consults it. The
    /// token itself stays in the session's own slot; an entry here carries the
    /// slot's key and the connection, never the token
    /// (`docs/v2-CONTRACT.md` §3b).
    pub(super) tools_mounts: Vec<(String, AdapterIo)>,
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
    /// The causality of the task this slot serves, read off the envelope that
    /// arrived with it. A handoff the session reports afterwards is its child,
    /// which is what `onlyne handoff` computes too. The whole link is kept here
    /// so the family id and the family's figures travel with the child, the
    /// depth included.
    pub(super) causality: Causality,
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
    /// slot is handed no assignment and no note: the work belongs to the session
    /// that took the task, and this one answers only for how its own turn ended.
    pub(super) read_only: bool,
    /// The task family this session was opened for, under the `task` scope. The
    /// scope hands that family's later deliveries here instead of opening a
    /// second conversation for one chain, so this is the key the resolution
    /// reads and the reason a session outlives the delivery that opened it.
    pub(super) family: Option<String>,
    /// When this session stopped serving a delivery, while it is still alive.
    /// The role's `idle_close` bound runs from here, and a slot serving a
    /// delivery has none.
    pub(super) idle_since: Option<Instant>,
    /// A session whose process has been released and the conversation lives in
    /// the runtime's own store: the next delivery bound to this session resumes
    /// it. A suspended slot spends no capacity, holds no transport, and answers
    /// no frame.
    pub(super) suspended: bool,
    /// The capability token a `tools` mount presents to speak for this session.
    ///
    /// Minted when the session opens and handed to the session's own drive
    /// through its spawn spec; it lives here, in the session's own state, so
    /// nothing outside the session may hand it out, it dies with the slot, and
    /// a session that reopens gets a new one. It is a capability, so it never
    /// reaches a log line, a fault, or a ledger row
    /// (`docs/v2-CONTRACT.md` §3b, `AGENTS.md` §8).
    pub(super) tools_token: String,
    /// The roles this session delivered to since it opened.
    ///
    /// A send the client carried is a handoff, whatever envelope kind it was,
    /// and this set is the evidence the relay guard reads at the session's next
    /// completion. It belongs to the session rather than to one delivery: the
    /// family's obligation outlives the task that opened it
    /// (`plugins/onlyne-agent-pi`, `guards.rs`).
    pub(super) delivered_roles: BTreeSet<String>,
    /// When this session was opened. The order a `role` pool hands its sessions
    /// out in: the one that has waited longest takes the delivery.
    pub(super) opened_at: Instant,
    /// The argv this session's runtime was started with, rendered when the
    /// session was born. Resuming it starts this command again rather than a
    /// freshly rendered one, because the command carries the runtime's own key
    /// for the conversation and may interpolate the delivery into it.
    pub(super) command: Vec<String>,
    /// This session's scope keeps it alive after a delivery settles.
    ///
    /// `oneshot` does not: that session's own id is the delivery that opened it,
    /// and when that delivery is answered the session is over. A `task` or
    /// `role` session serving nothing between deliveries is idle, which is a
    /// live session rather than an exit, and the verdict of a delivery it has
    /// already finished says nothing about it.
    pub(super) keeps_idle: bool,
}

/// One operator's word this client is still waiting to see answered.
///
/// The command is a `notify` the plugin may or may not live to answer, so the
/// note is what this client knows on its own: which task the word named, when it
/// was given, and what the operator said. It is not a second settle path — the
/// note is the authority of the one settle door, which `settle.rs` reads as
/// `SettleAuthority::ControlDriven` — and it is the record the watchdog holds a
/// word to once no report arrives at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlNote {
    /// The task the word named.
    pub task_id: String,
    /// When the word was given. The watchdog's bound runs from here.
    pub noted_at: Instant,
    /// What the operator said.
    pub word: ControlWord,
}

/// The operator's word one note is the record of.
///
/// `cancel` and `recycle` are the two commands that reach a live task, and each
/// word carries both halves of what a note is for: the ending it gives the work,
/// and the string a refusal of that task's delivery row is written with. The note
/// holds the word rather than the verdict it stands for, because the verdict does
/// not name the word back — `failed` is only the fallback a `recycle` leaves when
/// nothing answers it — and the column the refusal lands in has to read what the
/// operator actually said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlWord {
    /// `cancel`: the work ends now, and the task reads `cancelled`.
    Cancel,
    /// `recycle`: the plugin is asked for its own ending. It prescribes no
    /// outcome, so a word nothing answers leaves the task `failed`.
    Recycle,
}

impl ControlWord {
    /// The outcome this word stands for: the verdict a settle answering the note
    /// files when the plugin reports none of its own.
    pub fn outcome(self) -> Outcome {
        match self {
            Self::Cancel => Outcome::Cancelled,
            Self::Recycle => Outcome::Failed,
        }
    }

    /// The word as the refusal that names it reads on the wire.
    ///
    /// `operator cancel` and `operator recycle` stand in the same column as the
    /// `operator close` and `operator ack` an operator's other verbs already wrote
    /// there, and each names the command that was given rather than the verdict it
    /// left behind.
    pub fn refusal(self) -> &'static str {
        match self {
            Self::Cancel => "operator cancel",
            Self::Recycle => "operator recycle",
        }
    }
}

/// How long this client holds an operator's word open before settling it.
///
/// The words this bounds are `cancel` and `recycle`, and each one is a `notify`
/// the plugin answers with a frame on the connection it already serves: a plugin
/// that ends its turn to answer has answered inside one
/// [`HEARTBEAT_INTERVAL`](crate::session::dispatch::HEARTBEAT_INTERVAL), and the
/// request round trip the adapter bounds itself with
/// ([`REQUEST_TIMEOUT`](crate::session::dispatch::REQUEST_TIMEOUT)) is well past
/// that. Three intervals leaves a plugin that is stalled but still alive two
/// missed beats before this client decides the word went unanswered — it is the
/// window the reconnect sweep reads an agent's silence through, so the two
/// readings agree — and it is half the sixty-second default of `[client]
/// reconnect_grace_secs`. An operator watching a stuck row gave up on the live
/// run in seconds and reached for `onlyne repair fail`; a minute would lose to
/// that, and this does not.
///
/// A constant rather than a config key on purpose: what it bounds is not a policy
/// an operator tunes, it is the point past which this client's own record of the
/// word outlives the plugin that was asked to answer it.
pub const CONTROL_SETTLE_BOUND: Duration = Duration::from_secs(HEARTBEAT_INTERVAL.as_secs() * 3);

/// The notes whose operator's word has gone unanswered past
/// [`CONTROL_SETTLE_BOUND`].
///
/// The reading consumes nothing. The caller settles each note through
/// [`take_controlled_settle`](DispatchState::take_controlled_settle), the one
/// door that spends a note, so a completion that answers a word between this read
/// and that call takes the note first and the task needs no verdict from the
/// sweep. A note stamped ahead of `now` is not due: an elapsed window is the only
/// reading this makes.
pub(super) fn due_control_settles(inner: &DispatchInner, now: Instant) -> Vec<ControlNote> {
    inner
        .control_settles
        .iter()
        .filter(|note| now.saturating_duration_since(note.noted_at) >= CONTROL_SETTLE_BOUND)
        .cloned()
        .collect()
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
/// nothing. A frame this client refused on other grounds still buys the stamp:
/// the silence arm asks whether an agent lives behind the slot, and it reads
/// this stamp only for a session whose task is still bound and unsettled, so a
/// settled or taken task is never the stamp's question and a demoted slot still
/// retires on its own schedule. What a refused frame never buys is a write: the
/// dimensions stay where the connection serving the session left them.
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

/// Move one session's row onto the generation after the one it holds.
///
/// Two shapes leave a row behind a generation that is gone, and both close the
/// gap the same way. A plugin that comes back to a session this client still
/// serves reports from a new process, so its sequence starts at its base again.
/// A session this client stages onto a task that already carries a row is born
/// onto the record the session that last served the task left, watermark and
/// all. The watermark is the counter this client's own feeds and the plugin's
/// beats share — `upsert_session` writes the event's version into it, and
/// `next_version` allocates the stored one plus one — so an inherited one drops
/// every frame of the new reporter as a stale duplicate, and no dimension ever
/// moves on that row again. A same-generation rebase is not available: the
/// reducer's no-op detection compares the version-free tuple, so an event that
/// moved only the watermark would be ignored and the row would keep the old one.
/// The generation is what moves, and the sequence starts again under it.
///
/// The caller composes `body`, the content of the new generation, and the caller
/// attests the old one dead: `Supersede` is the reducer's vocabulary for that
/// pair, and each caller is the authority on the fact it asserts. Answers `None`
/// when the task carries no row yet — a first dispatch, which has nothing to
/// rebase because `feed_created` seeds the row instead.
pub(super) fn rebase_generation(
    inner: &DispatchInner,
    task_id: &str,
    body: impl FnOnce(&Observation) -> Observation,
) -> anyhow::Result<Option<Verdict>> {
    let Some(row) = inner.store.get_session(task_id)? else {
        return Ok(None);
    };
    let stored = stored_observation(&inner.store, Some(&row));
    let event = LifecycleEvent::Supersede {
        v: Version::new(stored.version.generation.saturating_add(1), 0),
        old_generation_dead: true,
        body: body(&stored),
    };
    Ok(Some(apply_persist(
        &inner.bridge,
        &inner.store,
        task_id,
        &event,
    )?))
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

pub(super) fn render_tokens(tokens: &[String], session: &str, task: &str) -> Vec<String> {
    tokens
        .iter()
        .map(|token| token.replace("{session}", session).replace("{task}", task))
        .collect()
}

/// The session a `tools` mount token spoke for, as the mount needs it.
///
/// The token *is* the binding (`docs/v2-CONTRACT.md` §3b): the role this mount
/// speaks for is the client's own, and the session and its generation come from
/// the client's record rather than from a field the caller supplies. A caller
/// that could name its own session could speak for one it never held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolsSession {
    /// The session's own id, the spelling the mount answers for.
    pub session_id: String,
    /// The generation the session was opened under, the third half of the
    /// `(role, session_id, generation)` record.
    pub generation: u64,
    /// The delivery the session serves right now, when one is open.
    pub task_id: Option<String>,
}

/// Mint the capability token one session's `tools` mount presents.
///
/// The value is random and per-session: it is a capability, so it is handed to
/// the session's own drive alone and never logged, faulted, or written down.
pub(super) fn mint_tools_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The slot one live session's tools token names.
///
/// A token names a session while that session lives and serves: the slot
/// carries both, so a token whose slot has retired — or whose tuple and verdict
/// project `Exited` — names nothing, and neither does one whose session is
/// suspended (no process to mount from) or read-only (a newer session took the
/// task, and a read-only slot serves no state). The mount that presents such a
/// token is refused. The scan is small (one role's slots) and the token never
/// leaves this process.
pub(super) fn slot_key_for_token(inner: &DispatchInner, token: &str) -> Option<String> {
    if token.is_empty() {
        return None;
    }
    inner
        .sessions
        .iter()
        .find(|(_, slot)| slot.tools_token == token && token_names_session(inner, slot))
        .map(|(key, _)| key.clone())
}

/// Whether one slot is a session a tools token may still name.
///
/// A token names a session while that session lives and serves: an `Exited`
/// projection is the session over, a suspended session has no process to mount
/// from, and a read-only slot is a session a newer one took the task from, which
/// serves no state. The three refusals are one predicate so the handshake, the
/// per-frame gate, and the stamped scope cannot disagree about which tokens
/// still name anything.
pub(super) fn token_names_session(inner: &DispatchInner, slot: &SessionSlot) -> bool {
    !slot.suspended && !slot.read_only && !slot_exited(inner, slot)
}

/// What one live tools connection speaks for.
///
/// Everything a tools frame is stamped with comes from here, so the fields are
/// the session's own record rather than anything the mount supplied: the
/// delivery its `report` and `handoff` frames name, and the session a `send`
/// frame leaves from (`docs/v2-CONTRACT.md` §3b). A `send` starts a family of
/// its own, so no chain of this session's rides out with it.
#[derive(Clone, Debug)]
pub struct ToolsScope {
    /// The session's own id, the key its slot and row are held under.
    pub session_id: String,
    /// The delivery the session serves right now, when one is open.
    pub task_id: Option<String>,
}

/// The mount-side facts of one session slot.
pub(super) fn tools_session_of(inner: &DispatchInner, key: &str) -> Option<ToolsSession> {
    inner.sessions.get(key).map(|slot| ToolsSession {
        session_id: slot.session.task_id.clone(),
        generation: slot.session.generation,
        task_id: slot.task_id.clone(),
    })
}

/// Drop the tools binding one session held, when the session goes away.
///
/// A binding holds a live connection handle, and a slot that leaves this
/// client's books leaves nothing for the mount to speak for: the entry goes
/// with it so a retired session's socket is not kept open by this table.
pub(super) fn forget_tools_binding(inner: &mut DispatchInner, key: &str) {
    inner.tools_mounts.retain(|(held, _)| held != key);
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

/// How the delivery one slot is serving ended, read from the task's own record.
///
/// A slot serving nothing answers `Pending`. That is not a guess about work in
/// flight: no delivery of this session is open for a verdict right now, and
/// `project` reads the pair as a live session rather than as an exit — which is
/// what an idle `task` or `role` session is. The verdict of a delivery it has
/// already finished belongs to that delivery, and says nothing about a session
/// the scope kept open for the family's next one.
pub(super) fn binding_task_state(inner: &DispatchInner, slot: &SessionSlot) -> TaskState {
    match slot.task_id.as_deref() {
        Some(task_id) => stored_task_state(inner, task_id),
        None if slot.keeps_idle => TaskState::Pending,
        // A session no scope keeps has one delivery behind it and no next one:
        // its own id is that delivery's record, and its verdict is the answer.
        None => stored_task_state(inner, &slot.session.task_id),
    }
}

/// Whether the session one slot holds is over.
///
/// The slot's own id addresses the row: a session that has served several
/// deliveries has one row and a moving binding, and the row is the session's.
/// A slot with no row at all has not been written about yet and is live.
pub(super) fn slot_exited(inner: &DispatchInner, slot: &SessionSlot) -> bool {
    let Some(row) = inner
        .store
        .get_session(&slot.session.task_id)
        .ok()
        .flatten()
    else {
        return false;
    };
    projection_of(&row, binding_task_state(inner, slot)).lifecycle == Lifecycle::Exited
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
///
/// A suspended session spends nothing: the scope's rules count active sessions,
/// and a session whose process has been released is what freeing a slot means.
pub(super) fn live_sessions(inner: &DispatchInner) -> usize {
    inner
        .sessions
        .values()
        .filter(|slot| !slot.suspended && !slot_exited(inner, slot))
        .count()
}
