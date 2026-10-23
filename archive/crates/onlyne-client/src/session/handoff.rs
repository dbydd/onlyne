//! Hand one turn's work on to the roles its frame named.
//!
//! One route carries a handoff in this product: the session's own op. A plugin
//! sends the mount's `handoff` frame and a session driven through the client's
//! tools mount sends the same op, and this client speaks for the work
//! afterwards, because it is the only process holding a role connection.
//!
//! A frame names one recipient and carries its own text for that recipient. A
//! chain of such relays stays open by design: the `allowed_targets` edges the
//! supervisor writes in `spec.toml` decide what a role may address, and the
//! server's ACL gate answers each send on those edges alone.
//!
//! What a handoff therefore is on the wire: one ordinary `MsgKind::Task`
//! envelope, sent by this role over the link it already has, whose causality
//! names the task it was sent under as its parent and sits one hop below it.
//! That is what `onlyne handoff` builds from inside a session, and the server's
//! router answers both the same way — ACL, offline queue, ledger row and all. No
//! second socket and no invented verb.
//!
//! The prose keeps the `handoff:` prefix, so a recipient reading its inbox can
//! see another role passed work along rather than a human opening a request.

use onlyne_proto::{Body, Causality, Envelope, ImagePart, MsgKind, Principal, new_envelope};

/// The prefix a relayed body carries, so the recipient reads a relay.
pub const RELAY_BODY_PREFIX: &str = "handoff: ";

/// The task envelope one handoff becomes: a child of the parent task's causality,
/// one hop deeper, sent by this role and addressed to the named one.
///
/// The child link is [`Causality::child_of`]'s answer, so the family id, the hop
/// budget, the origin, the deadline, and the labels ride along on every relay
/// this client writes. The link travels back beside the envelope, which is what
/// lets a plugin-op answer name the task the host minted and the depth it sits at.
///
/// Every handoff this client carries is built here — the agent mount's frame and
/// the tools mount's op alike — so one family rule covers both routes.
pub(crate) fn relay(
    role: &str,
    parent: &Causality,
    to_role: &str,
    text: &str,
    image: Option<ImagePart>,
) -> Result<(Envelope, Causality), String> {
    let causality = parent.child_of();
    let mut body = Body::text(format!("{RELAY_BODY_PREFIX}{text}"));
    body.image = image;
    // `new_envelope` is the validator, so a recipient this protocol cannot
    // address fails here rather than reaching the server as a frame it must
    // refuse.
    new_envelope(
        MsgKind::Task,
        Principal::role(role),
        Principal::role(to_role),
        body,
        Some(causality.clone()),
    )
    .map(|envelope| (envelope, causality))
    .map_err(|error| error.to_string())
}
