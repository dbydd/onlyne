// The parts below open with `use super::*`, so this module keeps the imports the
// single-file version shared with them, and re-exports exactly the names it
// exported publicly before the split. No logic lives here.
use crate::runtime::intent::stamp_op_id;
use crate::runtime::runloop::ClientInit;
use crate::session::handoff::{self, Denial};
use anyhow::{Context, Result, anyhow};
use onlyne_adapter::AdapterIo;
use onlyne_layout::RoleWorkspace;
use onlyne_net::conn::{ClientConn, ConnReadiness, dial};
use onlyne_net::{ConnSettings, KeyPair, NetError};
use onlyne_proto::{
    AckArgs, AdapterMsg, AgentPhase, AssignArgs, Body, Capability, Causality, ClientOp, ControlOp,
    DeliveryPhase, Envelope, Frame, Handoff, HandshakeArgs, HostOp, Lifecycle, MsgKind, Outcome,
    PROTOCOL_VERSION, Principal, RecoveryPhase, RecycleArgs, Report, ResBody, ResourcePhase,
    SessionProjection, Welcome, new_envelope,
};
use onlyne_session::{
    AgentState, Bridge, DeliveryState, IgnoredReason, LifecycleEvent, Observation, PublicLifecycle,
    RecoveryState, ResourceState, SessionBackend, SessionLedger, SessionRecord, SessionRef,
    SpawnSpec, TaskState, Verdict, Version, apply_persist, feed_agent_gone, feed_created,
    feed_dispatched, feed_intent_receipt, feed_ready, feed_resource_closed, project, settle,
    stored_observation,
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

mod delivery;
mod env;
mod outbound;
mod projection;
mod reports;
mod retire;
mod settle;
mod slots;
mod state;
mod transport;

pub use delivery::{ReadyNotice, dispatch, on_ready};
pub use env::{
    HEARTBEAT_INTERVAL, HEARTBEAT_SILENCE_MARGIN, REQUEST_TIMEOUT, missing_capability, plugin_gap,
};
// The hello stamper is crate-internal and `claim`'s test module reaches it only
// through this path, so the re-export exists for that module alone: ungated, it
// is an unused import in every non-test build of the crate.
#[cfg(test)]
pub(crate) use outbound::hello_with_live_tasks;
pub use outbound::{ClientLink, Outbox, send_frame};
pub use projection::{
    note_intent_receipt, note_verdict, projection_of, sync_session, task_outcome_of, task_state_of,
    with_cluster,
};
pub use reports::{on_control, on_plugin_report};
pub use retire::{close_all, on_recycled};
pub use settle::on_out;
pub use state::{DispatchState, FrameGuard, SessionSlot};
