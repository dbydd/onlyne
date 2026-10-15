//! The chain shape's fork rule: when a plain reply becomes a branch.
//!
//! QQ has no thread and no reply list. A message carries at most the id of the
//! one it answers (`msg_id` — the same field its passive reply needs), which is
//! one forward pointer, and that is [`LinkShape::Chain`]: the chain of those
//! pointers *is* the list.
//!
//! [`LinkShape::Chain`]: crate::threadlink::LinkShape::Chain
//!
//! A flat list (Slack, Feishu) tells us where the list ends; a chain does not,
//! so a rule has to decide it, and "every reply forks" would make a plain
//! back-and-forth one branch per message. The rule, decided with the operator
//! on 2026-10-14:
//!
//! * **A reply is a reference.** It points at the message it answers and rides
//!   outbound as nothing but that link, which is all QQ can take.
//! * **A branch opens once two messages of the same conversation have answered
//!   each other.** `a ← b` is one hop and no branch; `b ← c` is the second, and
//!   *that* reply forks. The count comes from the window the gateway already
//!   holds, so a reader of the same log can check it by hand.
//!
//! The rule is a pure function over recorded messages: the gateway asks it
//! before recording an outbound reply, and no backend derives anything.

use crate::thread::{List, Message};

/// How many hops along one chain must already exist before a reply forks.
///
/// Two, decided with the operator: one hop is a plain reply, and the second is
/// where a back-and-forth has visibly become its own conversation.
pub const FORK_AFTER: usize = 2;

/// Whether a reply that answers `parent` opens a branch.
///
/// `window` is what the gateway holds for this conversation, oldest first. The
/// count starts at the parent and walks *up* the chain of replies inside that
/// window: the hop onto a root is the first, the hop that answers a message
/// which itself answered something is the second.
///
/// A chain that walks out of the window is read the plain way — no branch. The
/// alternative is a fork nobody can reproduce from the log, and a missing
/// branch costs a flat discussion where an extra one costs a split thread.
pub fn forks(window: &List, parent: &str) -> bool {
    let mut hops = 0usize;
    let mut at = Some(parent.to_string());
    while let Some(id) = at {
        let Some(message) = find(window, &id) else {
            // The chain left the window; `hops` counts what was verifiable.
            break;
        };
        hops += 1;
        if hops >= FORK_AFTER {
            return true;
        }
        at = message.parent.clone();
    }
    false
}

/// Record one outbound reply: into the aggregate always, and into a discussion
/// file of its own when this reply is the one that opened the branch.
///
/// `history` is where the role keeps both. The discussion file starts at the
/// message that was answered — the fork origin — which is what makes reading
/// one branch reading one file, and `fork_of` on its first row is what points
/// a reader back at the aggregate row it came from.
///
/// A reply that does not fork is one row in the aggregate and nothing else:
/// most traffic is that, and paying a file per message would make the record
/// harder to read rather than easier.
pub fn record_reply(
    history: &crate::history::History,
    entry: &mut crate::history::HistoryEntry,
    window: &List,
    parent: Option<&str>,
) -> bool {
    let parent = match parent {
        Some(parent) => parent,
        None => {
            // Nothing answered: no fork and no discussion file. The list would
            // be this message's own id, which the gateway does not know until
            // the platform answers the send — so the row records without one
            // rather than with a placeholder that names nothing.
            entry.list = entry.platform_message.clone().filter(|id| !id.is_empty());
            let _ = history.append_main(entry);
            return false;
        }
    };
    let forked = forks(window, parent);
    let answered = find(window, parent);
    let list = match answered {
        // A reply inherits its parent's list: that is what keeps a chain one
        // discussion instead of one per hop.
        Some(parent_message) if parent_message.list.is_some() => {
            parent_message.list.clone().unwrap()
        }
        Some(parent_message) => parent_message.id.clone(),
        None => parent.to_string(),
    };
    entry.list = Some(list.clone());
    if forked {
        let chat_key = entry.chat_key.clone().unwrap_or_default();
        entry.fork_of = Some(crate::history::Fork {
            chat_key,
            platform_message: parent.to_string(),
        });
    } else {
        entry.fork_of = None;
    }
    let _ = history.append_main(entry);
    if forked {
        let _ = history.append_session(entry);
    }
    forked
}

/// Record one inbound event in the role's history, in its list.
///
/// The inbound half of the same rule: a message lands in the list of what it
/// answers (the platform's coordinate, when it reports one) or anchors a list
/// of its own. Recording it is what gives a later reply a chain to count, so
/// the fork rule has something to read.
///
/// `coordinate` is `ImEvent.reply`'s anchor, already resolved by the caller:
/// the root when the platform names one, else the parent — a reply into a
/// discussion this cluster never recorded lands in a list named by the message
/// it answered, which is how a record starts from the middle.
pub fn record_inbound(
    history: &crate::history::History,
    event: &onlyne_im_backend::payload::ImEvent,
    chat_key: &str,
    coordinate: Option<(&str, Option<&str>)>,
) -> String {
    let mut row = crate::history::HistoryEntry::inbound(chat_key, &event.platform, &event.id);
    row.conversation = Some(event.conversation.clone());
    row.text = event.text.clone();
    row.ref_handle = event.reply.as_ref().and_then(|r| r.self_handle.clone());
    row.parent = coordinate.and_then(|(_, parent)| parent.map(str::to_owned));
    let list = match coordinate {
        // A known anchor: the platform named a root, so that root *is* the list.
        Some((anchor, _)) if !anchor.is_empty() => crate::thread::branch_id(chat_key, anchor),
        // No coordinate: the message anchors its own list.
        _ => crate::thread::branch_id(chat_key, &event.id),
    };
    row.list = Some(list.clone());
    if let Err(error) = history.append_main(&row) {
        tracing::warn!(error = %error, "history write failed");
    }
    list
}

/// The window of one conversation's messages, oldest first, rebuilt from the
/// role's aggregate record.
///
/// The record is the source; the window is a read of it, bounded by
/// [`crate::thread::DEFAULT_WINDOW`]. Rows of other conversations are skipped,
/// because one role's aggregate carries every channel.
pub fn window_of(history: &crate::history::History, chat_key: &str) -> List {
    let mut list = List::new(chat_key);
    for row in history.recent(crate::thread::DEFAULT_WINDOW) {
        if row.chat_key.as_deref() != Some(chat_key) {
            continue;
        }
        let Some(id) = row.platform_message.clone().filter(|id| !id.is_empty()) else {
            continue;
        };
        let mut message = crate::thread::Message::root(id);
        message.parent = row.parent.clone();
        message.list = row.list.clone();
        message.text = row.text.clone();
        message.handle = row.ref_handle.clone();
        list.messages.push(message);
    }
    list
}

/// The message one id names, by either id the platform handed out.
///
/// A platform can address one message two ways and use them in different
/// places: QQ's message id is a `ROBOT1.0_…` and its quote handle is a
/// `REFIDX_…`, and an inbound quote names the message it answers by the
/// **handle** while the record keys messages by their **id**. Looking up only
/// the id leaves every such chain one link short, which reads as "this reply
/// opens nothing" — the failure is silent and the branch simply never appears.
/// Measured 2026-10-15: an inbound quote recorded `parent = REFIDX_…` against
/// a window of `ROBOT1.0_…` ids.
fn find<'a>(window: &'a List, id: &str) -> Option<&'a Message> {
    window
        .messages
        .iter()
        .find(|m| m.id == id || m.handle.as_deref() == Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `a ← b ← c`, all in one chain, oldest first.
    fn window() -> List {
        let mut list = List::new("k");
        list.messages = vec![
            Message::root("a").with_list("la"),
            Message::root("b").answering("a").with_list("la"),
            Message::root("c").answering("b").with_list("la"),
        ];
        list
    }

    #[test]
    fn the_first_hop_onto_a_root_is_a_plain_reply() {
        // `a` has answered nothing, so a reply to it is hop one.
        assert!(!forks(&window(), "a"));
    }

    #[test]
    fn the_second_hop_opens_a_branch() {
        // `b` answered `a`, so a reply to `b` is hop two.
        assert!(forks(&window(), "b"));
        // And the third is still inside the branch it opened.
        assert!(forks(&window(), "c"));
    }

    #[test]
    fn a_chain_that_left_the_window_forks_nothing() {
        assert!(!forks(&window(), "long-gone"));
    }

    #[test]
    fn two_messages_of_one_conversation_are_what_the_count_counts() {
        // The same two ids in a window that does not carry `a` cannot form the
        // count: the rule is over what the gateway holds.
        let mut partial = List::new("k");
        partial.messages = vec![Message::root("c").answering("b").with_list("lc")];
        // `c` answered `b` — one hop this window can see. Whether `b` answered
        // anything is not in the window, so the second hop is unverifiable and
        // the reply is read the plain way.
        assert!(!forks(&partial, "b"), "one hop in this window, not two");
        assert!(
            !forks(&partial, "c"),
            "c's own parent is out of the window, so its chain is unverifiable"
        );
    }

    #[test]
    fn a_reply_that_forks_opens_a_discussion_file_and_one_that_does_not_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let history = crate::history::History::new(dir.path());
        let window = window();

        // A reply to `a` is one hop: aggregate only.
        let mut first =
            crate::history::HistoryEntry::outbound("k", "qqbot", Some("a")).with_text("hello");
        first.platform_message = Some("d".into());
        assert!(!record_reply(&history, &mut first, &window, Some("a")));
        assert_eq!(history.recent(10).len(), 1);
        assert!(
            !history
                .session_path(first.list.as_deref().unwrap())
                .exists(),
            "a plain reply opens no discussion file"
        );

        // A reply to `c`, whose chain is three deep, forks.
        let mut second =
            crate::history::HistoryEntry::outbound("k", "qqbot", Some("c")).with_text("again");
        second.platform_message = Some("e".into());
        assert!(record_reply(&history, &mut second, &window, Some("c")));
        let list = second.list.clone().unwrap();
        let path = history.session_path(&list);
        assert!(path.is_file(), "the fork opens its own file");
        assert_eq!(
            crate::history::fork_of(&path).unwrap().platform_message,
            "c",
            "the discussion starts at the message that was answered"
        );
        assert_eq!(history.session(&list).len(), 1);
        // Both sides: the aggregate carries it too.
        assert_eq!(history.recent(10).len(), 2);
    }

    #[test]
    fn a_reply_with_no_parent_records_no_fork() {
        let dir = tempfile::tempdir().unwrap();
        let history = crate::history::History::new(dir.path());
        let mut entry = crate::history::HistoryEntry::outbound("k", "qqbot", None);
        entry.platform_message = Some("z".into());
        assert!(!record_reply(&history, &mut entry, &window(), None));
        assert!(entry.fork_of.is_none());
        assert_eq!(history.recent(10).len(), 1);
    }

    #[test]
    fn the_constant_is_the_rule_the_operator_set() {
        assert_eq!(FORK_AFTER, 2);
    }

    #[test]
    fn a_parent_named_by_quote_handle_still_walks_the_chain() {
        // The live QQ shape: the replied-to message is named by its `REFIDX_…`
        // handle, while the window holds `ROBOT1.0_…` ids.
        let mut window = List::new("k");
        let mut a = Message::root("ROBOT1.0_a").with_list("la");
        a.handle = Some("REFIDX_a".into());
        let mut b = Message::root("ROBOT1.0_b")
            .answering("REFIDX_a")
            .with_list("la");
        b.handle = Some("REFIDX_b".into());
        window.messages = vec![a, b];
        // A reply to `b` is hop two: `b` answered `a`.
        assert!(forks(&window, "REFIDX_b"), "the handle must resolve");
        assert!(forks(&window, "ROBOT1.0_b"), "and so must the id");
        // A reply to `a` is hop one.
        assert!(!forks(&window, "REFIDX_a"));
    }
}
