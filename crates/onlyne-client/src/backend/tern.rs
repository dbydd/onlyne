//! Tern backend: one block per onlyne session, one tab per role, one session
//! per cluster.
//!
//! Tern's tree is session → tab → block, and this backend maps one onlyne
//! cluster onto a Tern session, one role onto a tab of it, and one onlyne
//! session onto a block split beside the tab's base block.
//!
//! Kept beside herdr's shape by operator decision. What differs is every host
//! operation: Tern takes a numeric block id rather than a `wF:p2` pane handle,
//! reports plain-text refusals with no JSON error code, and cannot rename a
//! block without renaming the tab that holds it — so this backend answers
//! `rename` unsupported rather than rename a role's whole tab.

mod cli;
mod policy;
mod resource;
mod session;

pub use session::TernBackend;

#[cfg(test)]
mod tests;
