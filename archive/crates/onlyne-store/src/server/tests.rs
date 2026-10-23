//! Unit tests for the server store's shared helpers: the row conversions, the
//! timestamp helpers, and the statement fragments the schema tests do not
//! exercise directly.
//!
//! These tests live beside the code they cover rather than in
//! `tests/schema_statements.rs` because they assert on private helpers — the
//! conversion functions and SQL fragments — that the integration test cannot
//! name.

use onlyne_proto::LedgerState;
use onlyne_proto::{MsgKind, Outcome};

use super::conn::{is_legacy_table, sql_limit};
use super::rows::{LedgerRow, parse_ledger_state, parse_msg_kind, parse_outcome};
use super::rows::{conversion_error, head_preview, rfc3339_to_unix, unix_to_rfc3339};

// The helpers under test are `pub(crate)`, so the test module reaches them
// through `super` and every assertion below runs inside the crate's own unit
// test pass rather than the integration suite.

// --- timestamp round trips -------------------------------------------------

#[test]
fn unix_to_rfc3339_and_back_round_trip() {
    let seconds = 1_787_616_000i64; // 2026-08-25T00:00:00Z
    let text = unix_to_rfc3339(seconds);
    assert_eq!(text, "2026-08-25T00:00:00.000Z");
    assert_eq!(rfc3339_to_unix(&text), seconds);
}

#[test]
fn unix_to_rfc3339_encodes_out_of_range_seconds_as_epoch() {
    assert_eq!(unix_to_rfc3339(i64::MAX), "1970-01-01T00:00:00.000Z");
    assert_eq!(unix_to_rfc3339(i64::MIN), "1970-01-01T00:00:00.000Z");
}

#[test]
fn rfc3339_to_unix_decodes_garbage_as_zero() {
    assert_eq!(rfc3339_to_unix("not a timestamp"), 0);
    assert_eq!(rfc3339_to_unix(""), 0);
}

// --- column parsing --------------------------------------------------------

#[test]
fn parse_msg_kind_reads_every_wire_spelling() {
    assert_eq!(parse_msg_kind("task"), Some(MsgKind::Task));
    assert_eq!(parse_msg_kind("completion"), Some(MsgKind::Completion));
    assert_eq!(parse_msg_kind("note"), Some(MsgKind::Note));
    assert_eq!(parse_msg_kind("control"), Some(MsgKind::Control));
    assert_eq!(parse_msg_kind("Task"), None);
    assert_eq!(parse_msg_kind(""), None);
}

#[test]
fn parse_ledger_state_reads_every_state_spelling() {
    assert_eq!(parse_ledger_state("queued"), Some(LedgerState::Queued));
    assert_eq!(parse_ledger_state("in_flight"), Some(LedgerState::InFlight));
    assert_eq!(parse_ledger_state("acked"), Some(LedgerState::Acked));
    assert_eq!(parse_ledger_state("rejected"), Some(LedgerState::Rejected));
    assert_eq!(parse_ledger_state("expired"), Some(LedgerState::Expired));
    assert_eq!(parse_ledger_state("working"), None);
}

#[test]
fn parse_outcome_reads_every_outcome_spelling() {
    assert_eq!(parse_outcome("done"), Some(Outcome::Done));
    assert_eq!(parse_outcome("failed"), Some(Outcome::Failed));
    assert_eq!(parse_outcome("cancelled"), Some(Outcome::Cancelled));
    assert_eq!(parse_outcome("blocked"), Some(Outcome::Blocked));
    assert_eq!(parse_outcome("settled"), None);
}

#[test]
fn conversion_error_wraps_the_message_for_rusqlite() {
    let error = conversion_error(3, "bad column".to_string());
    match error {
        rusqlite::Error::FromSqlConversionFailure(index, _, _) => assert_eq!(index, 3),
        other => panic!("expected a conversion failure, got {other}"),
    }
}

// --- text helpers ----------------------------------------------------------

#[test]
fn head_preview_cuts_on_grapheme_cluster_boundaries() {
    // A flag is two regional indicators; a code-point cut would split it.
    let flag = "🇯🇵🇯🇵🇯🇵";
    let head = head_preview(flag);
    assert_eq!(head.chars().count(), flag.chars().count());
    let long = "a".repeat(300);
    assert_eq!(head_preview(&long).chars().count(), 200);
}

#[test]
fn head_preview_keeps_a_zwj_emoji_whole() {
    // Family emoji: seven code points, one grapheme cluster.
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
    assert_eq!(head_preview(family), family);
}

#[test]
fn head_preview_takes_the_first_two_hundred_clusters_of_wide_text() {
    // CJK characters are one cluster each, so the count is exact rather than
    // byte-derived.
    let text = "字".repeat(250);
    assert_eq!(head_preview(&text).chars().count(), 200);
}

// --- SQL fragments ---------------------------------------------------------

#[test]
fn sql_limit_floors_to_the_default_and_caps_at_five_hundred() {
    assert_eq!(sql_limit(0), 100);
    assert_eq!(sql_limit(1), 1);
    assert_eq!(sql_limit(500), 500);
    assert_eq!(sql_limit(10_000), 500);
}

#[test]
fn legacy_tables_are_named_exactly() {
    assert!(is_legacy_table("io_cursors"));
    assert!(is_legacy_table("loopback_idempotency"));
    assert!(is_legacy_table("pending_replies"));
    assert!(is_legacy_table("swarm_alpha"));
    assert!(!is_legacy_table("ledger"));
    assert!(!is_legacy_table("io_cursors_v2"));
}

// --- row construction ------------------------------------------------------

#[test]
fn ledger_row_from_envelope_records_causality() {
    use onlyne_proto::{Body, Causality, Principal, new_envelope};

    let causality = Causality::root("00000000-0000-4000-8000-000000000001".to_string());
    let envelope = new_envelope(
        MsgKind::Task,
        Principal::role("alice"),
        Principal::role("worker"),
        Body::text("do the thing"),
        Some(causality),
    )
    .unwrap();
    let row = LedgerRow::from_envelope(&envelope, "fp").unwrap();
    assert_eq!(row.state, LedgerState::Queued);
    assert_eq!(row.hop, 0);
    assert!(row.family.is_some());
    assert_eq!(row.out_head.as_deref(), Some("do the thing"));
    assert_eq!(row.sender().unwrap().to_string(), "alice");
    assert!(!row.sender_is_admin());
}
