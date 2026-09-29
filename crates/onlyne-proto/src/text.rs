//! Canonical operator-facing text this protocol pins (decision D18).
//!
//! These items are the canonical copies of the protocol strings.
//! `tests/text_vectors.json` pins their exact bytes, and each emitter keeps its
//! own byte-exact assertions where it prints: `onlyne-config`, `onlyne-store`,
//! `onlyne-cli`, `onlyne-server`, `onlyne-client`, and the end-to-end scripts
//! `crates/onlyne-testkit/e2e/legacy-layout.sh` and
//! `crates/onlyne-testkit/e2e/idempotency.sh`.
//!
//! The fifth string, [`crate::frame::OP_ID_CONFLICT_MESSAGE`], is defined in
//! [`crate::frame`] and consumed by the server's idempotency check.
//!
//! The schema refusal below is the whole of the v2 upgrade path, and v2 has no
//! migration command by design: a cluster is drained, the old files are moved
//! aside by hand, and the new build starts on an empty ledger. So the sentence
//! names *what was found*, not the product version — a message that said only
//! "unsupported schema" left the reader to work out which file was wrong and
//! whether anything could be done about it.

/// Emitted by `onlyne-cli` when no socket can be resolved from the flags.
pub const NO_SOCKET_MESSAGE: &str =
    "onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace";

/// Emitted by `onlyne-cli` when a requested daemon binary is absent.
pub const BINARY_NOT_FOUND_PREFIX: &str = "onlyne: binary not found: ";

/// Emitted by `onlyne-config` when a workspace still holds the pre-v1 layout.
///
/// The remedy is named because no command performs it: v2 does not migrate a
/// workspace, it starts a new one beside the old.
pub fn legacy_workspace_message(found: &[String]) -> String {
    let named = if found.is_empty() {
        "a pre-v1 layout".to_string()
    } else {
        format!("the pre-v1 table(s) {}", found.join(", "))
    };
    format!(
        "onlyne: {named}; this build does not migrate a workspace — point the client at a \
         workspace `onlyne-client init` has written, and keep this one for reference"
    )
}

/// The exit code every binary returns when it refuses to start on a file from
/// another revision, rather than on input that is merely wrong.
///
/// A supervisor needs the two apart. `1` is "this run failed" and covers a bad
/// flag, a dead peer and a refused op; refusing to start on a database or a
/// workspace this build cannot read is a different thing with a different
/// remedy, and a script that only sees `1` retries forever. This is the plan's
/// §"落地顺序" split of the overloaded `2`, which used to carry both a usage
/// error and a legacy refusal.
///
/// `6`, because `5` is the client's "no supported terminal host" and a refusal
/// must not borrow a code that already means something else.
pub const EXIT_NEEDS_MIGRATION: i32 = 6;

/// What a store found where it wanted a database of its own shape.
///
/// Every arm carries what was actually read, because that is the half of the
/// sentence an operator can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaMismatch {
    /// Tables this build does not know are present, so the file predates it.
    LegacyTables {
        /// The unknown table names, so the reader can look for them.
        names: Vec<String>,
    },
    /// The marker's schema version is not this build's.
    Version {
        /// The revision the file carries.
        found: i64,
        /// The revision this build writes and reads.
        expected: i64,
    },
    /// The marker's protocol version is not this build's.
    Protocol {
        /// The protocol revision the file carries.
        found: i64,
        /// The protocol revision this build speaks.
        expected: i64,
    },
    /// The file already holds tables or another marker's row, so it is not the
    /// empty file this build would have created.
    NotEmpty {
        /// How many tables the file carries.
        tables: usize,
    },
}

/// The sentence a store prints when it will not open a database.
///
/// v2 rewrites nothing, so the message says what to do instead: the old file
/// stays where it is and the new build starts beside it.
pub fn unsupported_schema_message(which: &str, mismatch: &SchemaMismatch) -> String {
    let found = match mismatch {
        SchemaMismatch::LegacyTables { names } => {
            format!("the pre-v1 table(s) {}", names.join(", "))
        }
        SchemaMismatch::Version { found, .. } => format!("schema revision {found}"),
        SchemaMismatch::Protocol { found, .. } => format!("protocol revision {found}"),
        SchemaMismatch::NotEmpty { tables } => {
            format!("a file that already holds {tables} table(s)")
        }
    };
    format!(
        "onlyne: the {which} database carries {found}, which this build cannot read; v2 starts \
         on an empty ledger and rewrites nothing — move the old file aside and start again"
    )
}

/// Emitted by `onlyne-cli`, naming the missing daemon binary.
pub fn binary_not_found(name: &str) -> String {
    format!("{BINARY_NOT_FOUND_PREFIX}{name}")
}

/// Emitted by `Envelope::validate()` when a `Task`, `Completion`, or `Control`
/// envelope arrives with no causality chain (§3 line 179's rule block).
pub fn causality_required(kind: &str) -> String {
    format!("causality is required for kind {kind}")
}
