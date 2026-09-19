//! Hand one turn's work on to the roles its report named.
//!
//! One route carries a handoff in this product: an agent with no server link
//! leaves `handoff:` lines in its closing report, the session backend reads them
//! through [`onlyne_proto::payload`], and this client speaks for the work
//! afterwards, because it is the only process holding a role connection.
//!
//! A report names as many next roles as its lines fit under the grammar's cap,
//! and each line carries its own text for that recipient. A chain of such
//! relays stays open by design: the `allowed_targets` edges the supervisor
//! writes in `spec.toml` decide what a role may address, and the server's ACL
//! gate answers each send on those edges alone.
//!
//! What a handoff therefore is on the wire: one ordinary `MsgKind::Task`
//! envelope, sent by this role over the link it already has, whose causality
//! names the settled task as its parent and sits one hop below it. That is what
//! `onlyne handoff` builds from inside a session, and the server's router
//! answers both the same way — ACL, offline queue, ledger row and all. No second
//! socket and no invented verb.
//!
//! The prose keeps the `handoff:` prefix the report line used, so a recipient
//! reading its inbox can see another role passed work along rather than a human
//! opening a request.

use crate::dispatch::DispatchState;
use onlyne_proto::{
    Body, Causality, ClientOp, Envelope, ErrorCode, Handoff, MsgKind, Principal, new_envelope,
    new_task_id,
};
use std::time::{Duration, Instant};

/// The whole budget for one turn's handoffs. A report that cannot be routed
/// inside it settles its task anyway: the verdict is the agent's, and no relay
/// holds it hostage.
pub const HANDOFF_ROUTE_BUDGET: Duration = Duration::from_secs(6);

/// The prefix a relayed body carries, so the recipient reads a relay. Named
/// apart from `payload::HANDOFF_PREFIX`, which marks a line inside one agent's
/// report file.
pub const RELAY_BODY_PREFIX: &str = "handoff: ";

/// One handoff line this client could not put on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    /// The role the report named.
    pub to_role: String,
    /// The body that would have been sent.
    pub text: String,
    /// What stopped it, in the words the server or the transport used.
    pub reason: String,
}

/// Route every handoff line of one turn and report the ones that failed.
///
/// A `blocked` verdict routes nothing: its task finished no work, so there is
/// nothing to pass along. The session backend drops those lines before they
/// reach here, and this is the second gate, the one that still holds when a
/// caller arrives with a list it built itself.
///
/// Every line is answered before this returns, or the budget runs out and the
/// lines left standing come back as denials. None of it moves the settled task:
/// a refused relay is a record, never a verdict.
pub async fn route(
    state: &DispatchState,
    role: &str,
    task_id: &str,
    hop: u32,
    head_kind: Option<&str>,
    head: &str,
    handoffs: &[Handoff],
) -> Vec<Denial> {
    if handoffs.is_empty() {
        return Vec::new();
    }
    if head_kind == Some("blocked") {
        tracing::warn!(
            task = %task_id,
            handoffs = handoffs.len(),
            "a blocked report hands no work on"
        );
        return Vec::new();
    }
    let deadline = Instant::now() + HANDOFF_ROUTE_BUDGET;
    let mut denied = Vec::new();
    for handoff in handoffs {
        let text = handoff.text_or(head);
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            denied.push(denied_for(handoff, text, "the handoff budget ran out"));
            continue;
        }
        let envelope = match relay(role, task_id, hop, &handoff.to_role, text) {
            Ok(envelope) => envelope,
            Err(reason) => {
                denied.push(denied_for(handoff, text, &reason));
                continue;
            }
        };
        if let Some(reason) = deliver(state, &envelope, left).await {
            denied.push(denied_for(handoff, text, &reason));
        }
    }
    denied
}

/// One refusal, carrying what the recipient would have read.
fn denied_for(handoff: &Handoff, text: &str, reason: &str) -> Denial {
    Denial {
        to_role: handoff.to_role.clone(),
        text: text.to_string(),
        reason: reason.to_string(),
    }
}

/// The task envelope one handoff line becomes: a child of the settled task, one
/// hop deeper, sent by this role and addressed to the named one.
fn relay(
    role: &str,
    task_id: &str,
    hop: u32,
    to_role: &str,
    text: &str,
) -> Result<Envelope, String> {
    let causality = Causality {
        task: new_task_id(),
        parent_task: Some(task_id.to_string()),
        reply_to: None,
        hop: hop + 1,
        attempt: 0,
    };
    // `new_envelope` is the validator, so a recipient this protocol cannot
    // address fails here rather than reaching the server as a frame it must
    // refuse.
    new_envelope(
        MsgKind::Task,
        Principal::role(role),
        Principal::role(to_role),
        Body::text(format!("{RELAY_BODY_PREFIX}{text}")),
        Some(causality),
    )
    .map_err(|error| error.to_string())
}

/// Send one relayed task and answer with the reason it was not accepted, `None`
/// when it was.
///
/// The link's request round trip is what carries the server's verdict, so the
/// two refusals that mean something here — `acl_denied` and `unknown_role` — are
/// read off the response body instead of inferred. A transport that cannot
/// answer at all is not a refusal: the envelope falls to the durable intent
/// queue, which is how every other outbound frame of this role survives a
/// disconnect.
async fn deliver(state: &DispatchState, envelope: &Envelope, timeout: Duration) -> Option<String> {
    let op = ClientOp::Send(Box::new(envelope.clone()));
    match tokio::time::timeout(timeout, state.request(op)).await {
        Ok(Ok(body)) if body.ok => None,
        Ok(Ok(body)) => Some(match body.error {
            Some(refusal) => format!("{}: {}", refusal.code.as_str(), refusal.message),
            None => "the server refused the relay without a reason".to_string(),
        }),
        Ok(Err(error)) if is_refusal(&error) => Some(error.to_string()),
        Ok(Err(_)) => state
            .enqueue_outbound(envelope)
            .err()
            .map(|failure| format!("the relay was not queued: {failure}")),
        Err(_elapsed) => Some("the handoff budget ran out mid-request".to_string()),
    }
}

/// Whether a transport error is the server saying no, which no queue should
/// retry, rather than the wire failing, which one should.
fn is_refusal(error: &onlyne_net::NetError) -> bool {
    matches!(error, onlyne_net::NetError::Rejected { code, .. }
        if code == ErrorCode::AclDenied.as_str() || code == ErrorCode::UnknownRole.as_str())
}
