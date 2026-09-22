//! Herdr session backend: one pane per onlyne session.
//!
//! A role lives in one herdr tab. Each session splits a new pane inside that
//! tab, then either starts a recognized agent in it or runs the spawn command
//! as a single shell line.
//!
//! Deprecated: `DEPRECATED.md` beside this module says why, what its failures do and
//! do not mean, and what a removal would touch.

mod cli;
mod policy;
mod resource;
mod session;

pub(crate) use policy::agent_name;
pub use session::HerdrBackend;

#[cfg(test)]
mod tests;
