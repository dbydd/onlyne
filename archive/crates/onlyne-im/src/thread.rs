//! A conversation as a message tree, with replies that form it and references
//! that only point.
//!
//! `docs/v2.4-REPLY-THREADS.md` is the design; this module is its code. Two
//! link kinds, and the difference is the whole model:
//!
//! * **reply** ([`Message::parent`]) is *structural*: it is what forms the
//!   tree. Every message belongs to exactly one **list** — the reply chain it
//!   hangs from — and a list is anchored at its root, so a message's list is
//!   the root of its reply chain, not its direct parent. That is what keeps
//!   `m0 → m1 → m2` in one list instead of forking at every hop.
//! * **reference** ([`Reference`]) is *not* structural: it points at any
//!   message anywhere in the conversation and moves nothing. A quote, a
//!   sticker's source, an edit's original, a forward — all of it rides in
//!   [`Metadata`], where a sticker and a reaction already live.
//!
//! A platform whose reply list is flat (Slack's thread, Feishu's reply group)
//! therefore needs no special case: every message in the list carries the same
//! root, and the ones that point at an earlier sibling are references rather
//! than replies. A platform whose reply link is a single forward pointer (QQ)
//! carries only the parent and lets the chain *be* the list, which is what
//! [`OutboundLink`] and [`LinkShape`] in `threadlink` encode.

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

/// A non-structural pointer to a message, carried as metadata.
///
/// Why this is not `parent`: a reply changes which list a message belongs to,
/// and a reference changes nothing. A sticker a user sends *in response to*
/// some message points at that message without joining its thread; a quote
/// inside a reply points at material the reply itself is not answering.
/// Modelling both as `parent` would fork a list for a sticker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    /// The message this points at, by the platform's own id.
    pub target: String,
    #[serde(default)]
    pub kind: RefKind,
}

/// What a reference is for. The core acts on none of it; it is what a backend
/// renders and what a human reading the record understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    /// Material quoted inside the message.
    #[default]
    Quote,
    /// A sticker or reaction sent because of that message.
    Reaction,
    /// A message forwarded on.
    Forward,
    /// A message edited by this one.
    Edit,
}

/// What a message carries beyond its text and its reply link.
///
/// The core reads none of it. References ride here because they are
/// non-structural; a sticker and a reaction ride here because they are not
/// text. Everything is skipped on the wire when empty, so an ordinary text
/// message records exactly the bytes it needs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Metadata {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Reference>,
    /// The sticker or reaction id the platform assigned it, when the message
    /// is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sticker: Option<String>,
}

impl Metadata {
    pub fn is_empty(&self) -> bool {
        self.references.is_empty() && self.sticker.is_none()
    }
}

/// One message in a conversation's tree.
///
/// `id` is the platform's own id for it, which is the only id that can name a
/// message when the message is sent back to the platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    /// The platform id of the sender, `None` for what this cluster sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<Participant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The message this one *replies to*: the structural link, which is what
    /// forms the tree. `None` for a root, which starts its own list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// The root of this message's reply chain — the list it belongs to.
    ///
    /// Recorded rather than derived, because a reply inherits its parent's
    /// list when the parent has one and *is* the list when the parent is a
    /// root: that is the ATProto `{root, parent}` pair, and it makes reading a
    /// message's list `O(1)` with no walk and no chance of disagreeing with
    /// the file that recorded it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list: Option<String>,
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub metadata: Metadata,
    /// The platform's handle for quoting this message back, when it hands out
    /// a second id for that purpose (QQ's `REFIDX_…`, delivered in the event
    /// that carried the message). None on a platform with one id per message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

impl Message {
    /// A root message: it replies to nothing and anchors its own list.
    pub fn root(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            from: None,
            text: None,
            parent: None,
            list: None,
            metadata: Metadata::default(),
            handle: None,
        }
    }

    /// The list this message belongs to.
    ///
    /// The root's own id when the message is a root, its recorded list
    /// otherwise, and its parent's id when it replies to something with no
    /// list of its own — a reply to a root joins that root's list.
    pub fn list_id(&self) -> &str {
        self.list
            .as_deref()
            .or(self.parent.as_deref())
            .unwrap_or(&self.id)
    }

    /// Attach the list this message belongs to.
    pub fn with_list(mut self, list: impl Into<String>) -> Self {
        self.list = Some(list.into());
        self
    }

    /// Attach the message this one replies to.
    pub fn answering(mut self, parent: impl Into<String>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    /// Point at a message without answering it.
    pub fn referencing(mut self, target: impl Into<String>, kind: RefKind) -> Self {
        self.metadata.references.push(Reference {
            target: target.into(),
            kind,
        });
        self
    }

    /// Whether this message sits in the list the newest message is in.
    ///
    /// A reply to a message of another list is a fork; a reference to one is
    /// not. This is the test a caller applies to "did this reply branch?", and
    /// it is a plain string compare because the list is recorded on both.
    pub fn same_list_as(&self, other: &Message) -> bool {
        self.list_id() == other.list_id()
    }
}

/// How many messages a delivery carries.
///
/// A window, not the whole history: the log on disk is the record and this is
/// the part a model reads. Twenty is enough for a back-and-forth and small
/// enough that the block is not the bulk of the delivery.
pub const DEFAULT_WINDOW: usize = 20;

/// The messages a delivery names, oldest first.
///
/// `root` is the conversation's stable key (its chat key), which scopes every
/// list id derived for it to this conversation.
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
    ///
    /// Prefix means *head of the id*. A mid-string match would make every
    /// platform's id prefix-free, and `ROBOT1.0_…` would then be namable by a
    /// fragment of its middle, which is not a prefix anybody reads.
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

    /// The messages of one list, oldest first.
    pub fn in_list(&self, list: &str) -> impl Iterator<Item = &Message> {
        self.messages.iter().filter(move |m| m.list_id() == list)
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

/// The branch id for one (conversation, anchor) pair: 12 hex characters of
/// `sha256` over the two ids.
///
/// `sha256` rather than `DefaultHasher`, whose seed is drawn per process and
/// would give one conversation a different branch id after every restart. The
/// id lands in the history record and in a backend's bookkeeping, so the same
/// pair must give the same string on every machine and every run. It is the
/// same function `onlyne-wire` derives a socket digest from, which is the
/// workspace's existing choice for a short stable id.
///
/// Twelve hex characters is 48 bits: enough to keep two branches of one
/// conversation apart (a birthday collision needs thousands), and short enough
/// to quote in a log line.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, parent: Option<&str>) -> Message {
        let mut m = Message::root(id);
        m.from = Some(Participant::new("u_1", Some("operator")));
        m.text = Some(format!("body of {id}"));
        if let Some(parent) = parent {
            m = m.answering(parent);
        }
        m
    }

    fn list() -> List {
        List {
            root: "7f3a9c2e".into(),
            participants: vec![Participant::new("u_1", Some("operator"))],
            messages: vec![
                message("ROBOT1.0_AdAXNfwe", None),
                message("0f3a9c2e1b04", Some("ROBOT1.0_AdAXNfwe")),
                message("a19b2c3d4e5f", Some("0f3a9c2e1b04")),
            ],
        }
    }

    #[test]
    fn a_chain_of_replies_is_one_list() {
        // The bug this model exists to prevent: anchoring at the immediate
        // parent makes every hop a new list, so `m0 → m1 → m2` reads as three
        // lists instead of one.
        let r = message("m0", None).with_list("m0");
        let m1 = message("m1", Some("m0")).with_list(r.list_id());
        let m2 = message("m2", Some("m1")).with_list(m1.list_id());
        assert_eq!(r.list_id(), "m0");
        assert_eq!(m1.list_id(), "m0");
        assert_eq!(m2.list_id(), "m0");
        assert!(m1.same_list_as(&m2));
    }

    #[test]
    fn a_reply_to_a_root_from_elsewhere_joins_that_roots_list() {
        // ATProto's `{root, parent}`: the reply names a parent and takes the
        // parent's root as its own list.
        let root = message("m0", None);
        let late = Message::root("m7")
            .answering("m0")
            .with_list(root.list_id());
        assert_eq!(
            late.list_id(),
            "m0",
            "a reply to a root joins the root's list"
        );
        assert!(late.same_list_as(&root));
    }

    #[test]
    fn a_root_anchors_its_own_list() {
        let root = message("m9", None);
        assert_eq!(root.list_id(), "m9");
        assert!(!root.same_list_as(&message("m8", None)));
    }

    #[test]
    fn a_reference_is_metadata_and_moves_no_list() {
        let sticker = message("m5", None).referencing("m1", RefKind::Reaction);
        assert_eq!(sticker.list_id(), "m5", "a reaction does not join a thread");
        assert_eq!(sticker.metadata.references.len(), 1);
        assert_eq!(sticker.metadata.references[0].target, "m1");
        assert!(!sticker.metadata.is_empty());
        assert!(message("m6", None).metadata.is_empty());
    }

    #[test]
    fn a_full_id_is_its_own_prefix_and_a_prefix_matches_the_head() {
        let list = list();
        assert_eq!(list.resolve("0f3a9c2e1b04").unwrap().id, "0f3a9c2e1b04");
        assert_eq!(list.resolve("ROBOT").unwrap().id, "ROBOT1.0_AdAXNfwe");
        assert_eq!(list.resolve("a19b").unwrap().id, "a19b2c3d4e5f");
        // A prefix is the head of the id, not any substring: `AdAX` sits in
        // the middle of `ROBOT1.0_AdAXNfwe` and names nothing.
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
        assert_eq!(window.messages[0].id, "tail-02");
        assert_eq!(window.latest().unwrap().id, "tail-21");
        // A dropped head is not resolvable any more, and the refusal says so.
        assert!(window.resolve("head-").is_err());
    }

    #[test]
    fn one_list_reads_out_of_the_window() {
        let mut mixed = List::new("k");
        mixed.messages.push(message("m0", None).with_list("m0"));
        mixed.messages.push(message("n0", None).with_list("n0"));
        mixed
            .messages
            .push(message("m1", Some("m0")).with_list("m0"));
        let in_m: Vec<&str> = mixed.in_list("m0").map(|m| m.id.as_str()).collect();
        assert_eq!(in_m, vec!["m0", "m1"]);
    }

    #[test]
    fn a_participant_without_a_name_is_still_addressable_by_id() {
        assert_eq!(Participant::new("u_9", None::<String>).label(), "u_9");
        assert_eq!(Participant::new("u_9", Some("operator")).label(), "operator");
    }

    #[test]
    fn branch_ids_are_stable_and_scoped_to_one_conversation() {
        let anchor = branch_id("k", "m1");
        assert_eq!(anchor, branch_id("k", "m1"));
        assert_ne!(anchor, branch_id("k2", "m1"));
        // The separator is written, not implied.
        assert_ne!(branch_id("ab", "c"), branch_id("a", "bc"));
        assert_eq!(anchor.len(), 12);
    }
}
