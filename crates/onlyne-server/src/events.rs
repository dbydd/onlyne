//! Observation plane: bounded broadcast, cursor replay, and lag notices
//! (plan §4 line 214, §9).
//!
//! Every event the server emits is persisted in the `events` table and pushed
//! on a bounded broadcast. A subscriber that falls behind is told how many
//! events it missed so it can re-subscribe with `since_seq` from a known point.

use crate::state::{Server, State};
use anyhow::Context;
use chrono::{DateTime, Utc};
use onlyne_proto::{Event, EventRow, EventTier, Frame, HistoryArgs, Subscribe};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

/// Event name carried by the notice a lagging subscriber receives.
pub const RESYNC_LAG_KIND: &str = "resync_lag";

/// Default page size for a replay or history read.
pub const DEFAULT_REPLAY_LIMIT: u32 = 256;

/// Subscription filter built from a `subscribe` request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventFilter {
    pub kinds: Vec<String>,
    pub tiers: Vec<EventTier>,
    pub roles: Vec<String>,
}

impl EventFilter {
    pub fn from_subscribe(subscribe: &Subscribe) -> Self {
        EventFilter {
            kinds: subscribe.kinds.clone(),
            tiers: subscribe.tiers.clone(),
            roles: subscribe.roles.clone(),
        }
    }

    /// Whether one event passes every requested restriction.
    pub fn matches(&self, event: &Event) -> bool {
        if !self.tiers.is_empty() && !self.tiers.contains(&event.tier()) {
            return false;
        }
        if !self.kinds.is_empty() && !self.kinds.iter().any(|kind| kind == event.type_name()) {
            return false;
        }
        if !self.roles.is_empty() {
            let named = event_roles(event);
            if !named
                .iter()
                .any(|role| self.roles.iter().any(|wanted| wanted == role))
            {
                return false;
            }
        }
        true
    }
}

/// The roles an event names, empty for cluster-wide events.
pub fn event_roles(event: &Event) -> Vec<&str> {
    match event {
        Event::RolePresence(presence) => vec![presence.role.as_str()],
        Event::SessionState(session) => vec![session.role.as_str()],
        Event::LedgerState(ledger) => {
            let mut roles = Vec::new();
            if let Some(role) = ledger.from.role_name() {
                roles.push(role);
            }
            if let Some(role) = ledger.to.role_name() {
                roles.push(role);
            }
            roles
        }
        Event::Fault(fault) => fault.role.as_deref().into_iter().collect(),
        Event::GatewayPresence { gateway, .. } => vec![gateway.as_str()],
        Event::SpecReloaded(_) => Vec::new(),
    }
}

/// Notice handed to a subscriber whose cursor fell behind the retained window.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResyncNotice {
    pub kind: String,
    /// Events the subscriber missed between its cursor and the retained window.
    pub dropped: u64,
    pub head: u64,
    pub requested: u64,
}

impl ResyncNotice {
    pub fn new(dropped: u64, head: u64, requested: u64) -> Self {
        ResyncNotice {
            kind: RESYNC_LAG_KIND.to_string(),
            dropped,
            head,
            requested,
        }
    }
}

/// One replay page: the rows, the server head, and the lag notice when any.
#[derive(Debug, Clone)]
pub struct ReplayPage {
    pub rows: Vec<EventRow>,
    pub head: u64,
    /// Cursor the caller asked to resume from.
    pub requested: u64,
    pub notice: Option<ResyncNotice>,
}

/// Notice for a gap wider than the configured resync bound.
///
/// `rows` are the persisted rows at or after the cursor; a jump in their `seq`
/// values means the events between them were dropped from the table.
pub fn gap_notice(
    since_seq: u64,
    rows: &[EventRow],
    head: u64,
    resync_lag: u64,
) -> Option<ResyncNotice> {
    let mut dropped = 0u64;
    let mut expected = since_seq.saturating_add(1);
    for row in rows {
        if row.seq > expected {
            dropped = dropped.saturating_add(row.seq - expected);
        }
        expected = row.seq.saturating_add(1);
    }
    if dropped > resync_lag {
        Some(ResyncNotice::new(dropped, head, since_seq))
    } else {
        None
    }
}

/// Read persisted events after `since_seq`, filtered by `filter`.
pub fn replay(
    state: &State,
    since_seq: u64,
    filter: &EventFilter,
    limit: u32,
) -> anyhow::Result<ReplayPage> {
    let head = state.event_head().max(0) as u64;
    let wanted = if limit == 0 {
        DEFAULT_REPLAY_LIMIT
    } else {
        limit
    };
    let records = state
        .ledger
        .events_since(since_seq.min(i64::MAX as u64) as i64, wanted)
        .context("read the events table")?;
    let mut rows = Vec::with_capacity(records.len());
    for record in records {
        let event: Event =
            serde_json::from_value(record.data.clone()).context("decode a persisted event")?;
        if !filter.matches(&event) {
            continue;
        }
        rows.push(EventRow {
            seq: record.seq.max(0) as u64,
            created_at: parse_created_at(&record.created_at),
            event,
        });
    }
    let resync_lag = state
        .spec_snapshot()
        .map(|spec| u64::from(spec.server.resync_lag))
        .unwrap_or(256);
    let notice = gap_notice(since_seq, &rows, head, resync_lag);
    Ok(ReplayPage {
        rows,
        head,
        requested: since_seq,
        notice,
    })
}

/// The page a `subscribe` request receives.
pub fn page_for(state: &State, subscribe: &Subscribe) -> anyhow::Result<ReplayPage> {
    let filter = EventFilter::from_subscribe(subscribe);
    replay(state, subscribe.since_seq, &filter, DEFAULT_REPLAY_LIMIT)
}

/// The page an admin `history` request receives.
pub fn history(state: &State, args: &HistoryArgs) -> anyhow::Result<ReplayPage> {
    let filter = EventFilter {
        kinds: args.kind.iter().cloned().collect(),
        tiers: Vec::new(),
        roles: Vec::new(),
    };
    let mut page = replay(state, args.since_seq, &filter, args.limit)?;
    if let Some(task_id) = &args.task_id {
        page.rows
            .retain(|row| event_task(&row.event).as_deref() == Some(task_id.as_str()));
    }
    Ok(page)
}

/// The task an event names, when it names one.
pub fn event_task(event: &Event) -> Option<String> {
    match event {
        Event::SessionState(session) => Some(session.task_id.clone()),
        Event::LedgerState(ledger) => ledger.task.clone(),
        Event::Fault(fault) => fault.task_id.clone(),
        Event::RolePresence(_) | Event::GatewayPresence { .. } | Event::SpecReloaded(_) => None,
    }
}

/// Persist and broadcast one event.
pub fn publish(state: &Server, event: Event) -> anyhow::Result<u64> {
    state.emit(event)
}

/// Serialise a replay page for a response body.
pub fn page_json(page: &ReplayPage) -> serde_json::Value {
    serde_json::json!({
        "since_seq": page.requested,
        "head": page.head,
        "count": page.rows.len(),
        "events": page.rows,
        "resync_lag": page.notice,
    })
}

/// One step of a live subscription.
#[derive(Debug, Clone, PartialEq)]
pub enum LiveStep {
    Deliver(Arc<Frame>),
    /// The subscriber missed `dropped` events; it must re-subscribe.
    Lagged(u64),
    Closed,
}

/// Classify one broadcast receive without hiding a lag.
pub fn step(result: Result<Arc<Frame>, broadcast::error::RecvError>) -> LiveStep {
    match result {
        Ok(frame) => LiveStep::Deliver(frame),
        Err(broadcast::error::RecvError::Lagged(dropped)) => LiveStep::Lagged(dropped),
        Err(broadcast::error::RecvError::Closed) => LiveStep::Closed,
    }
}

/// Forward broadcast events into one connection until it lags or closes.
///
/// A lag ends the forwarder so the subscriber must re-subscribe and receive the
/// `resync_lag` notice with its drop count.
pub fn spawn_forwarder(
    state: &Arc<State>,
    filter: EventFilter,
    sender: mpsc::Sender<Frame>,
) -> tokio::task::JoinHandle<()> {
    let mut receiver = state.subscribe_frames();
    tokio::spawn(async move {
        loop {
            match step(receiver.recv().await) {
                LiveStep::Deliver(frame) => {
                    if let Frame::Ev { event, .. } = frame.as_ref() {
                        if !filter.matches(event) {
                            continue;
                        }
                    }
                    if sender.send(frame.as_ref().clone()).await.is_err() {
                        return;
                    }
                }
                LiveStep::Lagged(_) | LiveStep::Closed => return,
            }
        }
    })
}

fn parse_created_at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
