//! The observation board: the admin socket rendered in a terminal.
//!
//! The board used to ship as its own binary (`onlyne-tui`) that the `tui` verb
//! exec'd; it now runs in process. [`force`] and [`layout`] place the role map,
//! [`model`] holds the state and the frame loop's data, [`socket`] resolves the
//! admin socket, [`ui`] draws, and [`run`] is the verb's entry point.
//!
//! The board is a pure reader: it sends no op that changes the cluster.

pub mod force;
pub mod layout;
pub mod model;
mod run;
pub mod socket;
pub mod ui;

pub use run::run;
