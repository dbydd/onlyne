//! SQLite persistence for Onlyne v1.0.0.
//!
//! ## Schema versus plan
//!
//! Every timestamp column in both databases is written by this crate's conversion helpers, `unix_to_rfc3339` and `rfc3339`, and no caller supplies a raw stored value: the kernel's `i64` seconds (`SessionWrite`, `ServerFaultRow`, `VersionedSession`, `FaultRecord`) convert at the write, and the one text input, `RoleRow::updated_at` from the server's `upsert_role` call sites, is re-encoded through the same helpers before the insert.
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
