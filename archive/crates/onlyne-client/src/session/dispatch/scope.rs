//! Session scope: which deliveries one session of this role serves.
//!
//! Scope is a workspace setting of the role (`[client.session]`, plan §10) and
//! it takes effect here and nowhere else: the server delivers by role and keeps
//! zero orchestration, so the client is the only party that decides which of its
//! sessions takes a delivery.
//!
//! The three scopes and what they key on:
//!
//! * `oneshot` serves one delivery. A session is born with the delivery that
//!   opened it and closes when that delivery settles, so the resolution reaches
//!   no existing session at all and the key is the delivery's own task.
//! * `task` serves every delivery one task family sends this role, keyed on
//!   `causality.family` rather than on the task id: in planner → builder →
//!   reviewer → builder the second delivery to builder enters the session
//!   builder used the first time, which is the point of the scope.
//! * `role` serves a standing pool, at most `max_sessions` of them, and hands a
//!   delivery to whichever pooled session has waited longest.
//!
//! A session that is serving nothing right now is `idle`: it keeps its process
//! and its row, and it takes the next delivery the scope sends it. An idle
//! session whose runtime can resume may have its process released by this client
//! ([`super::DispatchState::suspend_idle_sessions`]) and is then `suspended`:
//! the conversation survives in the runtime's own store, the slot spends no
//! capacity, and the next delivery bound to that session resumes it.

use super::state::DispatchInner;
use super::*;
use onlyne_config::{SessionPolicy, SessionScope};

/// The idle bound a scope carries when `[client.session] idle_close` is absent.
///
/// Two hours is the value the plan prints beside the scope table (§10). It is a
/// real bound rather than an absent one because a session that can never be
/// released is a process held for the life of the client, and the scope table
/// says an idle `task` or `role` session closes on its idle timeout.
pub const DEFAULT_IDLE_CLOSE: Duration = Duration::from_secs(2 * 60 * 60);

/// The family one delivery belongs to.
///
/// `causality.family` is minted with the root task and carried by every handoff
/// of that run, so every delivery one family sends this role names the same
/// value. A root minted before the field existed names none, and `Causality::root`
/// writes the task itself, so the fallback is the delivery's own task: such a
/// root is its own family, which is what it was before the field existed.
pub(super) fn family_of(causality: &Causality) -> String {
    causality
        .family
        .clone()
        .unwrap_or_else(|| causality.task.clone())
}

/// The idle bound one policy carries, with the scope's own default filled in.
///
/// `None` is "never": a scope that opens one session per delivery has no idle
/// session to release at all, and `idle_close = 0` says the same for the other
/// two (the crate's other two duration knobs read zero the same way).
pub fn idle_bound(policy: &SessionPolicy) -> Option<Duration> {
    match policy.idle_close {
        Some(bound) if bound.is_zero() => None,
        Some(bound) => Some(bound),
        None => match policy.scope {
            SessionScope::Oneshot => None,
            SessionScope::Task | SessionScope::Role => Some(DEFAULT_IDLE_CLOSE),
        },
    }
}

/// Whether one scope keys its sessions on the task family.
pub(super) fn keys_on_family(scope: SessionScope) -> bool {
    matches!(scope, SessionScope::Task)
}

/// Whether one session can take a delivery without spending a new slot.
///
/// A session holding no delivery is one the scope kept idle for the work that
/// belongs to it, and a suspended session is one whose slot the client already
/// gave back. Both are places a delivery runs in: the difference is which one
/// the placement hands it to, not whether the role has room for it.
pub(super) fn takes_new_work(slot: &super::state::SessionSlot) -> bool {
    slot.suspended || slot.task_id.is_none()
}

/// Where one delivery of `family` can run, as the role's scope resolves it.
///
/// The answer is about scope alone: capacity, the intake gate, and whether the
/// session found here can actually be handed the payload are the caller's
/// questions, because each of them has an answer that is not a placement.
pub(super) enum Placement {
    /// The session at this key serves the delivery now.
    Bind(String),
    /// The session at this key holds the family's conversation with its process
    /// released: the delivery resumes it.
    Resume(String),
    /// No session of this role can take the delivery yet: the row waits for the
    /// one it belongs to, or for the pool to have room.
    Wait,
    /// No session of this role holds a conversation for the delivery: one is
    /// opened for it.
    Open,
}

/// Resolve one delivery against the sessions this client holds.
///
/// A suspended session is preferred over opening a new one in every scope that
/// reuses sessions at all: resuming is what the released conversation is for.
/// Within a scope that hands deliveries to a pool, the session that has waited
/// longest is the one that takes the work.
pub(super) fn placement(inner: &DispatchInner, family: &str) -> Placement {
    match inner.session_policy.scope {
        SessionScope::Oneshot => Placement::Open,
        SessionScope::Task => {
            let mut family_slots = inner
                .sessions
                .iter()
                .filter(|(_, slot)| slot.family.as_deref() == Some(family));
            // The family's own session, whichever state it is in, is the one
            // that serves this delivery. A session mid-delivery is the family
            // working: a second delivery of the same family waits for it rather
            // than opening a second conversation for one chain.
            let mut suspended = None;
            let mut busy = false;
            for (key, slot) in family_slots.by_ref() {
                if slot.suspended {
                    suspended = suspended.or(Some(key.clone()));
                } else if slot.task_id.is_none() {
                    return Placement::Bind(key.clone());
                } else {
                    busy = true;
                }
            }
            if busy {
                return Placement::Wait;
            }
            match suspended {
                Some(key) => Placement::Resume(key),
                None => Placement::Open,
            }
        }
        SessionScope::Role => {
            let mut idle: Vec<(&String, Instant)> = Vec::new();
            let mut suspended: Option<&String> = None;
            for (key, slot) in inner.sessions.iter() {
                if slot.suspended {
                    suspended = suspended.or(Some(key));
                    continue;
                }
                if slot.task_id.is_none() {
                    idle.push((key, slot.idle_since.unwrap_or(slot.opened_at)));
                }
            }
            // The pool hands out the session that has waited longest, so a role
            // under a steady stream of deliveries does not keep reheating one
            // conversation while the other pool member grows cold.
            idle.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(right.0)));
            if let Some((key, _)) = idle.first() {
                return Placement::Bind((*key).clone());
            }
            // The pool grows with the work: a delivery with no free member to
            // enter opens the next one, up to the role's `max_sessions` — and the
            // bound is what holds it back once there, which is the capacity
            // gate's answer, not this one. A suspended member is what a delivery
            // with nothing free resumes rather than opening another session.
            match suspended {
                Some(key) => Placement::Resume(key.clone()),
                None => Placement::Open,
            }
        }
    }
}
