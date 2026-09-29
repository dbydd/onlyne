//! The orderings a page draws and its selection steps.
//!
//! A page and the keys that move inside it must agree on what "the next row" is
//! or the highlighted row is not the row an op acts on. Every list the three
//! pages show is read here once, in the order it is drawn, and both
//! [`crate::tui::update`] and the page renderers call these. The `View`'s maps
//! are keyed by each row's own id, so "the order it is drawn" is this module's
//! decision and nowhere else's.

use onlyne_proto::view::{Card, View};
use onlyne_proto::{DeliveryView, Event, FaultEvent, RoleInfo};

/// The roles, in registry order.
pub fn roles(view: &View) -> Vec<&RoleInfo> {
    view.roles.values().collect()
}

/// One role's cards, in the order the board draws them: by column, then down
/// the family's hops, then by the delivery's own key.
///
/// Column first is the plan's board: a reader who looks at one column sees the
/// work sitting in it. The hop inside a column is what makes a family's path
/// readable when several deliveries of it land on one role.
pub fn cards<'a>(view: &'a View, role: &'a str) -> Vec<Card<'a>> {
    let mut cards: Vec<Card<'a>> = view.cards(role).collect();
    cards.sort_by_key(|card| {
        (
            card.column(),
            card.delivery.hop.unwrap_or_default(),
            card.delivery.msg_id.clone(),
        )
    });
    cards
}

/// Every task family the view holds, in id order.
///
/// A family is named by the deliveries that carry it, so a family exists here
/// exactly when some delivery names one.
pub fn families(view: &View) -> Vec<String> {
    view.deliveries
        .values()
        .filter_map(|delivery| delivery.family.clone())
        .collect::<std::collections::BTreeSet<String>>()
        .into_iter()
        .collect()
}

/// One family's deliveries, in the order the task page draws them: by hop, so
/// the page reads as the arc the plan describes.
pub fn family_deliveries<'a>(view: &'a View, family: &'a str) -> Vec<&'a DeliveryView> {
    let mut deliveries: Vec<&DeliveryView> = view.family(family).collect();
    deliveries.sort_by_key(|delivery| (delivery.hop.unwrap_or_default(), delivery.msg_id.clone()));
    deliveries
}

/// The open faults, oldest first, so a repair the operator just made stays
/// where the eye left it.
pub fn open_faults(view: &View) -> Vec<&FaultEvent> {
    let mut faults: Vec<&FaultEvent> = view.open_faults().collect();
    faults.sort_by_key(|fault| fault.id);
    faults
}

/// The lines a scrollable pane shows, from `scroll` lines back off the newest.
///
/// Both the event tail and a session's log are newest first, which is what
/// makes one offset serve both: the operator scrolls back through what just
/// happened.
pub fn window(lines: &[Event], scroll: usize, height: usize) -> &[Event] {
    let end = lines.len().saturating_sub(scroll);
    let start = end.saturating_sub(height.max(1));
    &lines[start..end]
}
