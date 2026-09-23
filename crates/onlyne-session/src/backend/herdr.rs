//! Herdr session backend: one pane per onlyne session.
//!
//! A role lives in one herdr tab. Each session splits a new pane inside that
//! tab, then either starts a recognized agent in it or runs the spawn command
//! as a single shell line.
//!
//! Kept by operator decision. `NOTE.md` beside this module records the standing, the tty
//! cost this backend carries at many panes, and the one test flake in this tree.

mod cli;
mod policy;
mod resource;
mod session;

pub(crate) use policy::agent_name;
pub use session::HerdrBackend;

#[cfg(test)]
mod tests;
