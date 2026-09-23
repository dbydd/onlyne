use super::*;

use super::outbound::store_ack;
use super::projection::stored_task_state;
use super::retire::stored_close_reason;
use super::state::{
    DispatchInner, DispatchState, live_sessions, session_exited, slot_key_serving_task,
};
use super::transport::names_session;

impl DispatchState {
    pub fn new(
        role: impl Into<String>,
        workspace: impl Into<PathBuf>,
        command: Vec<String>,
        max_sessions: u32,
        backend: Arc<dyn SessionBackend>,
        store: ClientStore,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(DispatchInner {
                role: role.into(),
                workspace: workspace.into(),
                command,
                max_sessions,
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
                stall: crate::session::stall::StallWatch::new(),
                revived: Vec::new(),
                held_handoffs: HashMap::new(),
                control_settles: Vec::new(),
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

    /// Note that one task's plugin has been told by this client's own `control`
    /// command to report the ending of that task.
    ///
    /// `on_control` runs this before the `recycle` frame leaves, which is the
    /// point where the client still knows the order: the plugin's completion and
    /// the retirement that command triggers race over the session's row, and a
    /// guard that read the row would answer the same operator action two ways. One
    /// note per task is kept, so a command issued twice waits for one answer.
    pub fn owe_controlled_settle(&self, task_id: &str) {
        let mut inner = self.inner.lock();
        if !inner.control_settles.iter().any(|owed| owed == task_id) {
            inner.control_settles.push(task_id.to_string());
        }
    }

    /// Whether one completion answers a command noted above, consuming the note.
    ///
    /// The note is spent whichever way the settle it authorises goes: a refused
    /// verdict leaves no second answer owed, and an applied one has travelled the
    /// command's own completion. A later report for the same task is the plugin
    /// speaking for itself again, and reads the ordinary door.
    pub fn take_controlled_settle(&self, task_id: &str) -> bool {
        let mut inner = self.inner.lock();
        let Some(at) = inner
            .control_settles
            .iter()
            .position(|owed| owed == task_id)
        else {
            return false;
        };
        inner.control_settles.swap_remove(at);
        true
    }

    /// The role slice the dispatcher currently runs.
    pub fn role_slice(&self) -> crate::session::slice::RoleSlice {
        let inner = self.inner.lock();
        crate::session::slice::RoleSlice {
            command: inner.command.clone(),
            max_sessions: inner.max_sessions,
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
        crate::session::claim::from_slots(self.live_task_ids())
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
    /// watermark. A tuple and verdict that derive `exited` retire their progress
    /// clock before the send boundary.
    pub fn stall_report(&self, task_id: &str) -> Option<Report> {
        let mut inner = self.inner.lock();
        let row = inner.store.get_session(task_id).ok().flatten();
        let exited = row.as_ref().is_some_and(|row| {
            projection_of(row, stored_task_state(&inner, task_id)).lifecycle == Lifecycle::Exited
        });
        if exited {
            inner.stall.forget(task_id);
            return None;
        }
        Some(crate::session::stall::report(
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
    pub fn reconfigure(&self, slice: crate::session::slice::RoleSlice) {
        let mut inner = self.inner.lock();
        inner.command = slice.command;
        inner.max_sessions = slice.max_sessions;
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
    /// offers it again once a session frees. Each task runs in its own session,
    /// so a slot whose task has finished still spends capacity until it retires.
    pub fn has_capacity(&self) -> bool {
        let inner = self.inner.lock();
        live_sessions(&inner) < inner.max_sessions as usize
    }

    /// Whether this role already finished one task with a terminal `Done`.
    ///
    /// The task's own record is the account: the settle writes the verdict the
    /// agent filed and refuses to overwrite it, so a record reading `done` means
    /// this role answered for this task id once already. A redelivery of that
    /// task is not new work — running it again would stage its payload on
    /// whichever session happens to be idle, so one chain's task executes inside
    /// another conversation and the second answer collides with the verdict the
    /// first one settled.
    ///
    /// Only `done` counts. A session killed or crashed mid-flight leaves its task
    /// open, or settles it `failed`, and the server's requeue, `repair_retry`, and
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
    /// exactly this state, and the mount ends the wait. A session answers through
    /// the transport its first task claimed, so it stays served.
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
}
