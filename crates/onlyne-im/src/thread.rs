//! A conversation as a message list, with replies that name any message in it.
//!
//! `docs/v2.4-REPLY-THREADS.md` is the design; this module is its code. Three
//! things live here and nowhere else:
//!
//! * the **branch id** a message belongs to — derived from the link it answers,
//!   so a reply to an older message lands in its own branch and a reply to the
//!   newest stays in the current one;
//! * **unique prefix resolution** — a reply may name a message by any prefix of
//!   its id, and an ambiguous prefix is refused with the candidates rather than
//!   guessed at;
//! * the **participants** a conversation has seen, which is what a reply names
//!   when it addresses someone.
//!
//! None of this touches the core: the list is the gateway's own record, the
//! branch is a string, and a reply reaches the platform as the same
//! `reply_to` message id it carried before.

use serde::{Deserialize, Serialize};

/// One person in a conversation, by the platform's own id for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Participant {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Participant {
    pub fn new(id: impl Into<String>, name: Option<impl Into<String>>) -> Self {
        Self {
            id: id.into(),
            name: name.map(Into::into),
        }
    }

    /// What a delivery prints for this participant: the name when the platform
    /// gave one, the id otherwise. An unnamed id is still addressable.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

/// One message in a conversation's list.
///
/// `id` is the platform's own id for it, which is the only id that can name a
/// message when the message is sent back to the platform. `parent` is the id it
/// answers, absent for the first message in a branch — the shape a mailing
/// list's `In-Reply-To` has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    /// The platform id of the sender, `None` for what this cluster sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<Participant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The message this one answers; `None` for a root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// RFC 3339 UTC.
    pub at: String,
}

impl Message {
    /// The branch this message belongs to: its own id when it answers nothing,
    /// the parent's branch otherwise. `root` is the stable key of the
    /// conversation (its chat key), which keeps two conversations' branches
    /// apart without a shared registry.
    ///
    /// Deterministic and restart-stable: the same message in the same
    /// conversation derives the same branch on every machine and every run.
    pub fn branch(&self, root: &str) -> String {
        let anchor = self.parent.as_deref().unwrap_or(&self.id);
        branch_id(root, anchor)
    }
}

/// The branch id for one (conversation, anchor) pair: 12 hex characters of
/// `sha256` over the two ids.
///
/// `sha256` rather than `DefaultHasher`, whose seed is drawn per process and
/// would give one conversation a different branch id after every restart. The
/// id lands in the chat log and in a backend's bookkeeping, so the same pair
/// must give the same string on every machine and every run. It is the same
/// function `onlyne-wire` derives a socket digest from, which is the
/// workspace's existing choice for a short stable id.
///
/// Twelve hex characters is 48 bits: enough to keep two branches of one
/// conversation apart (a birthday collision needs thousands), and short
/// enough to quote in a log line.
///
/// Public because the history layout names its branch files by it: a writer
/// that holds a message record can name the file the branch lives in without
/// asking any registry.
pub fn branch_id(root: &str, anchor: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"onlyne-im/branch/");
    // The separator is written, not implied: `("ab", "c")` and `("a", "bc")`
    // must not derive the same branch.
    hasher.update(root.as_bytes());
    hasher.update(b"/");
    hasher.update(anchor.as_bytes());
    let digest = hasher.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(12);
    for byte in &digest[..6] {
        hex.push(HEX[(*byte >> 4) as usize] as char);
        hex.push(HEX[(*byte & 0x0f) as usize] as char);
    }
    hex
}

/// How many messages a delivery carries.
///
/// A window, not the whole history: the log on disk is the record and this is
/// the part a model reads. Twenty is enough for a back-and-forth and small
/// enough that the block is not the bulk of the delivery.
pub const DEFAULT_WINDOW: usize = 20;

/// The messages a delivery names, oldest first.
///
/// `root` is the conversation's stable key (its chat key) so branches derive
/// the same way on every call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    pub root: String,
    pub participants: Vec<Participant>,
    pub messages: Vec<Message>,
}

impl List {
    pub fn new(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            participants: Vec::new(),
            messages: Vec::new(),
        }
    }

    /// The window this list carries, oldest first, bounded to `window`.
    pub fn truncated(mut self, window: usize) -> Self {
        let excess = self.messages.len().saturating_sub(window);
        if excess > 0 {
            self.messages.drain(..excess);
        }
        self
    }

    /// The message a prefix names, or why it does not.
    ///
    /// A full id is its own prefix, so the long form is always accepted. The
    /// match is over the ids this list carries: a message the window has
    /// already scrolled past is not resolvable, which the error says plainly
    /// rather than falling back to the closest.
    pub fn resolve(&self, prefix: &str) -> Result<&Message, Unresolved> {
        let prefix = prefix.trim();
        if prefix.is_empty() {
            return Err(Unresolved::Empty);
        }
        let matches: Vec<&Message> = self
            .messages
            .iter()
            .filter(|message| message.id.starts_with(prefix))
            .collect();
        match matches.as_slice() {
            [] => Err(Unresolved::Absent {
                window: self.messages.iter().map(|m| m.id.clone()).collect(),
            }),
            [one] => Ok(one),
            many => Err(Unresolved::Ambiguous {
                candidates: many.iter().map(|m| m.id.clone()).collect(),
            }),
        }
    }

    /// The last message in the list, which is what a reply with no `<msg:…>`
    /// line answers.
    pub fn latest(&self) -> Option<&Message> {
        self.messages.last()
    }

    /// The participants of this conversation, in first-seen order.
    pub fn participants(&self) -> &[Participant] {
        &self.participants
    }
}

/// Why [`List::resolve`] found no message.
///
/// Each variant carries what a caller needs to write a better prefix, because
/// the reader of the refusal is a model that will try again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unresolved {
    /// The prefix is empty. `None` resolves to the latest message instead, so
    /// an empty prefix is never sent here by the gateway.
    Empty,
    /// Nothing in the window starts with this prefix.
    Absent { window: Vec<String> },
    /// More than one message starts with it.
    Ambiguous { candidates: Vec<String> },
}

impl std::fmt::Display for Unresolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "the reply names no message"),
            Self::Absent { window } => write!(
                f,
                "no message in the last {} starts with that prefix; the list carries [{}]",
                window.len(),
                window.join(", ")
            ),
            Self::Ambiguous { candidates } => write!(
                f,
                "that prefix matches more than one message: [{}]; write a longer one",
                candidates.join(", ")
            ),
        }
    }
}

impl std::error::Error for Unresolved {}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, parent: Option<&str>) -> Message {
        Message {
            id: id.into(),
            from: Some(Participant::new("u_1", Some("dbydd"))),
            text: Some(format!("body of {id}")),
            parent: parent.map(str::to_owned),
            at: "2026-10-14T00:00:00.000Z".into(),
        }
    }

    fn list() -> List {
        List {
            root: "7f3a9c2e".into(),
            participants: vec![Participant::new("u_1", Some("dbydd"))],
            messages: vec![
                message("ROBOT1.0_AdAXNfwe", None),
                message("0f3a9c2e1b04", Some("ROBOT1.0_AdAXNfwe")),
                message("a19b2c3d4e5f", Some("0f3a9c2e1b04")),
            ],
        }
    }

    #[test]
    fn a_root_gets_its_own_branch_and_a_reply_gets_its_parents() {
        let root = message("ROBOT1.0_AdAXNfwe", None);
        let reply = message("0f3a9c2e1b04", Some("ROBOT1.0_AdAXNfwe"));
        // Same anchor: the reply answers the root, so both sit in the root's
        // branch.
        assert_eq!(root.branch("k"), reply.branch("k"));
        let deeper = message("a19b2c3d4e5f", Some("0f3a9c2e1b04"));
        assert_ne!(deeper.branch("k"), root.branch("k"));
    }

    #[test]
    fn branches_are_stable_and_scoped_to_one_conversation() {
        let anchor = message("m1", None);
        let again = message("m1", None);
        assert_eq!(anchor.branch("k"), again.branch("k"));
        // The same id in another conversation must not share a branch.
        assert_ne!(anchor.branch("k1"), anchor.branch("k2"));
    }

    #[test]
    fn a_branch_id_matches_what_the_message_computes() {
        let root = message("m-root", None);
        let reply = message("m-reply", Some("m-root"));
        assert_eq!(reply.branch("k"), root.branch("k"));
        assert_eq!(branch_id("k", "m-root"), root.branch("k"));
    }

    #[test]
    fn a_full_id_is_its_own_prefix() {
        let list = list();
        let found = list.resolve("0f3a9c2e1b04").expect("the full id resolves");
        assert_eq!(found.id, "0f3a9c2e1b04");
    }

    #[test]
    fn a_unique_prefix_resolves() {
        let list = list();
        assert_eq!(list.resolve("ROBOT").unwrap().id, "ROBOT1.0_AdAXNfwe");
        assert_eq!(list.resolve("a19b").unwrap().id, "a19b2c3d4e5f");
        // A prefix is the head of the id, not any substring: this is what
        // makes `AdAX` a *longer* prefix rather than a mid-string match.
        assert!(list.resolve("AdAX").is_err());
    }

    #[test]
    fn an_ambiguous_prefix_is_refused_with_its_candidates() {
        let mut many = list();
        many.messages.push(message("0f3a9c2e0000", None));
        let error = many.resolve("0f3a").expect_err("two share the prefix");
        match &error {
            Unresolved::Ambiguous { candidates } => {
                assert_eq!(
                    candidates,
                    &vec!["0f3a9c2e1b04".to_string(), "0f3a9c2e0000".to_string()]
                );
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
        // The sentence a model reads names every candidate.
        let said = error.to_string();
        assert!(
            said.contains("0f3a9c2e1b04") && said.contains("0f3a9c2e0000"),
            "{said}"
        );
    }

    #[test]
    fn a_prefix_nothing_matches_says_the_window_it_looked_at() {
        let error = list().resolve("zzz").expect_err("nothing matches");
        let said = error.to_string();
        assert!(said.contains("ROBOT1.0_AdAXNfwe"), "{said}");
        assert_eq!(
            error,
            Unresolved::Absent {
                window: vec![
                    "ROBOT1.0_AdAXNfwe".into(),
                    "0f3a9c2e1b04".into(),
                    "a19b2c3d4e5f".into(),
                ],
            }
        );
    }

    #[test]
    fn an_empty_prefix_resolves_to_nothing_without_guessing() {
        assert_eq!(list().resolve("  "), Err(Unresolved::Empty));
    }

    #[test]
    fn the_window_keeps_the_tail_and_drops_the_head() {
        let mut long = List::new("k");
        long.messages.push(message("head-1", None));
        for i in 0..(DEFAULT_WINDOW + 2) {
            long.messages.push(message(&format!("tail-{i:02}"), None));
        }
        let window = long.truncated(DEFAULT_WINDOW);
        assert_eq!(window.messages.len(), DEFAULT_WINDOW);
        // The head is gone, the tail is complete, and the newest is last.
        assert_eq!(window.messages[0].id, "tail-02");
        assert_eq!(window.latest().unwrap().id, "tail-21");
        // A dropped head is not resolvable any more — the refusal says so.
        assert!(window.resolve("head-").is_err());
    }

    #[test]
    fn a_participant_without_a_name_is_still_addressable_by_id() {
        let quiet = Participant::new("u_9", None::<String>);
        assert_eq!(quiet.label(), "u_9");
        let named = Participant::new("u_9", Some("dbydd"));
        assert_eq!(named.label(), "dbydd");
    }
}
