//! The canonical reply tree as one platform's link shape.
//!
//! `docs/v2.4-REPLY-THREADS.md` §5/§6 is the design; this module is the
//! conversion layer it asks for. The canonical model has one list per reply
//! chain with a structural `parent` (see [`crate::thread`]); a platform carries
//! that in one of three shapes, and which one is the platform's business, not
//! the core's:
//!
//! | shape | a list is | what a reply carries |
//! |---|---|---|
//! | [`LinkShape::FlatList`] | one flat list anchored at its root (Slack's thread, Feishu's reply group) | the list's root, and the parent inside it |
//! | [`LinkShape::Chain`] | a chain of forward pointers, the list being the chain itself (QQ's `msg_id`) | the parent only; the chain *is* the list |
//! | [`LinkShape::Headers`] | `In-Reply-To` plus the `References` chain (email) | the parent and every ancestor from the root down |
//!
//! A platform that can express neither is [`LinkShape::Flat`], where nothing
//! rides outbound and the reference text carries the context — the same
//! degradation the delivery template already makes for quote material.
//!
//! The conversion is a pure function over a recorded message, so a backend
//! asks once for the outbound link and never re-derives the list.

use crate::thread::Message;

/// How one platform carries reply links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkShape {
    /// One flat list per reply chain; the root is addressable on its own.
    FlatList,
    /// One forward pointer per message; the list is the chain of them.
    Chain,
    /// Headers: the parent plus the whole ancestor chain.
    Headers,
    /// Nothing structural rides outbound; context travels as quoted text.
    Flat,
}

/// Which shape one platform's backend carries, by its platform name.
///
/// A table rather than a wire field: the shape is a property of the platform,
/// not of one run of its backend, and the gateway is the side that has to know
/// it before a message arrives. An unknown platform reads as [`LinkShape::Flat`]
/// — nothing structural rides outbound, which is the safe default for a channel
/// whose reply handling nobody has measured.
///
/// Measured, one live session each (2026-10-14):
///
/// | platform | shape | what the platform actually takes |
/// |---|---|---|
/// | `qqbot` | `Chain` | `msg_id` only, one forward pointer |
/// | `feishu` | `FlatList` | a reply group is flat, and a message in it may answer any earlier one |
/// | `telegram` | `Flat` | `reply_to_message_id` exists but carries no list; the gateway keeps its own |
/// | `fake` | `Flat` | nothing to carry |
/// How long a platform accepts a passive reply, measured from the message it
/// answers.
///
/// QQ is the one that has a clock: `40034005 回复消息msg_id已过期` is what a
/// passive reply past five minutes gets, and the operator then has to send from
/// the platform's own client or wait for the next message. An explicit window
/// makes the refusal a sentence the operator reads here rather than a platform
/// error against a row that already left the queue.
///
/// A platform without a window answers `None` and nothing is checked.
pub fn passive_window(platform: &str) -> Option<std::time::Duration> {
    match platform {
        "qqbot" => Some(std::time::Duration::from_secs(5 * 60)),
        _ => None,
    }
}

pub fn shape_of(platform: &str) -> LinkShape {
    match platform {
        "qqbot" => LinkShape::Chain,
        "feishu" => LinkShape::FlatList,
        _ => LinkShape::Flat,
    }
}

/// What an outbound reply must carry on one platform.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutboundLink {
    /// The message this reply answers. Every structural shape carries it; only
    /// [`LinkShape::Flat`] leaves it empty.
    pub parent: Option<String>,
    /// The list the reply belongs to, when the platform names lists on their
    /// own (a thread root, a subject). A chain carries it implicitly, which is
    /// what `None` means there.
    pub list: Option<String>,
    /// The chain from the list's root down to the parent, when the platform
    /// spells it out. Empty for every other shape.
    pub chain: Vec<String>,
}

/// Build the outbound link for `message` on a platform of the given shape.
///
/// `message` is the reply being sent, already recorded: its `parent` is the
/// structural link and its `list` the list it belongs to, which is what makes
/// this a lookup rather than a walk. `ancestors` is the recorded chain for the
/// message's list, oldest first, and only [`LinkShape::Headers`] reads it.
pub fn outbound(message: &Message, shape: LinkShape, ancestors: &[String]) -> OutboundLink {
    let parent = message.parent.clone();
    match shape {
        LinkShape::Flat => OutboundLink::default(),
        LinkShape::Chain => OutboundLink {
            parent,
            ..OutboundLink::default()
        },
        LinkShape::FlatList => OutboundLink {
            parent,
            list: Some(message.list_id().to_string()),
            chain: Vec::new(),
        },
        LinkShape::Headers => OutboundLink {
            parent,
            list: ancestors.first().cloned(),
            chain: ancestors.to_vec(),
        },
    }
}

/// What an inbound message of one platform's shape means, canonically.
///
/// The other direction: a backend reports the link it received, and the
/// gateway decides what the message *is* — a reply that joins a list, or a
/// reference that points and moves nothing. A platform that reports only a
/// parent (QQ, Telegram) gives no references at all; one that reports a thread
/// root (Slack, Feishu) can tell the two apart.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InboundLink {
    /// The reply target the platform names, if any.
    pub reply_to: Option<String>,
    /// The root the platform put the message in, if it says.
    pub list: Option<String>,
    /// Messages this one points at without answering.
    pub references: Vec<String>,
}

/// Fold an inbound link onto a message being recorded.
///
/// The list the message joins is, in order of preference: the list the platform
/// placed it in, the list of the message it replies to, the replied-to message
/// itself when that message is a root — which is the ATProto `{root, parent}`
/// pair again, where a reply to a root inherits the root as its list.
pub fn assign_list(
    message: &Message,
    inbound: &InboundLink,
    replied_to: Option<&Message>,
) -> String {
    if let Some(list) = inbound.list.as_deref() {
        return list.to_string();
    }
    match (inbound.reply_to.as_deref(), replied_to) {
        (Some(_), Some(parent)) if parent.list.is_some() => parent.list.clone().unwrap(),
        (Some(_), Some(parent)) => parent.id.clone(),
        (Some(target), None) => target.to_string(),
        (None, _) => message.id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::Message;

    fn root(id: &str) -> Message {
        Message::root(id).with_list(id)
    }

    fn reply(id: &str, parent: &Message) -> Message {
        Message::root(id)
            .answering(parent.id.clone())
            .with_list(parent.list_id())
    }

    #[test]
    fn a_chain_shape_carries_only_the_parent() {
        let r = root("m0");
        let m1 = reply("m1", &r);
        let link = outbound(&m1, LinkShape::Chain, &[]);
        assert_eq!(link.parent.as_deref(), Some("m0"));
        // A chain carries the list implicitly: the chain of parents is it.
        assert_eq!(link.list, None);
        assert!(link.chain.is_empty());
    }

    #[test]
    fn a_flat_list_shape_names_the_root_and_the_parent() {
        let r = root("m0");
        let m1 = reply("m1", &r);
        let m2 = reply("m2", &m1);
        let link = outbound(&m2, LinkShape::FlatList, &[]);
        assert_eq!(link.parent.as_deref(), Some("m1"));
        // The whole chain shares one root, so the root is the same on all
        // three — which is what keeps `m0 → m1 → m2` one list.
        assert_eq!(link.list.as_deref(), Some("m0"));
    }

    #[test]
    fn a_headers_shape_spells_the_chain_from_the_root_down() {
        let r = root("m0");
        let m1 = reply("m1", &r);
        let m2 = reply("m2", &m1);
        let chain = ["m0".to_string(), "m1".to_string()];
        let link = outbound(&m2, LinkShape::Headers, &chain);
        assert_eq!(link.parent.as_deref(), Some("m1"));
        // The chain's head is the list, which is what an email subject names.
        assert_eq!(link.list.as_deref(), Some("m0"));
        assert_eq!(link.chain, chain);
    }

    #[test]
    fn a_flat_shape_carries_nothing_structural() {
        let r = root("m0");
        let m1 = reply("m1", &r);
        assert_eq!(outbound(&m1, LinkShape::Flat, &[]), OutboundLink::default());
    }

    #[test]
    fn a_reply_to_a_message_inside_its_own_list_stays_in_that_list() {
        // The Slack/Feishu case: `m2` answers an earlier sibling rather than
        // the newest, and the list does not change.
        let r = root("m0");
        let m1 = reply("m1", &r);
        let m2 = reply("m2", &m1);
        let back = Message::root("m3").answering("m1").with_list(m1.list_id());
        assert!(back.same_list_as(&m2));
        assert_eq!(back.list_id(), "m0");
    }

    #[test]
    fn only_the_platform_with_a_clock_has_a_window() {
        assert_eq!(
            passive_window("qqbot"),
            Some(std::time::Duration::from_secs(300))
        );
        assert_eq!(passive_window("feishu"), None);
        assert_eq!(passive_window("telegram"), None);
    }

    #[test]
    fn a_platform_names_its_own_shape() {
        assert_eq!(shape_of("qqbot"), LinkShape::Chain);
        assert_eq!(shape_of("feishu"), LinkShape::FlatList);
        // A platform nobody has measured carries nothing structural, which is
        // the reading that cannot send a link the platform cannot take.
        assert_eq!(shape_of("telegram"), LinkShape::Flat);
        assert_eq!(shape_of("a-platform-added-tomorrow"), LinkShape::Flat);
    }

    #[test]
    fn a_reference_moves_no_list() {
        let r = root("m0");
        let pointed = Message::root("m9").with_list("other");
        let sticker = reply("m8", &r).referencing("m9", crate::thread::RefKind::Reaction);
        assert_ne!(sticker.list_id(), pointed.list_id());
        assert_eq!(sticker.metadata.references.len(), 1);
        assert_eq!(
            sticker.metadata.references[0].kind,
            crate::thread::RefKind::Reaction
        );
    }

    #[test]
    fn an_inbound_list_folds_the_way_the_platform_reported_it() {
        let r = root("m0");
        let m1 = reply("m1", &r);
        // A platform that names the list: its word is the list.
        assert_eq!(
            assign_list(
                &m1,
                &InboundLink {
                    list: Some("t0".into()),
                    ..Default::default()
                },
                Some(&r)
            ),
            "t0"
        );
        // A platform that names only the parent: the parent's list, and the
        // parent's id when the parent is a root.
        assert_eq!(
            assign_list(
                &m1,
                &InboundLink {
                    reply_to: Some("m0".into()),
                    ..Default::default()
                },
                Some(&r)
            ),
            "m0"
        );
        let m2 = reply("m2", &m1);
        assert_eq!(
            assign_list(
                &m2,
                &InboundLink {
                    reply_to: Some("m1".into()),
                    ..Default::default()
                },
                Some(&m1)
            ),
            "m0"
        );
        // No link at all: the message is its own root.
        assert_eq!(assign_list(&m1, &InboundLink::default(), None), "m1");
    }
}
