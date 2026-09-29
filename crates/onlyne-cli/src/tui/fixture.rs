//! The pinned board the TUI's tests draw and fold.
//!
//! One fixture, shared by the render tests and the key-map tests: both assert
//! against the same cluster, so a failure is about the behaviour a case names
//! rather than about a second set of rows. The five reads are written out as
//! their answers carry them — the same shapes the reducer's own fixture uses —
//! because the shapes are the server's and a front end that invented its own
//! would be testing itself.

use crate::tui::state::State;
use crate::tui::update::{self, Event};
use onlyne_proto::view::Snapshot;
use serde_json::{Value, json};

/// The pinned cluster: two roles, three sessions, four deliveries, and one
/// fault open with one already handled.
///
/// It is arranged so every reading the three pages make has something to say:
///
/// - `builder` is offline with one idle session and two queued sends;
/// - `planner` is online with one busy and one suspended session;
/// - the family `t1` runs `_supervisor → planner → builder → planner` and ends
///   in a receipt, with one hop still queued;
/// - the fault on `t2` is open, so the faults page offers its verbs.
pub fn snapshot() -> Snapshot {
    Snapshot {
        status: Some(json!({
            "ok": true,
            "cluster": "onlyne-dev",
            "version": "1.4.1",
            "spec_hash": "9f2c",
            "roles": 2,
            "role_count": 2,
            "gateway_count": 1,
            "gateways": [{"id": "gw1", "state": "offline"}],
            "routes": 3,
            "channels": 0,
            "connected_roles": 1,
            "connected_gateways": 0,
            "event_head": 42,
            "uptime_s": 900
        })),
        roles: serde_json::from_value(json!([
            {
                "name": "builder",
                "admin": false,
                "max_sessions": 1,
                "runtime": {"drive": "acp", "command": []},
                "spec_hash": "9f2c",
                "prose": "build the work",
                "state": "offline",
                "sessions": 1,
                "queued": 2,
                "edges": [],
                "aggregate": "workers"
            },
            {
                "name": "planner",
                "admin": false,
                "max_sessions": 2,
                "runtime": {"drive": "plugin", "command": ["pi"]},
                "spec_hash": "9f2c",
                "prose": "plan the work",
                "state": "online",
                "sessions": 2,
                "queued": 0,
                "edges": ["builder"],
                "aggregate": null
            }
        ]))
        .expect("the pinned roles"),
        sessions: serde_json::from_value(json!([
            {
                "session_id": "s-builder-1",
                "task_id": "t2",
                "role": "builder",
                "generation": 1,
                "seq": 4,
                "public_lifecycle": "idle",
                "projection": {
                    "lifecycle": "idle",
                    "agent": "idle",
                    "delivery": "none",
                    "resource": "attached",
                    "recovery": "none",
                    "outcome": "blocked"
                },
                "updated_at": "2026-09-28T10:00:20Z",
                "last_seen": "2026-09-28T10:00:25Z"
            },
            {
                "session_id": "s-planner-1",
                "task_id": "t1",
                "role": "planner",
                "generation": 1,
                "seq": 7,
                "public_lifecycle": "working",
                "projection": {
                    "lifecycle": "working",
                    "agent": "running",
                    "delivery": "pending",
                    "resource": "attached",
                    "recovery": "none"
                },
                "updated_at": "2026-09-28T10:00:00Z",
                "last_seen": "2026-09-28T10:00:05Z"
            },
            {
                "session_id": "s-planner-2",
                "role": "planner",
                "generation": 1,
                "seq": 2,
                "public_lifecycle": "idle",
                "projection": {
                    "lifecycle": "idle",
                    "agent": "idle",
                    "delivery": "none",
                    "resource": "closed",
                    "recovery": "idle_waiting"
                },
                "updated_at": "2026-09-28T09:58:00Z",
                "last_seen": "2026-09-28T09:58:30Z"
            }
        ]))
        .expect("the pinned sessions"),
        ledger: serde_json::from_value(json!([
            {
                "msg_id": "m1",
                "op_id": "o-1",
                "kind": "task",
                "from": {"role": {"role": "_supervisor"}},
                "to": {"role": {"role": "planner"}},
                "task": "t1",
                "hop": 0,
                "family": "t1",
                "hop_budget": 8,
                "origin": "_supervisor",
                "attempt": 1,
                "state": "in_flight",
                "out_head": "plan the work",
                "enqueued_at": "2026-09-28T10:00:00Z"
            },
            {
                "msg_id": "m2",
                "op_id": "o-2",
                "kind": "task",
                "from": {"role": {"role": "planner"}},
                "to": {"role": {"role": "builder"}},
                "task": "t2",
                "parent_task": "t1",
                "hop": 1,
                "family": "t1",
                "hop_budget": 8,
                "origin": "_supervisor",
                "attempt": 1,
                "state": "in_flight",
                "out_head": "build the thing",
                "enqueued_at": "2026-09-28T10:00:10Z"
            },
            {
                "msg_id": "m3",
                "op_id": "o-3",
                "kind": "completion",
                "from": {"role": {"role": "builder"}},
                "to": {"role": {"role": "planner"}},
                "task": "t3",
                "parent_task": "t1",
                "hop": 2,
                "family": "t1",
                "hop_budget": 8,
                "origin": "_supervisor",
                "attempt": 1,
                "state": "acked",
                "out_head": "the build is done",
                "reason": "delivered",
                "enqueued_at": "2026-09-28T10:00:20Z",
                "acked_at": "2026-09-28T10:00:30Z"
            },
            {
                "msg_id": "m4",
                "op_id": "o-4",
                "kind": "note",
                "from": {"role": {"role": "planner"}},
                "to": {"role": {"role": "builder"}},
                "task": "t4",
                "parent_task": "t1",
                "hop": 3,
                "family": "t1",
                "hop_budget": 8,
                "origin": "_supervisor",
                "attempt": 1,
                "state": "queued",
                "out_head": "a note for later",
                "enqueued_at": "2026-09-28T10:00:40Z"
            }
        ]))
        .expect("the pinned ledger"),
        // A repair verb moves a row's state, and the next snapshot re-read is
        // what drops it: the second row here is one already handled, so the
        // page's "open" filter is exercised rather than assumed.
        faults: serde_json::from_value(json!([
            {
                "id": 4,
                "task_id": "t2",
                "role": "builder",
                "kind": "intent_exhausted",
                "reason": "retries exhausted",
                "state": "open",
                "created_at": 1790000000
            },
            {
                "id": 5,
                "role": "planner",
                "kind": "idle_fault",
                "reason": "handled by an operator",
                "state": "handled",
                "created_at": 1790000100
            }
        ]))
        .expect("the pinned faults"),
    }
}

/// A board that has read the pinned snapshot, and connected to nothing.
///
/// No socket is opened and no server exists in this process: the one `update`
/// call is what a page's rows come from, which is the contract's "a page renders
/// from a `View` alone" made literal.
pub fn state() -> State {
    update::update(
        State::new("operator"),
        Event::Snapshot(Box::new(snapshot())),
    )
}

/// One event off the subscription, as the wire carries it: a class word and a
/// payload.
pub fn stream(value: Value) -> Event {
    let event = serde_json::from_value(value).expect("a pinned stream event");
    Event::Stream(Box::new(event))
}

/// The notice a lagging subscriber receives, which is a gap rather than news.
pub fn resync_lag() -> Event {
    stream(json!({
        "type": "fault",
        "data": {
            "id": 9,
            "kind": "resync_lag",
            "reason": "the subscriber lagged behind the broadcast",
            "state": "open",
            "created_at": 1790000200
        }
    }))
}

/// One `role_presence` event, the live half of a registry row.
pub fn presence(role: &str, state: &str, sessions: u32) -> Event {
    stream(json!({
        "type": "role_presence",
        "data": {
            "role": role,
            "state": state,
            "aggregate": "workers",
            "sessions": sessions
        }
    }))
}
