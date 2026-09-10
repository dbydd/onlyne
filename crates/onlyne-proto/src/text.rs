//! Canonical operator-facing text this protocol pins (decision D18).
//!
//! These constants are the canonical copies of the five protocol strings.
//! `tests/text_vectors.json` pins their exact bytes, and each emitter keeps its
//! own byte-exact assertions where it prints: `onlyne-layout`, `onlyne-store`,
//! `onlyne-cli`, and the end-to-end scripts
//! `crates/onlyne-testkit/e2e/legacy-layout.sh` and
//! `crates/onlyne-testkit/e2e/idempotency.sh`.
//!
//! The fifth string, [`crate::frame::OP_ID_CONFLICT_MESSAGE`], is defined in
//! [`crate::frame`] and consumed by the server's idempotency check.

/// Emitted by `onlyne-cli` when no socket can be resolved from the flags.
pub const NO_SOCKET_MESSAGE: &str =
    "onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace";

/// Emitted by `onlyne-cli` when a requested daemon binary is absent.
pub const BINARY_NOT_FOUND_PREFIX: &str = "onlyne: binary not found: ";

/// Emitted by `onlyne-layout` when a workspace still holds the pre-v1 layout.
pub const LEGACY_WORKSPACE_MESSAGE: &str = "onlyne: legacy workspace layout; v1.0.0 does not migrate";

/// Emitted by `onlyne-store` when a database schema marker does not match this revision.
pub const UNSUPPORTED_SCHEMA_MESSAGE: &str = "onlyne: unsupported schema; v1.0.0 does not migrate";

/// Emitted by `onlyne-cli`, naming the missing daemon binary.
pub fn binary_not_found(name: &str) -> String {
    format!("{BINARY_NOT_FOUND_PREFIX}{name}")
}
