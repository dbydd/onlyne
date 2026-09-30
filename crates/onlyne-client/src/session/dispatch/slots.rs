use super::*;

use super::outbound::store_ack;
use super::projection::{stored_task_state, task_state_of};
use super::retire::stored_close_reason;
use super::state::{
    ControlNote, ControlWord, DispatchInner, DispatchState, ToolsScope, ToolsSession,
    due_control_settles, live_sessions, session_exited, slot_key_for_token, slot_key_named,
    slot_key_serving_task, token_names_session, tools_session_of,
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
                drive: None,
                placement: None,
                runtime_refusal: None,
                max_sessions,
                session_policy: onlyne_config::SessionPolicy::default(),
                required_targets: Vec::new(),
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
                parked: Vec::new(),
                standing: Vec::new(),
                stall: crate::session::stall::StallWatch::new(),
                revived: Vec::new(),
                control_settles: Vec::new(),
                tools_mounts: Vec::new(),
                in_frame: Vec::new(),
                turn_end: super::turn_end::TurnEndWatch::default(),
            })),
        }
    }

    /// Adopt the role's `[client.session]` policy.
    pub fn with_session_policy(self, policy: onlyne_config::SessionPolicy) -> Self {
        self.inner.lock().session_policy = policy;
        self
    }

    /// Install the placement this machine resolved. It is the other half of the
    /// drive rule, so it is recorded even when the pair it makes is refused.
    pub fn with_placement(self, placement: crate::backend::SessionPlacement) -> Self {
        self.inner.lock().placement = Some(placement);
        self
    }

    /// The drive the role's spec declares, `None` before the first `welcome`.
    pub fn drive(&self) -> Option<onlyne_config::Drive> {
        self.inner.lock().drive
    }

    /// The placement this machine resolved, as the registration spells it.
    pub fn placement_name(&self) -> Option<&'static str> {
        self.inner
            .lock()
            .placement
            .map(crate::backend::SessionPlacement::as_str)
    }

    /// The name of the session backend currently installed.
    pub fn session_backend(&self) -> &'static str {
        self.inner.lock().backend.name()
    }

    /// The role workspace this client serves.
    pub fn workspace(&self) -> PathBuf {
        self.inner.lock().workspace.clone()
    }

    /// Record the drive the role's spec declares, and the sentence a delivery
    /// meets when that drive cannot run under this machine's placement.
    ///
    /// The drive lands whether or not a backend could be installed for it: a
    /// pair this machine cannot host still has to be *known*, or the next
    /// delivery would run under the backend the previous drive left behind.
    pub fn set_drive(&self, drive: onlyne_config::Drive, refusal: Option<String>) {
        let mut inner = self.inner.lock();
        inner.drive = Some(drive);
        inner.runtime_refusal = refusal;
    }

    /// Install the backend a role's drive selects on this machine.
    ///
    /// Answers whether it landed. A drive cannot move while this role holds live
    /// sessions: their panes, tabs, and children are the installed backend's to
    /// close and to probe, and a backend that never opened a resource cannot
    /// answer for it. So the move waits — the caller keeps the old drive
    /// recorded, and the next `welcome` or spec reload tries again once the
    /// sessions are gone.
    pub fn set_backend(&self, backend: Arc<dyn SessionBackend>) -> bool {
        let mut inner = self.inner.lock();
        if inner.backend.name() != backend.name() && !inner.sessions.is_empty() {
            return false;
        }
        if inner.backend.name() != backend.name() {
            tracing::info!(
                previous = inner.backend.name(),
                selected = backend.name(),
                "the session backend moved with the role's drive"
            );
        }
        inner.backend = backend;
        true
    }

    /// Whether this client holds a session this name answers to.
    ///
    /// A mount names the session it was spawned for, and a plugin that outlived
    /// a restart still names the one it was serving when it redials — to a client
    /// that has no memory of it, because nothing here rebuilds slots from the
    /// store. So this is a question with a real "no", and the caller needs to be
    /// able to ask it before it binds anything under that name.
    pub fn knows_session(&self, session_id: &str) -> bool {
        super::state::slot_key_named(&self.inner.lock(), session_id).is_some()
    }

    /// The role's `[client.session]` policy.
    pub fn session_policy(&self) -> onlyne_config::SessionPolicy {
        self.inner.lock().session_policy.clone()
    }

    pub fn session_count(&self) -> usize {
        self.inner.lock().sessions.len()
    }

    /// Feed the turn-started fact for a self-driven session's turn, so the
    /// row's agent phase reads `running` before the agent can report a
    /// completion through its tools mount (`docs/v2-CONTRACT.md` §3c). A plugin
    /// drive feeds this through its heartbeats; a self-driven drive has none,
    /// so the dispatch path feeds it where it hands the turn to the backend.
    pub fn feed_turn_started(&self, task_id: &str) {
        let inner = self.inner.lock();
        if let Err(error) =
            crate::reconcile::feed_turn_started(&inner.bridge, &inner.store, task_id)
        {
            tracing::warn!(task = %task_id, error = %error, "the turn-start feed did not apply");
        }
    }

    /// Feed the turn-ended fact for a self-driven session's turn, so the row's
    /// agent phase reads `idle` when the turn the drive witnessed ends. The
    /// never-ran guard reads the same column, so a completion that arrives
    /// after this feed still passes it.
    pub fn feed_turn_ended(&self, task_id: &str) {
        let inner = self.inner.lock();
        if let Err(error) = crate::reconcile::feed_turn_ended(&inner.bridge, &inner.store, task_id)
        {
            tracing::warn!(task = %task_id, error = %error, "the turn-end feed did not apply");
        }
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
    pub fn outcome_feed(&self) -> Option<crate::backend::OutcomeFeed> {
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
    ///
    /// `word` is what the operator said and `now` is when they said it, and both
    /// are the caller's to name rather than this call's to invent: the command is
    /// the authority on the ending it asked for, and the instant it reads is the
    /// one the watchdog's bound runs from.
    pub fn owe_controlled_settle(&self, task_id: &str, word: ControlWord, now: Instant) {
        let mut inner = self.inner.lock();
        if !inner
            .control_settles
            .iter()
            .any(|owed| owed.task_id == task_id)
        {
            inner.control_settles.push(ControlNote {
                task_id: task_id.to_string(),
                noted_at: now,
                word,
            });
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
            .position(|owed| owed.task_id == task_id)
        else {
            return false;
        };
        inner.control_settles.swap_remove(at);
        true
    }

    /// The notes whose operator's word has gone unanswered past the bound.
    ///
    /// The reading a sweep takes before it acts, and it spends nothing: the
    /// settle below goes through [`take_controlled_settle`], so a completion that
    /// answers a word between this read and that call takes the note first.
    ///
    /// [`take_controlled_settle`]: DispatchState::take_controlled_settle
    pub fn control_settles_due(&self, now: Instant) -> Vec<ControlNote> {
        due_control_settles(&self.inner.lock(), now)
    }

    /// Settle the work one operator's word left open, with no report behind it.
    ///
    /// The word asks a plugin for its own ending and the completion that answers
    /// it is a frame of the plugin's, so a plugin that never sends one — it left
    /// with the command's frame, or implements no `recycle` at all — leaves the
    /// task open and the delivery row this client was handed in flight, with no
    /// later caller to answer either. This is that caller.
    ///
    /// The writes are the ones `retire_dropped_ghosts` makes for the task its
    /// owed session left: the verdict through the task's own record, which
    /// refuses to overwrite one that landed first, and the still-held delivery row
    /// refused with the operator's word, which is terminal for that row the way
    /// every refusal is. The publish is the caller's, because it cannot run under
    /// this lock.
    ///
    /// Answers `false` when this call is not the settle: the note is already spent
    /// by a completion that answered the word, or the task's record carries a
    /// verdict already, and either way nothing here is written and nothing is for
    /// the caller to publish.
    ///
    /// [`retire_dropped_ghosts`]: DispatchState::retire_dropped_ghosts
    pub fn settle_unanswered_control(&self, note: &ControlNote) -> bool {
        // The note is taken first and at once: a report that answers the word
        // while this call waits for the lock spends it, and the task then needs no
        // verdict from here.
        if !self.take_controlled_settle(&note.task_id) {
            return false;
        }
        let mut inner = self.inner.lock();
        // The verdict goes through the task's own record, which keeps the first
        // one it was handed: a row an earlier settle answered stays as that settle
        // left it. A verdict that was not this call's is a settle that already
        // happened — every door that writes one answers the delivery row in the
        // same breath — so there is nothing left here to refuse or to publish.
        let verdict = task_state_of(note.word.outcome());
        match inner.store.settle_task(&note.task_id, verdict) {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(
                    task = %note.task_id,
                    ?verdict,
                    "an unanswered control command's task was already settled; the first verdict stands"
                );
                return false;
            }
            Err(error) => {
                // A store that refused the write must not cost the word its
                // answer: the note goes back where it came from — through the door
                // that records one, stamped where it was — and the next tick tries
                // again rather than leaving the task open forever.
                tracing::warn!(
                    task = %note.task_id,
                    error = %error,
                    "the task of an unanswered control command was not settled; the word stays owed"
                );
                drop(inner);
                self.owe_controlled_settle(&note.task_id, note.word, note.noted_at);
                return false;
            }
        }
        // The delivery row this client is still holding is refused, and the
        // reason is the operator's own word: the row is answered once, by whoever
        // still holds its handle, and a plugin's report arriving later finds no
        // handle left to spend.
        let held = slot_key_serving_task(&inner, &note.task_id)
            .and_then(|key| inner.sessions.get_mut(&key))
            .and_then(|slot| slot.msg_id.take());
        if let Some(msg_id) = held {
            store_ack(
                &inner,
                AckArgs {
                    msg_id,
                    op_id: None,
                    accepted: false,
                    reason: Some(note.word.refusal().to_string()),
                },
            );
        }
        true
    }

    /// The role slice the dispatcher currently runs.
    pub fn role_slice(&self) -> crate::session::slice::RoleSlice {
        let inner = self.inner.lock();
        crate::session::slice::RoleSlice {
            drive: inner.drive.unwrap_or_default(),
            command: inner.command.clone(),
            max_sessions: inner.max_sessions,
            required_targets: inner.required_targets.clone(),
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

    /// The sorted live sessions one hello claims: the union of the memory
    /// slots and the DB-persisted active sessions, so a process crash does not
    /// lose the claim.
    ///
    /// A store failure is not swallowed: it is logged with the memory claim
    /// still derivable from the slots, and returned so the caller can degrade
    /// to [`live_claim_from_slots`] deliberately instead of answering as if the
    /// durable half were simply empty. Silently dropping that half lets the
    /// server requeue every in_flight row a crash left behind, which is the
    /// duplicate delivery this claim exists to prevent.
    pub fn hello_live_sessions(&self) -> onlyne_store::StoreResult<Vec<LiveSession>> {
        let held = self.live_claim_from_slots();
        // Merge DB-persisted sessions that are not yet exited. A fresh process
        // after crash has empty slots but the DB still holds the sessions it was
        // serving, so the hello must claim them to prevent the server from
        // requeuing work this process is still running.
        let persisted = {
            let inner = self.inner.lock();
            inner.store.active_sessions()
        };
        match persisted {
            Ok(persisted) => {
                let sessions = held.into_iter().chain(persisted);
                Ok(crate::session::claim::from_sessions(sessions))
            }
            Err(error) => {
                tracing::error!(
                    error = %error,
                    memory_claim = ?held,
                    "hello claim lost its durable half: active_sessions failed; the DB-persisted sessions are missing and the server may requeue them"
                );
                Err(error)
            }
        }
    }

    /// The memory half of [`hello_live_sessions`]: the claim to dial with when
    /// the durable store cannot answer. The slots are what this process is
    /// serving right now, so even a degraded hello keeps those rows in_flight.
    ///
    /// A slot answers with the session's own id — the one that stays put while a
    /// scope hands the session delivery after delivery — the delivery it is
    /// serving now, and whether its process has been released. A session between
    /// deliveries claims no delivery: the server keeps the rows those sessions
    /// are still the owners of, and invents none.
    pub fn live_claim_from_slots(&self) -> Vec<LiveSession> {
        let inner = self.inner.lock();
        crate::session::claim::from_sessions(inner.sessions.values().map(|slot| LiveSession {
            session_id: slot.session.task_id.clone(),
            task_id: slot.task_id.clone(),
            suspended: slot.suspended,
        }))
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
        !inner.transports.is_empty() || !inner.parked.is_empty()
    }

    /// Adopt a role slice: the one `welcome` carried, or the one a reload's
    /// role row carries.
    pub fn reconfigure(&self, slice: crate::session::slice::RoleSlice) {
        let mut inner = self.inner.lock();
        inner.command = slice.command;
        inner.max_sessions = slice.max_sessions;
        inner.required_targets = slice.required_targets;
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
    ///
    /// A scope that hands a delivery on to a session the role already holds
    /// spends no slot at all, so an `task` or `role` session sitting idle — or
    /// suspended, its slot already given back — is room even at the cap. The
    /// placement decides which delivery goes where; this only answers whether
    /// there is somewhere for one to go. `oneshot` reuses nothing, which is the
    /// count it has always been.
    pub fn has_capacity(&self) -> bool {
        let inner = self.inner.lock();
        if !matches!(
            inner.session_policy.scope,
            onlyne_config::SessionScope::Oneshot
        ) && inner.sessions.values().any(super::scope::takes_new_work)
        {
            return true;
        }
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
            Some(crate::backend::CloseReason::Completed)
        )
    }

    /// One staged session this role's standing runtime can be offered.
    ///
    /// The mirror of [`Self::staged_without_transport`], narrowed to a session
    /// that has no transport *because* a hosting runtime will supply one. A
    /// session another connection already serves is not offered, and a session
    /// whose work is already in flight is not either — a runtime must never be
    /// asked to open a second conversation for a chain that has one.
    pub fn staged_hosting_session(&self) -> Option<String> {
        let inner = self.inner.lock();
        inner
            .sessions
            .iter()
            .find(|(key, slot)| {
                slot.payload.is_some()
                    && slot.session.backend == "hosting"
                    && !inner
                        .transports
                        .keys()
                        .any(|session| super::transport::names_session(key, slot, session))
            })
            .map(|(key, _)| key.clone())
    }

    /// What to ask a hosting runtime for, by the name this client gave the
    /// session.
    ///
    /// The resume handle of the family's previous session rides along when there
    /// is one, so a `task`-scoped runtime hands back the conversation it already
    /// holds rather than opening a second one for the same chain. Nothing reads
    /// the handle here: it is the runtime's own word for where its conversation
    /// is, and this client stores it and gives it back.
    pub fn hosting_open_args(&self, session_id: &str) -> onlyne_proto::OpenArgs {
        let inner = self.inner.lock();
        let slot = inner.sessions.get(session_id);
        let (task_id, family, prose) = match slot {
            Some(slot) => (slot.task_id.clone(), slot.family.clone(), String::new()),
            None => (Some(session_id.to_string()), None, String::new()),
        };
        let handle = inner
            .sessions
            .iter()
            .filter(|(key, other)| other.family == family && *key != session_id)
            .filter_map(|(_, other)| other.resume_handle.clone())
            .next();
        onlyne_proto::OpenArgs {
            session_id: session_id.to_string(),
            task_id: task_id.unwrap_or_else(|| session_id.to_string()),
            scope: format!("{:?}", inner.session_policy.scope).to_lowercase(),
            family,
            prose,
            resume_handle: handle,
        }
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

    /// The tools token of one live session, when this client holds it.
    ///
    /// The one door the token leaves this process through: the session's own
    /// drive reads it while building the child that mounts `onlyne mcp`
    /// (`SpawnSpec.tools_token`), and nothing else may hand it out
    /// (`docs/v2-CONTRACT.md` §3b).
    pub fn tools_token(&self, session_id: &str) -> Option<String> {
        let inner = self.inner.lock();
        let key = slot_key_named(&inner, session_id)?;
        inner
            .sessions
            .get(&key)
            .filter(|slot| !session_exited(&inner, &slot.session.task_id))
            .map(|slot| slot.tools_token.clone())
    }

    /// The session a `tools` mount token speaks for, when it names a live one.
    pub fn tools_mount_for(&self, token: &str) -> Option<ToolsSession> {
        let inner = self.inner.lock();
        let key = slot_key_for_token(&inner, token)?;
        tools_session_of(&inner, &key)
    }

    /// Bind one `tools` connection to the session its token names.
    ///
    /// The hello validated the token; this writes the binding the session's
    /// later frames are measured against, and answers the session the
    /// connection now speaks for. A second connection presenting the same token
    /// takes the binding over, and the earlier one then fails the per-frame
    /// liveness check — a token belongs to one session, and the newest
    /// connection is the one that speaks for it. `None` is a token whose
    /// session retired between the handshake and this line.
    pub fn bind_tools_mount(&self, token: &str, io: AdapterIo) -> Option<ToolsSession> {
        let mut inner = self.inner.lock();
        let key = slot_key_for_token(&inner, token)?;
        let session = tools_session_of(&inner, &key)?;
        inner.tools_mounts.retain(|(held, _)| held != &key);
        inner.tools_mounts.push((key, io));
        Some(session)
    }

    /// Whether one live tools connection still speaks for a session this client
    /// holds.
    ///
    /// A token dies with its session, and a mount whose session has ended is
    /// refused from then on: this is the per-frame half of that rule, so a
    /// connection left open past its session's retirement answers `unauthorized`
    /// and closes rather than speaking for work nobody holds.
    pub fn tools_connection_live(&self, io: &AdapterIo) -> bool {
        self.tools_scope(io).is_some()
    }

    /// What one live tools connection speaks for, read under the lock that owns
    /// it; `None` when the connection is unbound or its session has stopped
    /// serving.
    ///
    /// The token is the binding (`docs/v2-CONTRACT.md` §3b), so everything a
    /// tools frame is stamped with comes from the session's own slot: the open
    /// delivery its `report` and `handoff` frames name, and the session a `send`
    /// leaves from. Reading it in one locked pass is what keeps a frame from
    /// being stamped from a state that moved between the lookup and the stamp.
    pub fn tools_scope(&self, io: &AdapterIo) -> Option<ToolsScope> {
        let inner = self.inner.lock();
        let (key, _) = inner
            .tools_mounts
            .iter()
            .find(|(_, bound)| bound.same_connection(io))?;
        inner
            .sessions
            .get(key)
            .filter(|slot| token_names_session(&inner, slot))
            .map(|slot| ToolsScope {
                session_id: slot.session.task_id.clone(),
                task_id: slot.task_id.clone(),
            })
    }

    /// Drop the binding one tools connection held, whichever session it named.
    pub fn release_tools_connection(&self, io: &AdapterIo) {
        self.inner
            .lock()
            .tools_mounts
            .retain(|(_, bound)| !bound.same_connection(io));
    }
}
