//! SQLite persistence for Onlyne v1.0.0.
//!
//! ## Schema versus plan
//!
//! The `sessions.updated_at` columns are INTEGER Unix seconds because the frozen `SessionLedger` rows use `i64` timestamps.
//! The server `ledger.body_json` column is nullable because retention pruning clears acknowledged bodies after the cutoff.
//! Extra indexes support ledger pulls, fault deduplication, due-intent ordering, and event-cursor scans.
//! `schema_marker` records the v1.0.0 schema and protocol gate for each database.

mod client;
mod error;
mod server;
#[cfg(test)]
mod tests;
pub use client::{CLIENT_DDL, ClientStore, IntentRow};
pub use error::{StoreError, StoreResult, UNSUPPORTED_SCHEMA};
pub use server::{
    Append, CursorRow, EventRecord, FaultQuery, LedgerRow, RoleRow, SERVER_DDL, ServerFaultRow,
    ServerLedger, ServerSessionRow, SessionWrite, rfc3339,
};

pub use onlyne_proto::LedgerState;

pub fn transition_allowed(from: LedgerState, to: LedgerState) -> bool {
    matches!(
        (from, to),
        (LedgerState::Queued, LedgerState::InFlight)
            | (LedgerState::Queued, LedgerState::Rejected)
            | (LedgerState::Queued, LedgerState::Expired)
            | (LedgerState::InFlight, LedgerState::Acked)
            | (LedgerState::InFlight, LedgerState::Queued)
            | (LedgerState::InFlight, LedgerState::Rejected)
    )
}
