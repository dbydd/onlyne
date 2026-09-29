//! The `view` reducer: one state for the TUI and the web.
//!
//! [`snapshot_to_view`] builds a [`View`] from the rows the admin reads already
//! return; [`update`] folds one event off slice 4's `subscribe` stream into it.
//! Both are total, pure, and free of I/O, clocks, and randomness, so a golden
//! case pins the whole fold without a server, a socket, or a terminal. The front
//! ends own the IO task; this module owns nothing but the fold.
//!
//! ## The two axes stay apart
//!
//! The plan keeps a delivery's state (`queued` → `in_flight` → settled) and a
//! session's state (`busy` / `idle` / `suspended` / `closed`) as two facts about
//! two different things (`docs/v2-PLAN.md` line 249), and v1 squashed them into
//! one projection row. This module keeps them in two rows: [`DeliveryView`]
//! holds the ledger's own word for one send, [`SessionView`] holds the session's
//! four dimensions plus its binding, and nothing holds a row that merges the
//! two. Every reading that talks about both — the axis a delivery sits on
//! ([`DeliveryView::axis`]), the state a session is in ([`SessionView::state`]),
//! the column a card lands in ([`Card::column`]) — is a pure function over the
//! two rows, computed where it is read rather than stored. A delivery that
//! settled while its session is suspended is two rows a reader sees side by
//! side: not a contradiction to resolve, and this reducer has no way to resolve
//! it.
//!
//! ## A gap is a first-class input
//!
//! The synthetic [`RESYNC_LAG_KIND`] fault a lagging subscriber receives marks
//! [`View::stale`] instead of being folded in as ordinary news, and the next
//! snapshot clears it. A renderer can then say "catching up" rather than draw a
//! state that silently lost events.
//!
//! ## What the fold does not own
//!
//! A class the fold has no fact for leaves the view unchanged: [`update_class`]
//! is the boundary a front end's IO task sits on, and it folds a class this
//! build knows while an unknown class is folded as nothing rather than rendered
//! as news. Classes that are news but carry no state change — a gateway's
//! health, a spec reload, the turn-end family — reach [`View::event_tail`] and
//! move nothing else; each arm below says why.

use crate::envelope::{MsgKind, Outcome, Principal};
use crate::event::{
    Event, FaultEvent, LedgerState, LedgerStateEvent, Lifecycle, SessionStateEvent,
};
use crate::lifecycle::{AgentPhase, DeliveryPhase, RecoveryPhase, ResourcePhase};
use crate::ops::{LedgerEntry, RoleInfo, SessionRow};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The event class a lagging subscriber receives instead of the events it lost
/// (`docs/v2-CONTRACT.md` §"Slice 4").
///
/// The word is the one `onlyne_net::RESYNC_LAG_KIND` and
/// `onlyne_server::events::RESYNC_LAG_KIND` publish. It is spelled here too
/// because this crate may not depend on either, and the fold has to recognise
/// the notice to tell a gap from an ordinary fault; the cutover is one
/// definition in [`crate::event`], beside the classes it belongs with.
pub const RESYNC_LAG_KIND: &str = "resync_lag";

/// The fault state word a fault nothing has moved carries
/// (`onlyne_server::faults::STATE_OPEN`).
pub const FAULT_STATE_OPEN: &str = "open";

/// How many events the cluster page's tail keeps. A stream that runs for days
/// would otherwise grow the view without bound, and the tail exists to show
/// what just happened.
pub const EVENT_TAIL_LIMIT: usize = 128;

/// The delivery axis: the three places the plan names, `queued` → `in_flight`
/// → settled.
///
/// A row stores the ledger's own word for its state ([`LedgerState`], the
/// wire's vocabulary); this is the axis reading of it, derived where a board
/// asks for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryAxis {
    /// Accepted for a recipient that is not connected yet.
    Queued,
    /// Handed to the recipient's client, awaiting `ack`.
    InFlight,
    /// The recipient settled it: `acked`, `rejected`, or `expired`.
    Settled,
}

impl DeliveryAxis {
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryAxis::Queued => "queued",
            DeliveryAxis::InFlight => "in_flight",
            DeliveryAxis::Settled => "settled",
        }
    }
}

/// The session axis: the state machine the plan draws (line 236's diagram).
///
/// Derived on demand by [`SessionView::state`], never stored: the row keeps the
/// four dimensions the reducer owns, and a stored reading would be a second
/// copy of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Opened and booting, before its first turn.
    Opening,
    /// Working: a turn in flight, an intent unanswered, or an ending the
    /// session still carries.
    Busy,
    /// Ready with nothing in flight, or between deliveries.
    Idle,
    /// The client released the session's process. The conversation waits in the
    /// runtime's own store and its slot is free.
    Suspended,
    /// The session is over.
    Closed,
}

/// The five columns a board arranges its cards into: the joint projection of
/// the two axes the plan names (`docs/v2-PLAN.md` line 373).
///
/// This is the one place the axes are read together, and it is a *reading*:
/// [`Card::column`] derives it from the delivery row and the session row every
/// time it is asked, so no row ever holds a merged state.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BoardColumn {
    /// Queued: accepted, not handed over yet.
    Queued,
    /// Running: handed over, and a session is working it.
    Running,
    /// Waiting: handed over, and nothing is working it yet.
    Waiting,
    /// Done: settled with nothing waiting on it.
    Done,
    /// Failed or blocked: everything else that settled.
    FailedOrBlocked,
}

impl BoardColumn {
    /// The column's own word, in the plan's order.
    pub fn as_str(self) -> &'static str {
        match self {
            BoardColumn::Queued => "queued",
            BoardColumn::Running => "running",
            BoardColumn::Waiting => "waiting",
            BoardColumn::Done => "done",
            BoardColumn::FailedOrBlocked => "failed_or_blocked",
        }
    }
}

/// One delivery: the ledger's row for one send, as the delivery axis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct DeliveryView {
    /// The ledger row's key.
    pub msg_id: String,
    pub op_id: Option<String>,
    pub kind: MsgKind,
    pub from: Principal,
    pub to: Principal,
    /// The delivery this row carries, which is also how a reader reaches the
    /// session serving it ([`View::session_for`]).
    pub task_id: Option<String>,
    /// The family's root task id and the hop this row sits at, read off the
    /// stored envelope's causality. The `ledger` read owns these; a row this
    /// view first saw on the stream carries none of them, and the next snapshot
    /// fills them in.
    pub family: Option<String>,
    pub hop: Option<u32>,
    pub origin: Option<String>,
    pub attempt: Option<u32>,
    /// The ledger's own word for this row's state. The axis reading is
    /// [`Self::axis`].
    pub state: LedgerState,
    /// The task verdict the ledger mirrors, when the settling event carried
    /// one. The durable carrier of a blocked delivery is the session row's own
    /// outcome ([`SessionView::outcome`]), which a snapshot re-read restores.
    pub outcome: Option<Outcome>,
    pub reason: Option<String>,
    pub out_head: Option<String>,
    /// When the row was accepted. Absent on a row first seen on the stream,
    /// which carries no clock.
    pub enqueued_at: Option<DateTime<Utc>>,
    pub acked_at: Option<DateTime<Utc>>,
}

impl DeliveryView {
    /// The delivery axis this row sits on.
    pub fn axis(&self) -> DeliveryAxis {
        match self.state {
            LedgerState::Queued => DeliveryAxis::Queued,
            LedgerState::InFlight => DeliveryAxis::InFlight,
            LedgerState::Acked | LedgerState::Rejected | LedgerState::Expired => {
                DeliveryAxis::Settled
            }
        }
    }

    /// The `ledger` read's row.
    pub fn from_entry(entry: &LedgerEntry) -> Self {
        Self {
            msg_id: entry.msg_id.clone(),
            op_id: entry.op_id.clone(),
            kind: entry.kind,
            from: entry.from.clone(),
            to: entry.to.clone(),
            task_id: entry.task.clone(),
            family: entry.family.clone(),
            hop: Some(entry.hop),
            origin: entry.origin.clone(),
            attempt: Some(entry.attempt),
            state: entry.state,
            // `ledger` has no outcome column: the verdict a settled row carries
            // lives on the session that took it.
            outcome: None,
            reason: entry.reason.clone(),
            out_head: entry.out_head.clone(),
            enqueued_at: Some(entry.enqueued_at),
            acked_at: entry.acked_at,
        }
    }

    /// The row a first `ledger_state` event for a row this view has not read.
    fn from_event(reported: &LedgerStateEvent) -> Self {
        Self {
            msg_id: reported.msg_id.clone(),
            op_id: reported.op_id.clone(),
            kind: reported.kind,
            from: reported.from.clone(),
            to: reported.to.clone(),
            task_id: reported.task.clone(),
            family: None,
            hop: None,
            origin: None,
            attempt: None,
            state: reported.state,
            outcome: reported.outcome,
            reason: reported.reason.clone(),
            out_head: None,
            enqueued_at: None,
            acked_at: None,
        }
    }

    /// Fold one `ledger_state` event into a row the snapshot already read.
    ///
    /// The event is a transition: it carries what changed and not the row's
    /// causality, so the fields it has no word for keep the snapshot's value.
    fn absorb(&mut self, reported: &LedgerStateEvent) {
        self.op_id = reported.op_id.clone();
        self.kind = reported.kind;
        self.from = reported.from.clone();
        self.to = reported.to.clone();
        self.task_id = reported.task.clone();
        self.state = reported.state;
        self.outcome = reported.outcome;
        self.reason = reported.reason.clone();
    }
}

/// One session: the session axis, as the `sessions` read and `session_state`
/// events between them can prove.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SessionView {
    /// The session table's key.
    pub session_id: String,
    pub role: Option<String>,
    /// The delivery this session is currently serving, read off its open
    /// binding. A session a client holds but has not bound carries none.
    pub task_id: Option<String>,
    /// The row's version watermark: `(generation, seq)`, the pair
    /// `onlyne_server` gates its own writes with. A stream row below it is a
    /// replay and is dropped by [`update`].
    pub generation: u64,
    pub seq: u64,
    pub lifecycle: Lifecycle,
    pub agent: AgentPhase,
    pub delivery: DeliveryPhase,
    pub resource: ResourcePhase,
    pub recovery: RecoveryPhase,
    /// The task's verdict, as the client that owns it published it. A
    /// `blocked` verdict is the delivery that waits on something outside it.
    pub outcome: Option<Outcome>,
    /// The raw reducer observation, retained for repair and forensics.
    pub observed: Option<Value>,
    /// The operator who filed the last write on this session's behalf over the
    /// admin surface. Absent when the session reported on itself.
    pub admin: Option<Principal>,
    /// When the projection content last moved, from the `sessions` read. A
    /// stream event moves the row and carries no stamp, so this is cleared
    /// there rather than left reading older than the movement.
    pub updated_at: Option<String>,
    /// When this session was last seen at all, from the `sessions` read, which
    /// is what a reader judges the row's freshness by. Cleared on a stream
    /// event for the same reason as [`Self::updated_at`].
    pub last_seen: Option<String>,
    /// True when a working row the server has seen is silent past heartbeat
    /// grace. The `sessions` read decides this; a stream event clears it.
    pub heartbeat_stale: bool,
}

impl SessionView {
    /// The session axis: the state this row is in.
    ///
    /// Read in this order, each step a fact the tuple can prove:
    /// - an exited lifecycle or a gone agent is a session that is over;
    /// - `working` is open work — a turn in flight, an unanswered intent, or an
    ///   ending the session still carries (`project`'s own rule);
    /// - a closed resource on a session that is otherwise done working is the
    ///   client's release, which is what `Suspend` writes;
    /// - a created row has not taken its first turn yet;
    /// - everything else is idle.
    pub fn state(&self) -> SessionState {
        if self.lifecycle == Lifecycle::Exited || self.agent == AgentPhase::Gone {
            return SessionState::Closed;
        }
        if self.lifecycle == Lifecycle::Working {
            return SessionState::Busy;
        }
        if self.resource == ResourcePhase::Closed {
            return SessionState::Suspended;
        }
        if self.lifecycle == Lifecycle::Created {
            return SessionState::Opening;
        }
        SessionState::Idle
    }

    /// True when the task's verdict is a delivery waiting on something outside
    /// it, rather than work that failed. A board reads it as waiting.
    pub fn is_blocked(&self) -> bool {
        self.outcome == Some(Outcome::Blocked)
    }

    /// The `sessions` read's row.
    pub fn from_row(row: &SessionRow) -> Self {
        Self {
            session_id: row.session_id.clone(),
            role: row.role.clone(),
            task_id: row.task_id.clone(),
            generation: row.generation,
            seq: row.seq,
            lifecycle: row.projection.lifecycle,
            agent: row.projection.agent,
            delivery: row.projection.delivery,
            resource: row.projection.resource,
            recovery: row.projection.recovery,
            outcome: row.projection.outcome,
            observed: row.projection.observed.clone(),
            admin: None,
            updated_at: row.updated_at.clone(),
            last_seen: row.last_seen.clone(),
            heartbeat_stale: row.heartbeat_stale,
        }
    }

    /// The row a `session_state` event reports for a session this view has not
    /// read. The event carries the projection whole, so the row is complete
    /// except for the liveness stamps only the `sessions` read can date.
    fn from_event(reported: &SessionStateEvent) -> Self {
        Self {
            session_id: reported.session_id.clone(),
            role: Some(reported.role.clone()),
            task_id: reported.task_id.clone(),
            generation: reported.generation,
            seq: reported.seq,
            lifecycle: reported.projection.lifecycle,
            agent: reported.projection.agent,
            delivery: reported.projection.delivery,
            resource: reported.projection.resource,
            recovery: reported.projection.recovery,
            outcome: reported.projection.outcome,
            observed: reported.projection.observed.clone(),
            admin: reported.admin.clone(),
            updated_at: None,
            last_seen: None,
            heartbeat_stale: false,
        }
    }

    /// Fold one `session_state` event into this row, the caller having checked
    /// the version watermark.
    fn absorb(&mut self, reported: &SessionStateEvent) {
        self.role = Some(reported.role.clone());
        self.task_id = reported.task_id.clone();
        self.generation = reported.generation;
        self.seq = reported.seq;
        self.lifecycle = reported.projection.lifecycle;
        self.agent = reported.projection.agent;
        self.delivery = reported.projection.delivery;
        self.resource = reported.projection.resource;
        self.recovery = reported.projection.recovery;
        self.outcome = reported.projection.outcome;
        self.observed = reported.projection.observed.clone();
        self.admin = reported.admin.clone();
        // The event does not carry the liveness stamps, and the row just moved,
        // so the snapshot's stamps read older than the movement they date.
        self.updated_at = None;
        self.last_seen = None;
        self.heartbeat_stale = false;
    }
}

/// The cluster header, read off the `status` answer.
///
/// The server owns this shape (`onlyne_server::router::status`, whose keys its
/// own test pins); every field is optional so a key this build does not know
/// reads as absent rather than as a plausible zero. The gateway rows it also
/// answers are not mirrored here: they are not one of the three screens' facts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", default)]
pub struct ClusterSummary {
    pub cluster: Option<String>,
    pub version: Option<String>,
    pub spec_hash: Option<String>,
    pub role_count: Option<u64>,
    pub gateway_count: Option<u64>,
    pub routes: Option<u64>,
    pub channels: Option<u64>,
    pub connected_roles: Option<u64>,
    pub connected_gateways: Option<u64>,
    /// The newest event `seq` the server has.
    pub event_head: Option<u64>,
    pub uptime_s: Option<i64>,
}

impl ClusterSummary {
    /// Read the header off one `status` answer.
    pub fn from_status(status: &Value) -> Self {
        fn text(status: &Value, key: &str) -> Option<String> {
            status.get(key)?.as_str().map(str::to_string)
        }
        fn number(status: &Value, key: &str) -> Option<u64> {
            status.get(key)?.as_u64()
        }
        Self {
            cluster: text(status, "cluster"),
            version: text(status, "version"),
            spec_hash: text(status, "spec_hash"),
            role_count: number(status, "role_count"),
            gateway_count: number(status, "gateway_count"),
            routes: number(status, "routes"),
            channels: number(status, "channels"),
            connected_roles: number(status, "connected_roles"),
            connected_gateways: number(status, "connected_gateways"),
            event_head: number(status, "event_head"),
            uptime_s: status.get("uptime_s").and_then(Value::as_i64),
        }
    }
}

/// How many sessions of one role are busy, idle, or suspended: the three
/// numbers the plan's board header shows (line 375).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", default)]
pub struct SessionCounts {
    pub busy: u32,
    pub idle: u32,
    pub suspended: u32,
}

/// One role's card: a delivery on that role, and the session serving it.
///
/// A reading, not a row: both halves are borrowed from [`View`], so a card can
/// never disagree with the rows it came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Card<'a> {
    /// The delivery, whose `to` names the board it belongs to.
    pub delivery: &'a DeliveryView,
    /// The session serving it, when this view has read one for its task.
    pub session: Option<&'a SessionView>,
}

impl Card<'_> {
    /// The column this card sits in: the joint reading of the two axes.
    pub fn column(&self) -> BoardColumn {
        match self.delivery.axis() {
            DeliveryAxis::Queued => BoardColumn::Queued,
            DeliveryAxis::InFlight => match self.session.map(SessionView::state) {
                Some(SessionState::Busy) => BoardColumn::Running,
                _ => BoardColumn::Waiting,
            },
            DeliveryAxis::Settled => match self.delivery.state {
                LedgerState::Acked => match self.session.and_then(|session| session.outcome) {
                    Some(Outcome::Blocked) => BoardColumn::FailedOrBlocked,
                    _ => BoardColumn::Done,
                },
                _ => BoardColumn::FailedOrBlocked,
            },
        }
    }

    /// True when the work waits rather than being done or failed: handed to a
    /// session that is not running it, or settled `blocked` because it waits on
    /// something outside the delivery.
    pub fn waits(&self) -> bool {
        self.column() == BoardColumn::Waiting || self.session.is_some_and(SessionView::is_blocked)
    }
}

/// The five admin reads one snapshot is made of, as the answers carry them.
///
/// A front end fills this from `status`, `roles`, `sessions`, `ledger`, and
/// `faults` and hands it to [`snapshot_to_view`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", default)]
pub struct Snapshot {
    /// The `status` answer, verbatim: the server owns its shape.
    pub status: Option<Value>,
    pub roles: Vec<RoleInfo>,
    pub sessions: Vec<SessionRow>,
    pub ledger: Vec<LedgerEntry>,
    pub faults: Vec<FaultEvent>,
}

/// One state the TUI and the web both render.
///
/// The maps are keyed by each row's own id, so serving the three screens is a
/// matter of indexing rather than of a second shape: the cluster page reads
/// [`Self::cluster`] and [`Self::roles`] with [`Self::counts`]; the task page
/// filters [`Self::deliveries`] by `family` and reaches the serving session
/// through [`Self::session_for`]; the faults page reads [`Self::open_faults`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", default)]
pub struct View {
    pub cluster: ClusterSummary,
    pub roles: BTreeMap<String, RoleInfo>,
    pub sessions: BTreeMap<String, SessionView>,
    pub deliveries: BTreeMap<String, DeliveryView>,
    pub faults: BTreeMap<i64, FaultEvent>,
    /// What just happened on the stream, newest first, capped at
    /// [`EVENT_TAIL_LIMIT`]. A gap notice never enters it.
    pub event_tail: Vec<Event>,
    /// True when a resync notice arrived: the stream lost events, so what this
    /// view holds is not the whole truth. The next snapshot clears it.
    pub stale: bool,
}

impl View {
    /// The session serving one delivery, read off the binding both rows carry.
    ///
    /// This is the one link between the two axes, and it is a lookup: the
    /// delivery row keeps its own state and the session row keeps its own, so a
    /// settled delivery on a suspended session reads as both.
    pub fn session_for(&self, task_id: &str) -> Option<&SessionView> {
        self.sessions
            .values()
            .find(|session| session.task_id.as_deref() == Some(task_id))
    }

    /// One role's cards: every delivery addressed to it.
    ///
    /// Rows come in `msg_id` order, and the board's arrangement is
    /// [`Card::column`] over each one.
    pub fn cards<'a>(&'a self, role: &'a str) -> impl Iterator<Item = Card<'a>> + 'a {
        self.deliveries
            .values()
            .filter_map(move |delivery| match principal_role(&delivery.to) {
                Some(to) if to == role => Some(Card {
                    delivery,
                    session: delivery
                        .task_id
                        .as_deref()
                        .and_then(|task_id| self.session_for(task_id)),
                }),
                _ => None,
            })
    }

    /// One role's session counts, for the board header.
    ///
    /// Sessions that have not started ([`SessionState::Opening`]) and sessions
    /// that are over ([`SessionState::Closed`]) are in none of the three: the
    /// header counts the three states a session can still be worked from.
    pub fn counts(&self, role: &str) -> SessionCounts {
        let mut counts = SessionCounts::default();
        for session in self.sessions.values() {
            if session.role.as_deref() != Some(role) {
                continue;
            }
            match session.state() {
                SessionState::Busy => counts.busy += 1,
                SessionState::Idle => counts.idle += 1,
                SessionState::Suspended => counts.suspended += 1,
                SessionState::Opening | SessionState::Closed => {}
            }
        }
        counts
    }

    /// One task family's deliveries, unordered: the task page's path across
    /// roles, which a renderer orders by `hop`.
    pub fn family<'a>(&'a self, family: &'a str) -> impl Iterator<Item = &'a DeliveryView> + 'a {
        self.deliveries
            .values()
            .filter(move |delivery| delivery.family.as_deref() == Some(family))
    }

    /// The faults page's rows: the ones no repair verb has moved.
    pub fn open_faults(&self) -> impl Iterator<Item = &FaultEvent> {
        self.faults.values().filter(|fault| fault_is_open(fault))
    }

    /// The task page's session log for one session, newest first.
    pub fn session_log<'a>(&'a self, session_id: &'a str) -> impl Iterator<Item = &'a Event> + 'a {
        self.event_tail.iter().filter(move |event| match event {
            Event::SessionState(reported) => reported.session_id == session_id,
            _ => false,
        })
    }
}

/// Whether a fault is one no repair verb has moved.
///
/// A recorded fault carries the `open` word; every repair verb publishes the
/// word it moved the row to. An absent word is a row written before the column
/// carried one, read as open because nothing can have moved it.
pub fn fault_is_open(fault: &FaultEvent) -> bool {
    fault
        .state
        .as_deref()
        .is_none_or(|state| state == FAULT_STATE_OPEN)
}

/// Whether one event is the resync notice rather than news.
///
/// A front end reads this to decide that what it holds is incomplete and the
/// snapshot has to be re-read; [`update`] reads it to mark [`View::stale`].
pub fn is_resync_lag(event: &Event) -> bool {
    matches!(event, Event::Fault(fault) if fault.kind == RESYNC_LAG_KIND)
}

/// Build a view from one snapshot. Pure, total, and free of I/O.
pub fn snapshot_to_view(snapshot: &Snapshot) -> View {
    let sessions: BTreeMap<String, SessionView> = snapshot
        .sessions
        .iter()
        .map(|row| {
            let session = SessionView::from_row(row);
            (session.session_id.clone(), session)
        })
        .collect();
    let deliveries: BTreeMap<String, DeliveryView> = snapshot
        .ledger
        .iter()
        .map(|entry| {
            let delivery = DeliveryView::from_entry(entry);
            (delivery.msg_id.clone(), delivery)
        })
        .collect();
    let faults: BTreeMap<i64, FaultEvent> = snapshot
        .faults
        .iter()
        .map(|fault| (fault.id, fault.clone()))
        .collect();
    let roles: BTreeMap<String, RoleInfo> = snapshot
        .roles
        .iter()
        .map(|role| (role.name.clone(), role.clone()))
        .collect();
    View {
        cluster: snapshot
            .status
            .as_ref()
            .map(ClusterSummary::from_status)
            .unwrap_or_default(),
        roles,
        sessions,
        deliveries,
        faults,
        // A fresh read is the whole truth it was read from: the tail is the
        // stream's, and a front end that re-reads re-subscribes from its own
        // cursor.
        event_tail: Vec::new(),
        stale: false,
    }
}

/// Fold one event off the observation stream into the view. Pure and total.
///
/// Every class this build knows has an arm; the arms that move nothing say why,
/// and the classes that are news without a state change still reach
/// [`View::event_tail`].
pub fn update(mut view: View, event: &Event) -> View {
    let news = match event {
        // The `roles` read owns the registry row, and this event is the live
        // half of it: presence, the session count, the aggregate, and the
        // client's detail word. A role the snapshot does not carry is not
        // re-invented here — a registry row minted from a presence event would
        // declare a role no spec ever named.
        Event::RolePresence(reported) => {
            if let Some(role) = view.roles.get_mut(&reported.role) {
                role.state = reported.state;
                role.sessions = reported.sessions;
                role.aggregate = reported.aggregate.clone();
                role.detail = reported.detail.clone();
            }
            true
        }
        Event::SessionState(reported) => {
            match view.sessions.get_mut(&reported.session_id) {
                // `(generation, seq)` is the row's version watermark, and the
                // server gates its own writes with the same pair: a row at or
                // below the one this view holds is a replay of a write it
                // already has, so it is neither a state change nor news.
                Some(stored)
                    if (reported.generation, reported.seq) <= (stored.generation, stored.seq) =>
                {
                    false
                }
                Some(stored) => {
                    stored.absorb(reported);
                    true
                }
                None => {
                    let session = SessionView::from_event(reported);
                    view.sessions.insert(session.session_id.clone(), session);
                    true
                }
            }
        }
        Event::LedgerState(reported) => {
            match view.deliveries.get_mut(&reported.msg_id) {
                Some(stored) => stored.absorb(reported),
                None => {
                    let delivery = DeliveryView::from_event(reported);
                    view.deliveries.insert(delivery.msg_id.clone(), delivery);
                }
            }
            true
        }
        // A gap is an input, not news: the events it stands for never arrive,
        // so folding the notice as an ordinary fault would draw it beside a
        // state that silently lost them.
        Event::Fault(reported) if reported.kind == RESYNC_LAG_KIND => {
            view.stale = true;
            false
        }
        Event::Fault(reported) => {
            view.faults.insert(reported.id, reported.clone());
            true
        }
        // A gateway's health is the `status` read's gateway rows, and no screen
        // of the three shows it: news, and no state here.
        Event::GatewayPresence { .. } => true,
        // A reload replaces the spec, and this event carries the counts and the
        // hash but no role list — a view cannot be updated into a spec it
        // cannot read. The front end re-reads the snapshot for the registry.
        Event::SpecReloaded(_) => true,
        // The turn-end family is the client's own account of a delivery it
        // settled. What it settles lands on the two rows that own it: the
        // delivery reaches `acked` on a `ledger_state` event, and the verdict
        // reaches the session row's outcome on a `session_state` event, where
        // `blocked` is the waiting a board shows. The class itself is news.
        Event::DeliveryBlocked(_) | Event::TurnEndWithoutComplete(_) | Event::Handoff(_) => true,
    };
    if news {
        view.event_tail.insert(0, event.clone());
        view.event_tail.truncate(EVENT_TAIL_LIMIT);
    }
    view
}

/// Fold one item off the stream by its wire class word and payload.
///
/// This is the boundary a front end's IO task sits on: the wire carries a class
/// word and a payload, and this build's [`Event`] is a closed set over them. A
/// class this build knows is decoded and folded by [`update`]; a class it does
/// not know — one a newer server publishes — leaves the view exactly as it was,
/// so an unrecognised class is never silently rendered as news.
pub fn update_class(view: View, class: &str, payload: Value) -> View {
    let encoded = serde_json::json!({ "type": class, "data": payload });
    match serde_json::from_value::<Event>(encoded) {
        Ok(event) => update(view, &event),
        Err(_) => view,
    }
}

/// The role a principal addresses, for the board a delivery belongs to.
fn principal_role(principal: &Principal) -> Option<&str> {
    match principal {
        Principal::Role { role, .. } => Some(role.as_str()),
        Principal::Gateway { .. } | Principal::Cluster { .. } => None,
    }
}

#[cfg(test)]
mod tests;
