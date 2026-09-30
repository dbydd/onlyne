use super::*;

use super::env::{missing_capability, reject_unpaired_runtime, served_socket, session_env};
use super::outbound::send_frame;
use super::projection::{note_verdict, sync_session};
use super::state::{
    DispatchInner, DispatchState, SessionSlot, live_sessions, mint_tools_token, note_beat,
    rebase_generation, render_tokens, slot_key_serving_task,
};
use super::transport::{
    is_revived_connection, names_session, note_binding_locked, record_revived_connection,
};
use crate::delivery::{from_label, render, write_attachment};

/// One delivery becomes the work of one session, or waits for the session its
/// scope sends it to.
///
/// The role's scope decides which session takes the delivery and nothing else
/// does: `oneshot` opens a session for every delivery, `task` hands the
/// delivery to the session that already holds its family's conversation, and
/// `role` hands it to whichever pooled session has waited longest. `Ok(None)`
/// is the third answer and it is not a refusal: the scope's session is busy, or
/// every slot is spent, and the row stays in flight for the pull that comes
/// after one frees (plan §5 `max_sessions`).
pub fn dispatch(state: &DispatchState, envelope: &Envelope) -> Result<Option<SessionRef>> {
    let causality = envelope
        .causality
        .clone()
        .context("task envelope missing causality.task")?;
    let task_id = causality.task.clone();
    let family = scope::family_of(&causality);
    // The role's prose, read from its one owner before the dispatch lock is
    // taken. A session this delivery opens needs it at spawn — a runtime with no
    // instruction channel of its own is handed it in the workspace — and the
    // assignment frame that follows carries the same value
    // (`docs/v2-CONTRACT.md` §3b).
    let prose = state.role_prose();
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
                slot.causality = causality.clone();
            }
            slot.session.clone()
        })
    {
        inner.stall.note_assigned(&task_id, Instant::now());
        return Ok(Some(session));
    }
    match scope::placement(&inner, &family) {
        scope::Placement::Bind(key) => {
            let session = bind_delivery(&mut inner, &key, envelope, &causality, &task_id)?;
            return Ok(Some(session));
        }
        scope::Placement::Resume(key) => {
            let session =
                resume_delivery(&mut inner, &key, envelope, &causality, &task_id, &prose)?;
            return Ok(Some(session));
        }
        scope::Placement::Wait => {
            tracing::debug!(
                task = %task_id,
                family = %family,
                "the delivery waits: the session its scope names is serving another delivery"
            );
            return Ok(None);
        }
        scope::Placement::Open => {}
    }
    // What is left opens a session, so this is where `max_sessions` bites.
    if live_sessions(&inner) >= inner.max_sessions as usize {
        tracing::debug!(
            task = %task_id,
            max_sessions = inner.max_sessions,
            "no session slot is free; the delivery waits"
        );
        return Ok(None);
    }
    let session = open_session(&mut inner, envelope, &causality, &task_id, &family, &prose)?;
    Ok(Some(session))
}

/// Open one session for a delivery: the `oneshot` path, and the first delivery
/// of a family or a role pool.
///
/// The session's own id is the delivery's task id, which is what makes the row
/// it is born onto this delivery's row: a session that goes on to serve the
/// family's later deliveries keeps this id and gains a binding for each of them.
fn open_session(
    inner: &mut DispatchInner,
    envelope: &Envelope,
    causality: &Causality,
    task_id: &str,
    family: &str,
    prose: &str,
) -> Result<SessionRef> {
    reject_unpaired_runtime(inner)?;
    let session_id = task_id.to_string();
    let command = render_tokens(&inner.command, &session_id, task_id);
    let env = session_env(
        &inner.role,
        &session_id,
        task_id,
        &inner.topology,
        // One tree answers both halves of this spawn: the cwd below and the
        // socket the plugin dials, so a session whose workspace resolves to a
        // short endpoint is handed the served path directly.
        &served_socket(&inner.workspace),
    );
    // The tools token is minted before the spawn, because the child that mounts
    // `onlyne mcp` is handed it there, and it is recorded on the slot below so
    // the session's own state is the only other place it lives: a capability
    // never reaches a log, a fault, or a ledger row (`docs/v2-CONTRACT.md` §3b).
    let tools_token = mint_tools_token();
    // A hosting runtime is already resident, so the host does not start a
    // process for this session — it asks. The slot is staged either way: the slot
    // is what the delivery, the session row and the scope all name. What differs
    // is whether a `SpawnSpec` went out first.
    //
    // The staged `SessionRef` is the host's own name for the session and carries
    // no command, because there is no argv to carry — the process belongs to the
    // runtime. The connection answers `open` with whatever *it* calls that
    // conversation, and `hosted_session_ready` puts the two together.
    let hosted = !inner.standing.is_empty();
    let session = if hosted {
        SessionRef {
            task_id: session_id.clone(),
            backend: "hosting".into(),
            backend_ref: serde_json::Value::Null,
            generation: 1,
        }
    } else {
        inner.backend.spawn(SpawnSpec {
            cwd: inner.workspace.clone(),
            task_id: session_id.clone(),
            command: command.clone(),
            env,
            tools_token: tools_token.clone(),
            prose: prose.to_string(),
            focus: None,
            placement: None,
            rename: None,
        })?
    };
    let family = scope::keys_on_family(inner.session_policy.scope).then(|| family.to_string());
    // Everything that can still refuse this delivery runs inside `stage_slot`,
    // and a refusal past this line leaves a live pane, tab, or child process
    // behind. Nothing in `sessions` names it, so `close_all`, both reconnect
    // sweeps, and `live_sessions` never see it again: the resource and the
    // bridge's record of it leak for the life of the client, which is the shape
    // a SQLite error in `open_task` or `feed_created` used to leave. The slot is
    // given back here, and the close runs after the lock is off it.
    if let Err(error) = stage_slot(
        inner,
        Spawned {
            session: session.clone(),
            command,
            tools_token,
        },
        task_id,
        family,
        causality,
        envelope,
    ) {
        // A hosted session never opened a resource, so there is none to hand
        // back: the runtime owns the process and the `open` that fails undoes
        // itself on that side.
        let backend = Arc::clone(&inner.backend);
        if !hosted
            && let Err(close_error) =
                backend.close(&session, crate::backend::CloseReason::Fault, false)
        {
            tracing::warn!(
                task = %task_id,
                backend = %session.backend,
                resource = %session.backend_ref,
                error = %close_error,
                "the resource of a dispatch that did not land was not given back"
            );
        }
        return Err(error);
    }
    inner.stall.note_assigned(task_id, Instant::now());
    Ok(session)
}

/// Hand one delivery to a session this client already holds.
///
/// The session's own dimensions do not move: a new delivery changes which
/// delivery it serves, not what it is. So the binding is taken on its own —
/// `open_task` writes the delivery's record, the binding is opened in this
/// client's store, and the session's row advances one sequence so the publish
/// that follows carries the new binding to the mirror. Without that step the
/// server's gate would take nothing at the watermark it already holds, and the
/// row an operator reads would keep naming the delivery this session has
/// finished.
/// The scope word an assignment carries: the config's own spelling, so the
/// runtime compares against what the operator wrote rather than a second
/// vocabulary of this crate's own.
fn scope_word(scope: &onlyne_config::SessionScope) -> String {
    scope.as_str().to_string()
}

fn bind_delivery(
    inner: &mut DispatchInner,
    key: &str,
    envelope: &Envelope,
    causality: &Causality,
    task_id: &str,
) -> Result<SessionRef> {
    let session = inner
        .sessions
        .get(key)
        .map(|slot| slot.session.clone())
        .ok_or_else(|| anyhow!("no session answers to {key}"))?;
    let session_id = session.task_id.clone();
    inner.store.open_task(causality, task_cause(causality))?;
    advance_session_row(inner, &session_id);
    inner.store.bind_task(&session_id, task_id)?;
    if let Some(slot) = inner.sessions.get_mut(key) {
        slot.task_id = Some(task_id.to_string());
        slot.payload = Some(envelope.clone());
        slot.causality = causality.clone();
        slot.origin = Some(envelope.from.clone());
        slot.msg_id = None;
        slot.read_only = false;
        slot.idle_since = None;
    }
    tracing::info!(
        session = %session_id,
        task = %task_id,
        "the delivery joined the session its scope keeps for it"
    );
    inner.stall.note_assigned(task_id, Instant::now());
    Ok(session)
}

/// Resume one suspended session for a delivery, and bind it.
///
/// A suspended session's conversation lives in the runtime's own store, and the
/// client's half of resuming it is to start the command the session was born
/// with again: the same argv — because the command carries the runtime's own
/// session key, and it may interpolate the delivery into it — and the
/// environment rebuilt for the delivery being served. The runtime resumes the
/// conversation it was asked for. This client never composes a history summary
/// to hand a model: a summary it wrote would be context pollution it also
/// invented (plan §10).
fn resume_delivery(
    inner: &mut DispatchInner,
    key: &str,
    envelope: &Envelope,
    causality: &Causality,
    task_id: &str,
    prose: &str,
) -> Result<SessionRef> {
    reject_unpaired_runtime(inner)?;
    let (session_id, command, tools_token) = inner
        .sessions
        .get(key)
        .map(|slot| {
            (
                slot.session.task_id.clone(),
                slot.command.clone(),
                slot.tools_token.clone(),
            )
        })
        .ok_or_else(|| anyhow!("no session answers to {key}"))?;
    let env = session_env(
        &inner.role,
        &session_id,
        task_id,
        &inner.topology,
        &served_socket(&inner.workspace),
    );
    // The resumed process runs the same session, so it is handed the same tools
    // token: the token belongs to the session and lives in its slot, and this
    // client's binding is what the mount's first call is measured against. A
    // session that *reopens* — a new slot for the same task — mints a new one.
    let resumed = inner.backend.spawn(SpawnSpec {
        cwd: inner.workspace.clone(),
        task_id: session_id.clone(),
        command,
        env,
        tools_token,
        prose: prose.to_string(),
        focus: None,
        placement: None,
        rename: None,
    })?;
    inner.bridge.track_live(resumed.clone());
    // The row moves next, and the store's own binding write is handed back once
    // it is final: `Resume` attaches the resource again while the generation
    // stays live, and the row then names the process that has just started
    // rather than the one the release gave back.
    if let Err(error) = feed_resumed(&inner.bridge, &inner.store, &session_id) {
        tracing::warn!(
            session = %session_id,
            error = %error,
            "a resumed session's row was not written"
        );
    }
    let _ = inner.store.release_binding(&session_id, &session_id);
    inner.store.open_task(causality, task_cause(causality))?;
    inner.store.bind_task(&session_id, task_id)?;
    let now = Instant::now();
    if let Some(slot) = inner.sessions.get_mut(key) {
        slot.session = resumed.clone();
        slot.suspended = false;
        slot.task_id = Some(task_id.to_string());
        slot.payload = Some(envelope.clone());
        slot.causality = causality.clone();
        slot.origin = Some(envelope.from.clone());
        slot.msg_id = None;
        slot.ready = false;
        slot.idle_since = None;
        // The runtime that resumes this session is a new process, and its plugin
        // has to dial before any frame of this delivery can reach it: the
        // reconnect window is the one that reads a plugin which never arrives.
        slot.dropped_at = (!inner.backend.self_driven()).then_some(now);
        slot.last_beat = Some(now);
    }
    tracing::info!(
        session = %session_id,
        task = %task_id,
        backend = %resumed.backend,
        "suspended session resumed for its scope's next delivery"
    );
    inner.stall.note_assigned(task_id, Instant::now());
    Ok(resumed)
}

/// Advance one session's watermark one sequence past where it stands.
///
/// The row's dimensions are unchanged, so this is the write-side half of "this
/// session was written about": the store's gate takes only a strictly newer
/// version, and the publish that carries a new binding has to clear it. A
/// failure here is not fatal — the binding is still taken in this client's own
/// store, and the next publish of a real transition carries it — so it is
/// logged and the caller goes on.
fn advance_session_row(inner: &mut DispatchInner, session_id: &str) {
    let row = match inner.store.get_session(session_id) {
        Ok(Some(row)) => row,
        _ => return,
    };
    let seq = row.seq.max(0) as u64 + 1;
    if let Err(error) =
        inner
            .store
            .bump_session_version(session_id, row.generation.max(0) as u64, seq)
    {
        tracing::warn!(
            session = %session_id,
            error = %error,
            "a rebound session's row was not advanced; the binding reaches the mirror with its next write"
        );
    }
}

/// The resource one delivery was spawned onto, as the slot has to record it:
/// the reference the backend answers to, the command that opened it, and the
/// tools token this client minted for the session it will serve.
struct Spawned {
    session: SessionRef,
    command: Vec<String>,
    tools_token: String,
}

/// Land one spawned session in this role's bookkeeping: the task's own record,
/// the session row it is born onto, the slot that holds its payload, and the
/// stall clock that answers for its work.
///
/// The slot is keyed by the session's own id, which a client-held session takes
/// from the delivery that opened it: that is the key the session keeps for every
/// delivery the scope later hands it, and the spelling its stored row carries.
///
/// Split from `open_session` so a failure names itself as one: every fallible step
/// lives here, and the caller owns the single undo that matters — a resource the
/// backend has already opened. The steps run in the order that keeps the ledger
/// causal: the task record opens before the session row it hosts, and the row
/// before the slot that can report against it.
///
/// This function owns the in-memory half of its own undo. The bridge is tracked
/// first because `feed_created` reads it for the session's generation, and a step
/// that refuses would otherwise leave that track behind: the reconciler answers
/// for sessions this client holds, and a live entry no slot addresses has nothing
/// left to report about it. The resource is the caller's to hand back, because
/// only the caller can give it up off the dispatch lock.
fn stage_slot(
    inner: &mut DispatchInner,
    spawned: Spawned,
    task_id: &str,
    family: Option<String>,
    causality: &Causality,
    envelope: &Envelope,
) -> Result<()> {
    inner.bridge.track_live(spawned.session.clone());
    let landed = (|| -> Result<()> {
        // The task's own record opens with the session that serves it, out of the
        // causality that named the task. A redelivery that found a slot already
        // serving above never reaches this line, so the chain columns are the chain
        // the session opened on; a re-dispatch after retirement refreshes them.
        inner.store.open_task(causality, task_cause(causality))?;
        // A row this task already carries is the record of the session that served it
        // before, and the session staged here is born onto it: `rebase_born_session`
        // moves that row to a generation of its own, and a task with no row keeps the
        // plain seed below. Either way the row the feeds land on starts at
        // `Booting`/`Detached`, under a watermark this session's own count can clear.
        rebase_born_session(inner, task_id);
        feed_created(&inner.bridge, &inner.store, task_id)?;
        feed_dispatched(&inner.bridge, &inner.store, task_id);
        // A plugin-mode session's liveness is the heartbeat its connection sends and
        // nothing else, and that connection has not spoken yet: the window of
        // `[client] reconnect_grace_secs` starts at birth, so a spawn whose plugin
        // never dials is a ghost the sweep can see rather than a slot that holds its
        // resource forever. The mount that attaches clears the stamp. A self-driven
        // backend owns its agent and answers no adapter socket, so it never has a
        // heartbeat to read and its lifecycle, not this clock, is what ends it.
        let dropped_at = (!inner.backend.self_driven()).then(Instant::now);
        // The liveness stamp starts with the slot, so a session whose plugin mounts
        // and then never sends a frame is readable as silent rather than as a
        // session nobody can judge.
        let last_beat = Some(Instant::now());
        inner.sessions.insert(
            spawned.session.task_id.clone(),
            SessionSlot {
                session: spawned.session.clone(),
                // A session the host asked a hosting runtime for is named by the
                // host until the runtime answers `open`; nothing may resume it
                // before that, and a session with no handle is one the runtime
                // will open fresh next time.
                resume_handle: None,
                task_id: Some(task_id.to_string()),
                ready: false,
                payload: Some(envelope.clone()),
                msg_id: None,
                origin: Some(envelope.from.clone()),
                causality: causality.clone(),
                dropped_at,
                last_beat,
                read_only: false,
                family,
                idle_since: None,
                suspended: false,
                tools_token: spawned.tools_token,
                delivered_roles: BTreeSet::new(),
                opened_at: Instant::now(),
                command: spawned.command,
                keeps_idle: !matches!(
                    inner.session_policy.scope,
                    onlyne_config::SessionScope::Oneshot
                ),
            },
        );
        inner.stall.note_assigned(task_id, Instant::now());
        Ok(())
    })();
    if landed.is_err() {
        inner.bridge.untrack_live(&spawned.session.task_id);
    }
    landed
}

/// How a delivery reached this role, which is what the task record's `kind`
/// column holds. A task with a parent above it was handed down from another
/// session's work; one without was given to this role directly. The envelope's
/// own message kind is not that answer: only a task-shaped delivery ever reaches
/// a session, so the kind says nothing the chain does not.
fn task_cause(causality: &Causality) -> &'static str {
    if causality.parent_task.is_some() {
        "relay"
    } else {
        "root"
    }
}

/// Move the row a re-dispatched task already carries onto the generation of the
/// session this dispatch stages.
///
/// This client keeps `client.db` across a restart and a session row is keyed by
/// its task, so the session staged here is born onto whatever row that task
/// already carries; a first dispatch is the only shape with none. That row is the
/// record of the session that served the task before, one no slot of this process
/// holds any more, and none of what it holds can be inherited. Its phase and its
/// resource describe a session that no longer exists, and the feeds below would
/// have to move them from states that refuse them: `resource_attach` from a
/// closed resource is `UndefinedTransition`, which is how the log reads when a
/// ghost was swept before the task came back. Its watermark is worse, because a
/// row it stands on accepts nothing that reads older: the client's own feeds and
/// the plugin's beats share the one counter, and the plugin is a new process
/// whose sequence starts at its base again, below anything a session that lived a
/// while left. Every frame the new session sends is then dropped as a stale
/// duplicate, the turn its agent really ran never reaches the row, and the settle
/// door refuses the completion of work that happened.
///
/// The new generation itself is [`rebase_generation`]'s; the body here is the
/// born tuple of any session — the same `Observation::initial` a fresh row is
/// seeded from, with the role's reconcile policy carried over, so the two ways a
/// session's row comes into being cannot drift. What attests the old generation
/// dead is this client's own bookkeeping: this call is reached only because no
/// slot of this process serves the task, so nothing it holds speaks for that row
/// any more.
fn rebase_born_session(inner: &DispatchInner, task_id: &str) {
    let verdict = rebase_generation(inner, task_id, |stored| {
        Observation::initial(stored.isolate_after, stored.terminate_after)
    });
    match verdict {
        Ok(Some(Verdict::Applied(next))) => tracing::info!(
            task = %task_id,
            generation = next.version.generation,
            "the row a re-dispatched session is born onto was rebased onto a new generation"
        ),
        Ok(Some(verdict)) => tracing::warn!(
            task = %task_id,
            ?verdict,
            "the row of a re-dispatched session was not rebased"
        ),
        Ok(None) => {}
        Err(error) => tracing::warn!(
            task = %task_id,
            error = %error,
            "the row of a re-dispatched session was not rebased"
        ),
    }
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
///
/// The text both hand-over paths carry is [`crate::delivery::render`]'s answer,
/// rendered here and nowhere else: a plugin injects it, a self-driven backend
/// prompts with it, and neither composes a delivery of its own. The delivery's
/// image is written into the workspace first, because the line that names it is
/// the line the model reads.
pub async fn on_ready(state: &DispatchState, notice: ReadyNotice, prose: &str) -> Result<()> {
    let ReadyNotice {
        task_id,
        session_id,
        generation,
        io,
        capabilities,
    } = notice;
    let (payload, target, session, backend, version, workspace) = {
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
                // The delivery is the binding this session is serving now, and
                // the session's own id is the other spelling a plugin may
                // report under: a session that has served several deliveries
                // answers to both, and the binding is what a ready report names.
                slot.task_id.as_deref() == Some(task_id.as_str())
                    || slot.session.task_id == task_id
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
        if matches!(verdict, Verdict::Applied(_)) {
            // The ready report is a frame this session sent that the reducer
            // took, so it is liveness like any other: the sweep reads the stamp
            // rather than the socket, and a session whose plugin passed the
            // barrier and then went quiet has to be readable as quiet.
            note_beat(&mut inner, &task_id, Instant::now());
        }
        let version = note_verdict(&verdict, &task_id).unwrap_or(Version::new(generation, 0));
        (
            payload,
            io,
            session,
            backend,
            version,
            inner.workspace.clone(),
        )
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
    // The body travels as it was written, the material is quoted, and the
    // attachments are the paths just written under the workspace. Upstream
    // material is the one input no delivery carries yet: the field a sender
    // fills it from is slice 3b's `complete(details)`.
    let attachments = write_attachment(&workspace, &task_id, &payload)
        .into_iter()
        .collect::<Vec<_>>();
    let text = render(
        &from_label(&payload.from),
        payload.body.text.as_deref().unwrap_or_default(),
        None,
        &attachments,
    );
    match (backend.self_driven(), target) {
        (true, None) => {
            // A self-driven drive has no heartbeats, so the dispatch path feeds
            // the turn-started fact here — the same fact a plugin's beat would
            // carry — before the backend starts the turn. The never-ran guard
            // reads the row this writes, so a completion the agent files
            // through its tools mount during the turn passes it.
            state.feed_turn_started(&task_id);
            backend.deliver(&session, &task_id, &text)
        }
        (true, Some(_)) => Err(anyhow!(
            "self-driven session {session_id} unexpectedly has an adapter transport"
        )),
        (false, Some(target)) if capabilities.contains(&Capability::Inject) => {
            let assign = AssignArgs {
                envelope: Box::new(payload),
                prose: prose.to_string(),
                text,
                attachments,
                task_id,
                generation,
                session_id: Some(session_id.to_string()),
                scope: Some(scope_word(&state.session_policy().scope)),
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

impl DispatchState {
    /// Bind a plugin transport to one staged session and hand it the payload.
    ///
    /// Both hand-over paths run through here: a plugin that mounted first, and a
    /// session staged first. The report that marks the session ready leaves
    /// before the assignment, which is the causal order §6 line 285 fixes.
    ///
    /// The caller names the session. The delivery it is serving is read here,
    /// because the two are one id only until a scope hands the session a second
    /// delivery: the ready report and the reducer facts answer for the delivery,
    /// and the session is the row they land on.
    pub async fn hand_session(
        &self,
        session_id: &str,
        io: AdapterIo,
        capabilities: Vec<Capability>,
    ) -> Result<()> {
        let prose = self.role_prose();
        let (task_id, generation) = self.served_delivery(session_id);
        on_ready(
            self,
            ReadyNotice {
                task_id,
                session_id: session_id.to_string(),
                generation,
                io: Some(io),
                capabilities,
            },
            &prose,
        )
        .await
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
            let (task_id, generation) = self.served_delivery(session_id);
            on_ready(
                self,
                ReadyNotice {
                    task_id,
                    session_id: session_id.to_string(),
                    generation,
                    io: None,
                    capabilities: Vec::new(),
                },
                &prose,
            )
            .await?;
            return Ok(true);
        }
        // Three sources, in the order that keeps every role's behaviour the
        // shape it had: the session's own transport, then a parked agent — one
        // the client spawned for work in hand, so it is the more specific match.
        // A hosting runtime's connection is not a third source here: the session
        // reaches it by being asked for, so a connection that has already been
        // lent one is lent nothing by this path.
        let transport = self
            .session_transport(session_id)
            .or_else(|| self.claim_parked_transport(session_id));
        let Some((io, capabilities)) = transport else {
            return Ok(false);
        };
        self.hand_session(session_id, io, capabilities).await?;
        Ok(true)
    }

    /// The delivery one session is serving, and the generation it runs under.
    ///
    /// A session between deliveries answers with its own id, which is the
    /// spelling the row it was born onto was written at: the payload is handed
    /// over in the same breath a delivery is bound, so this is the idle-session
    /// fallback rather than the ordinary reading.
    fn served_delivery(&self, session_id: &str) -> (String, u64) {
        let inner = self.inner.lock();
        let slot = inner
            .sessions
            .iter()
            .find(|(key, slot)| names_session(key, slot, session_id))
            .map(|(_, slot)| slot);
        match slot {
            Some(slot) => (
                slot.task_id
                    .clone()
                    .unwrap_or_else(|| slot.session.task_id.clone()),
                slot.session.generation,
            ),
            None => (session_id.to_string(), 1),
        }
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
        // A note reaches a live agent the way a task does, so it travels the same
        // one template and its image is written under the workspace first.
        let workspace = self.inner.lock().workspace.clone();
        let attachments = write_attachment(&workspace, &task_id, envelope)
            .into_iter()
            .collect::<Vec<_>>();
        let text = render(
            &from_label(&envelope.from),
            envelope.body.text.as_deref().unwrap_or_default(),
            None,
            &attachments,
        );
        let assign = AssignArgs {
            envelope: Box::new(envelope.clone()),
            prose: self.role_prose(),
            text,
            attachments,
            task_id,
            generation,
            session_id: Some(session_id.to_string()),
            scope: Some(scope_word(&self.session_policy().scope)),
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
}
