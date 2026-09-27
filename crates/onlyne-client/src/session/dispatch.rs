// The parts below open with `use super::*`, so this module keeps the imports the
// single-file version shared with them, and re-exports exactly the names it
// exported publicly before the split. No logic lives here.
use crate::backend::{SessionBackend, SessionRef, SpawnSpec};
use crate::reconcile::{
    Bridge, apply_persist, feed_agent_gone, feed_created, feed_dispatched, feed_intent_receipt,
    feed_ready, feed_resource_closed, feed_resumed, feed_suspended, settle, stored_observation,
};
use crate::runtime::intent::stamp_op_id;
use crate::runtime::runloop::ClientInit;
use crate::session::handoff;
use anyhow::{Context, Result, anyhow};
use onlyne_adapter::AdapterIo;
use onlyne_config::layout::RoleWorkspace;
use onlyne_net::conn::{ClientConn, ConnReadiness, dial};
use onlyne_net::{ConnSettings, KeyPair, NetError};
use onlyne_proto::{
    AckArgs, AdapterMsg, AgentPhase, AssignArgs, Body, Capability, Causality, ClientOp, ControlOp,
    DeliveryPhase, Envelope, Frame, HandshakeArgs, HostOp, Lifecycle, LiveSession, MsgKind,
    Outcome, PROTOCOL_VERSION, Principal, RecoveryPhase, RecycleArgs, Report, ResBody,
    ResourcePhase, SessionProjection, Welcome, new_envelope,
};
use onlyne_proto::{
    IgnoredReason, LifecycleEvent, Observation, TaskState, Verdict, Version, project,
};
use onlyne_store::ClientStore;
use onlyne_store::session::{SessionLedger, SessionRecord};
use parking_lot::Mutex;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

mod delivery;
mod env;
mod guards;
mod idle;
mod outbound;
mod projection;
mod reports;
mod retire;
mod scope;
mod settle;
mod slots;
mod state;
mod transport;
mod turn_end;

pub use delivery::{ReadyNotice, dispatch, on_ready};
pub use env::{
    HEARTBEAT_INTERVAL, HEARTBEAT_SILENCE_MARGIN, REQUEST_TIMEOUT, missing_capability, plugin_gap,
};
// The hello stamper is crate-internal and `claim`'s test module reaches it only
// through this path, so the re-export exists for that module alone: ungated, it
// is an unused import in every non-test build of the crate.
#[cfg(test)]
pub(crate) use outbound::hello_with_live_sessions;
// 3c's one sentence lives with the transport that speaks it, and the ACP
// backend's tests read it here to assert a nudge reaches the agent verbatim.
pub use outbound::{ClientLink, Outbox, send_frame};
pub use projection::{
    note_intent_receipt, note_verdict, projection_of, sync_frame, sync_session, task_outcome_of,
    task_state_of, with_cluster,
};
pub use reports::{on_control, on_plugin_report};
pub use retire::{SESSION_DEAD, close_all, on_recycled};
pub use settle::{SETTLE_WITHOUT_TURN, SettleAuthority, on_out};
pub use state::{
    CONTROL_SETTLE_BOUND, ControlNote, ControlWord, DispatchState, FrameGuard, SessionSlot,
};
#[cfg(test)]
pub(crate) use transport::NUDGE_TEXT;
pub use turn_end::on_turn_end;
