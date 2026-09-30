//! The client-side constraints a session's tool calls are measured against.
//!
//! A `tools` mount carries no policy of its own (`docs/v2-CONTRACT.md` §3b):
//! the hop budget, the relay requirement, the completion's shape and the
//! `details` ceiling are enforced here, where the frame is handled, so the pi
//! drive and the ACP drive refuse identically. The frame's answer is its own
//! refusal — a tool call is answered as a tool error, not as a state frame that
//! also refused something — which is why these read a report and return the
//! `ResBody` the sender is handed.
//!
//! Two of the checks are the pi plugin's private guard moved down: the relay
//! requirement (a session that owes a downstream handoff may not report a
//! terminal outcome) and the completion's shape. The plugin's own wording is
//! kept so the sentence a model reads does not change with the drive that
//! delivered it. There is no waiver: the plugin's `force`/`reason` escape hatch
//! went with its guard, so a session that cannot make the handoff it owes
//! reports nothing and is settled by the turn-end rule — which is the honest
//! outcome, and what to do about it is operator policy (§3c).
//!
//! One duty of this module is attribution rather than constraint. A `tools`
//! mount names nothing session-scoped — the token in its hello is the binding —
//! so the task a frame names, the sender it leaves as, and its whole causality
//! chain are *stamped* from this client's own record of the session that token
//! names. `onlyne mcp` supplies the recipient, the text, the kind, and the
//! image, and nothing else (`docs/v2-CONTRACT.md` §3b): a caller that named its
//! own task or role would be asserting facts only this client's books hold.

use super::state::{DispatchInner, DispatchState, slot_key_named, slot_key_serving_task};
use super::transport::serves_session;
use super::*;
use onlyne_proto::adapter::HandoffArgs;
use onlyne_proto::{DETAILS_MAX_BYTES, ErrorCode, Report, ResBody};

/// The slot key a connection speaks for, live transport or tools mount alike.
///
/// The tools binding and the transport map are the two ways a connection is
/// tied to a session, and a frame's sender is the connection: resolving the
/// key here is what lets the relay guard read the session's own delivery
/// record rather than one the frame claims.
pub(super) fn session_key_of_connection(inner: &DispatchInner, io: &AdapterIo) -> Option<String> {
    let named = inner
        .transports
        .iter()
        .find(|(_, (transport, _))| transport.same_connection(io))
        .map(|(session, _)| session.clone())
        .or_else(|| {
            inner
                .tools_mounts
                .iter()
                .find(|(_, bound)| bound.same_connection(io))
                .map(|(key, _)| key.clone())
        })?;
    slot_key_named(inner, &named)
}

/// A `tools` mount's own bookkeeping, stamped from this client's record.
///
/// The scope is [`DispatchState::tools_scope`]'s answer — one locked pass over
/// the connection's binding and the session's slot — and the stamps below are
/// pure over it, so a frame cannot be attributed from a state that moved between
/// the lookup and the write.
impl DispatchState {
    /// The one sentence a `tools` mount reads when the session its token names is
    /// gone.
    ///
    /// The handshake and the per-frame gate answer it. The adapter's welcome
    /// refusal carries a code and a message and has no field slot, so the
    /// sentence is where the field name goes; the per-frame [`Self::tools_gone`]
    /// names the same field in the slot it has.
    pub const TOOLS_GONE_MESSAGE: &'static str = "token names no live session";

    /// The refusal a `tools` frame earns when its connection speaks for no live
    /// session.
    ///
    /// The field is named and the token's own value is not: the token is a
    /// capability the caller already holds, and a sentence that echoed it would
    /// put one in a fault a model reads (`AGENTS.md` §8). A session retires
    /// between one frame's liveness check and its stamping, and both doors read
    /// one answer because it is one fact.
    pub fn tools_gone() -> ResBody {
        ResBody::err(
            ErrorCode::Unauthorized,
            Self::TOOLS_GONE_MESSAGE,
            Some("token".to_string()),
        )
    }

    /// Stamp one `report` or `handoff` frame from a `tools` mount with the task
    /// the session serves.
    ///
    /// An empty `task_id` is this path's spelling of "the session's own open
    /// task" and is replaced with it. A non-empty one that disagrees with the
    /// session's own binding is refused rather than silently overwritten — a
    /// caller wrong about the session it speaks for is a bug in the caller, and
    /// a correction that hides it is the wrong answer. A session holding no open
    /// task answers `invalid` on the same field, because a completion for a task
    /// nobody holds cannot be recorded.
    pub fn stamp_tools_task(
        &self,
        io: &AdapterIo,
        task_id: &mut String,
    ) -> std::result::Result<(), ResBody> {
        let Some(scope) = self.tools_scope(io) else {
            return Err(Self::tools_gone());
        };
        let Some(open) = scope.task_id else {
            return Err(ResBody::err(
                ErrorCode::Invalid,
                "this session holds no open task",
                Some("task_id".to_string()),
            ));
        };
        if task_id.is_empty() {
            *task_id = open;
            return Ok(());
        }
        if *task_id != open {
            return Err(ResBody::err(
                ErrorCode::Forbidden,
                format!("this session serves task {open}, not {task_id}"),
                Some("task_id".to_string()),
            ));
        }
        Ok(())
    }

    /// The sender one `send` frame from a `tools` mount leaves as, and the chain
    /// that frame starts.
    ///
    /// The bridge supplies the recipient, the body, the kind, and the image; the
    /// role the frame leaves as comes from this client's own record, so nothing
    /// here is read off the frame or off `welcome`. A session serving no
    /// delivery is refused: a send it made would belong to no work at all.
    ///
    /// `handoff` continues a family and `send` starts one, so a `task` send is a
    /// **root** — a fresh task id at hop 0, attempt 0, with a fresh `op_id` — and
    /// nothing of the served delivery's family, budget, origin, or deadline rides
    /// along. The hop budget belongs to the family, which is why a spent budget
    /// stops a forward and never new work. A reader who "simplifies" this into
    /// [`Causality::child_of`] would make a new task spend its sender's hop and
    /// hand the server an envelope that `plugins/onlyne-agent-pi`'s own
    /// `sendEnvelope` never mints: two drives, one obligation, two shapes. A
    /// `note` joins no family: no chain and no `op_id`.
    pub fn stamp_tools_send(
        &self,
        io: &AdapterIo,
        envelope: &mut Envelope,
    ) -> std::result::Result<(), ResBody> {
        let Some(scope) = self.tools_scope(io) else {
            return Err(Self::tools_gone());
        };
        if scope.task_id.is_none() {
            return Err(ResBody::err(
                ErrorCode::Invalid,
                "this session serves no delivery, so a send it makes belongs to no work",
                None,
            ));
        }
        envelope.from = Principal::role(self.inner.lock().role.clone());
        if envelope.kind == MsgKind::Task {
            envelope.op_id = Some(onlyne_proto::new_op_id());
            envelope.causality = Some(Causality::root(onlyne_proto::new_task_id()));
        } else {
            envelope.op_id = None;
            envelope.causality = None;
        }
        Ok(())
    }
}

/// Record one delivery a session made, which is the relay guard's evidence.
///
/// Any envelope kind counts — a `note` and a `task` are both the session
/// reaching that role — and a recipient that names no role (a gateway, a
/// cluster) is not a downstream handoff. The caller records only sends the
/// client has carried: a refused envelope was never a delivery.
pub(super) fn record_delivery(inner: &mut DispatchInner, key: &str, to: &Principal) {
    let Some(role) = to.role_name() else {
        return;
    };
    if let Some(slot) = inner.sessions.get_mut(key) {
        slot.delivered_roles.insert(role.to_string());
    }
}

/// The relay guard's refusal, when the session still owes a delivery.
///
/// The obligation is the role's own `allowed_targets`: the list the server
/// gates the ACL on, which the handshake carries (and a reload's role row
/// re-carries). A session of that role must have delivered to every downstream
/// name on it before it may report a terminal outcome, and a role that declares
/// no target owes nothing.
///
/// The role this session's task came from is never one of them. The completion
/// is itself a delivery to that role — the one the ledger books the answer
/// against — so owing a second one would make the obligation unsatisfiable for
/// the self-addressed entry `onlyne-client init` prints and about twenty
/// fixtures restate (`e2e/acp-tools.sh`), and for a ring's return edge
/// (`e2e/running-lights.sh`). What the exclusion drops is the origin alone, not
/// the edge: an entry naming a downstream role beside it still owes that one.
/// An origin this client does not hold as a role — a principal naming none, or
/// no origin at all — buys no exclusion, which keeps the guard strict wherever
/// the exclusion cannot be justified.
///
/// The refusal names every role still owed and the set the session actually
/// delivered to. That sentence is what a model reads, and it is the same one
/// both drives see — the guard is here, where the frame is handled, so the pi
/// drive and the ACP drive refuse identically.
fn relay_refusal(inner: &DispatchInner, key: &str) -> Option<String> {
    let slot = inner.sessions.get(key)?;
    if inner.required_targets.is_empty() {
        return None;
    }
    let origin = slot.origin.as_ref().and_then(Principal::role_name);
    let missing: Vec<&str> = inner
        .required_targets
        .iter()
        .filter(|role| {
            Some(role.as_str()) != origin && !slot.delivered_roles.contains(role.as_str())
        })
        .map(String::as_str)
        .collect();
    if missing.is_empty() {
        return None;
    }
    let delivered = if slot.delivered_roles.is_empty() {
        "none".to_string()
    } else {
        slot.delivered_roles
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    Some(format!(
        "relay guard: missing handoff to: {} (this session delivered to: {delivered})",
        missing.join(", "),
    ))
}

/// The shape rule one completion carries: `details` inside the cap, and `files`
/// naming absolute paths.
fn shape_refusal(report: &Report) -> Option<(String, &'static str)> {
    let Report::Complete { details, files, .. } = report else {
        return None;
    };
    if let Some(details) = details {
        if details.len() > DETAILS_MAX_BYTES {
            return Some((
                format!("details exceeds the {DETAILS_MAX_BYTES}-byte cap"),
                "details",
            ));
        }
    }
    for path in files {
        if !Path::new(path).is_absolute() {
            return Some((format!("files must name absolute paths: {path}"), "files"));
        }
    }
    None
}

impl DispatchState {
    /// The refusal one incoming completion meets, when a client-side constraint
    /// refuses it; `None` when the frame may settle.
    ///
    /// `from` is the connection the frame arrived on. A completion that arrives
    /// with no connection — the local operator surface, and the fault a refused
    /// `focus` files — is measured against the shape rule alone: the relay guard
    /// reads the session's own delivery record, and a door with no sender has no
    /// session whose record it could read.
    pub fn completion_refusal(&self, from: Option<&AdapterIo>, report: &Report) -> Option<ResBody> {
        if let Some((message, field)) = shape_refusal(report) {
            return Some(ResBody::err(
                ErrorCode::Invalid,
                message,
                Some(field.to_string()),
            ));
        }
        let Report::Complete { .. } = report else {
            return None;
        };
        let io = from?;
        let inner = self.inner.lock();
        let key = session_key_of_connection(&inner, io)?;
        let message = relay_refusal(&inner, &key)?;
        Some(ResBody::err(ErrorCode::Invalid, message, None))
    }

    /// The refusal one `handoff` frame meets when the family's hop budget is
    /// spent; `None` when the frame may be built.
    ///
    /// A child sits one hop below the task that hands it on, and a family's
    /// `hop_budget` is the depth the chain may reach: the frame that would sit
    /// over it is refused here and names the budget it would break, rather than
    /// being minted for the server to sort out. A frame from a connection that
    /// does not serve the task is left for `plugin_handoff`'s own authority
    /// answer, so this check reveals nothing to a foreign connection.
    pub fn handoff_refusal(&self, io: &AdapterIo, args: &HandoffArgs) -> Option<ResBody> {
        let inner = self.inner.lock();
        let key = slot_key_serving_task(&inner, &args.task_id)?;
        if !serves_session(&inner, &key, io) {
            return None;
        }
        hop_refusal(&inner, &key, "handoff")
    }
}

/// The hop budget's own refusal for one session, naming the frame it answered.
///
/// A child sits one hop below the task its session serves, and a family's
/// `hop_budget` is the depth the chain may reach: the frame that would sit over
/// it is refused here and names the budget it would break, rather than being
/// minted for the server to sort out. Only a `handoff` reaches this door: a
/// `send` starts a family of its own and spends no hop of this one.
fn hop_refusal(inner: &DispatchInner, key: &str, what: &str) -> Option<ResBody> {
    let slot = inner.sessions.get(key)?;
    let budget = slot.causality.hop_budget?;
    let next = slot.causality.hop.saturating_add(1);
    (next > budget).then(|| {
        ResBody::err(
            ErrorCode::Invalid,
            format!("hop budget exhausted: this {what} would sit at hop {next} of {budget}"),
            None,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::shape_refusal;
    use onlyne_proto::{DETAILS_MAX_BYTES, Outcome, Report};

    /// One completion's shape, at the client's own door.
    ///
    /// The plan names this case by hand: "`complete` carrying a `details` body
    /// over the cap is refused with the cap named, and one at the cap passes"
    /// (`docs/v2-CONTRACT.md` §3c). The boundary is `>` and not `>=`, so the
    /// table carries the exact cap and one byte over it — a guard that read
    /// `>=` would refuse a completion the plan says passes, and a model would be
    /// told it had written too much when it had written exactly the limit.
    #[test]
    fn a_completion_at_the_cap_passes_and_one_byte_over_it_is_refused() {
        let at_cap = "x".repeat(DETAILS_MAX_BYTES);
        let over = "x".repeat(DETAILS_MAX_BYTES + 1);

        let at = shape_refusal(&complete(Some(at_cap.clone()), &[]));
        assert_eq!(at, None, "the cap itself is inside the cap");

        let over = shape_refusal(&complete(Some(over), &[]));
        let (message, field) = over.expect("one byte over the cap is refused");
        assert_eq!(field, "details", "the refusal names the field");
        assert!(
            message.contains(&DETAILS_MAX_BYTES.to_string()),
            "the refusal names the cap it measured against, so the model can \
             count its own bytes: {message}"
        );

        // No details at all is not a zero-length details: an absent field is
        // absent, and a cap that read `None` as empty would refuse a completion
        // that simply reported nothing.
        assert_eq!(shape_refusal(&complete(None, &[])), None);
    }

    /// `files` names paths the model is expected to read.
    ///
    /// A relative path resolves against whatever the runtime's working directory
    /// happens to be — a pane, a tab, a child's cwd — so a completion carrying
    /// one points the next agent at a file that is not there, and the delivery
    /// reads as a truncated result rather than a wrong one. The refusal quotes
    /// the offending path so the model can see which of its own entries was the
    /// problem.
    #[test]
    fn a_file_that_is_not_an_absolute_path_is_refused_by_name() {
        let refused = shape_refusal(&complete(
            None,
            &["relative/out.txt".to_string(), "/abs/ok.txt".to_string()],
        ));
        let (message, field) = refused.expect("a relative path is refused");
        assert_eq!(field, "files", "the refusal names the field");
        assert!(
            message.contains("relative/out.txt"),
            "the refusal quotes the path: {message}"
        );

        assert_eq!(
            shape_refusal(&complete(None, &["/abs/a.png".to_string()])),
            None,
            "an absolute path is the shape the contract asks for"
        );
        assert_eq!(
            shape_refusal(&complete(None, &[])),
            None,
            "no files, no rule"
        );
    }

    /// The shape rule reads one report, and a report that is not a completion has
    /// no shape to check.
    #[test]
    fn a_report_that_is_not_a_completion_is_never_refused_for_shape() {
        let ready = Report::Ready {
            task_id: "t-1".to_string(),
            session_id: "s-1".to_string(),
            generation: 1,
            seq: 7,
            cluster_ref: None,
        };
        assert_eq!(shape_refusal(&ready), None);
    }

    fn complete(details: Option<String>, files: &[String]) -> Report {
        Report::Complete {
            task_id: "t-1".to_string(),
            outcome: Outcome::Done,
            head: Some("done".to_string()),
            details,
            files: files.to_vec(),
            reply_to: None,
            cluster_ref: None,
        }
    }
}
