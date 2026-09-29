//! The boards the browser renders, read off the reducer's own [`View`].
//!
//! Every reading here is a call into `onlyne_proto::view` — [`View::cards`],
//! [`Card::column`], [`View::counts`] — so the columns, the counts, and the
//! operator's board are the reducer's readings, serialized for the wire. The
//! browser receives this and draws it; it folds nothing, which is the "one
//! model, two renderers" split the plan draws (`docs/v2-PLAN.md` line 357).
//!
//! The one board the spec may not declare is the operator's: sends from the
//! GUI are `_supervisor` sends and their receipts land in `_supervisor`'s
//! queue, so that queue is rendered as a board of its own (`spec.rs:39` names
//! the reserved role).

use onlyne_proto::view::Card;
use onlyne_proto::view::{BoardColumn, SessionCounts, View};
use onlyne_proto::{LedgerState, MsgKind, Outcome, Presence, RoleInfo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The reserved role the operator's board renders, whatever the spec declares.
/// Spelled here rather than imported because `onlyne-config` is not one of
/// this crate's dependencies; `onlyne_config::spec::SUPERVISOR_ROLE` is the
/// owner of the word.
pub const OPERATOR_ROLE: &str = "_supervisor";

/// One card: a delivery on this board's role, arranged by the reducer's joint
/// reading of the two axes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct BoardCard {
    pub msg_id: String,
    pub kind: MsgKind,
    /// The sending role, for the card's route line.
    pub from: Option<String>,
    pub task_id: Option<String>,
    /// The family the card is strung across boards by.
    pub family: Option<String>,
    pub hop: Option<u32>,
    /// The joint column: the one place the two axes are read together.
    pub column: BoardColumn,
    /// The ledger's own word, for the card's state chip.
    pub state: LedgerState,
    pub outcome: Option<Outcome>,
    pub out_head: Option<String>,
}

/// One board: a role's header counts and its cards.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Board {
    pub role: String,
    /// True for the operator's board, which exists whether or not the spec
    /// declares `_supervisor`.
    pub operator: bool,
    pub admin: bool,
    pub presence: Presence,
    /// The deliveries queued for this role's inbox, from the `roles` read.
    pub queued: u32,
    /// The header's busy / idle / suspended counts.
    pub counts: SessionCounts,
    pub cards: Vec<BoardCard>,
    /// The role's declared outbound routes, verbatim: the graph's edges.
    pub edges: Vec<String>,
}

/// Every board one view renders, in the `roles` read's order.
///
/// The operator's board is last, so a cluster that declares `_supervisor`
/// itself does not get two.
pub fn boards(view: &View) -> Vec<Board> {
    let mut boards: Vec<Board> = view.roles.values().map(|role| board(view, role)).collect();
    if !view.roles.contains_key(OPERATOR_ROLE) {
        boards.push(Board {
            role: OPERATOR_ROLE.to_string(),
            operator: true,
            admin: true,
            presence: Presence::Offline,
            queued: 0,
            counts: view.counts(OPERATOR_ROLE),
            cards: cards(view, OPERATOR_ROLE),
            edges: Vec::new(),
        });
    }
    boards
}

/// One role's board, its cards and counts read through the reducer's API.
fn board(view: &View, role: &RoleInfo) -> Board {
    Board {
        role: role.name.clone(),
        operator: role.name == OPERATOR_ROLE,
        admin: role.admin,
        presence: role.state,
        queued: role.queued,
        counts: view.counts(&role.name),
        cards: cards(view, &role.name),
        edges: role.edges.clone(),
    }
}

/// One role's cards, each with the column the reducer assigns it.
fn cards(view: &View, role: &str) -> Vec<BoardCard> {
    view.cards(role)
        .map(|card: Card| BoardCard {
            msg_id: card.delivery.msg_id.clone(),
            kind: card.delivery.kind,
            from: card.delivery.from.role_name().map(str::to_string),
            task_id: card.delivery.task_id.clone(),
            family: card.delivery.family.clone(),
            hop: card.delivery.hop,
            column: card.column(),
            state: card.delivery.state,
            outcome: card
                .delivery
                .outcome
                .or(card.session.and_then(|s| s.outcome)),
            out_head: card.delivery.out_head.clone(),
        })
        .collect()
}
