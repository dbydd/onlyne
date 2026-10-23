//! The observation board: the admin socket rendered in a terminal.
//!
//! The board runs inside `onlyne` (`AGENTS.md` §5) and is a reader that can
//! also ask for the four ops the plan gives it: send a task, `focus`, `repair`,
//! `report`.
//!
//! ## One state, one fold, one task
//!
//! [`state::State`] is `(View, UiState)`: `onlyne_proto::view`'s reducer output,
//! which this crate never writes by hand, and what only this screen knows.
//! [`update::update`] is the only function that moves it, and it is pure —
//! [`run`]'s loop is its only caller, and it is called for a key press and for a
//! frame off the socket alike. [`io`] is the one task that owns the admin
//! connection; it hands the front end events, and the front end hands it back
//! the ops the reducer recorded in the state's outbox.
//!
//! ## Three pages, no map
//!
//! [`cluster`] is the roles, the selected role's board, and the event tail;
//! [`task`] is one family's path across roles with every delivery, receipt, and
//! the session log's tail; [`faults`] is the open faults and the repair verb
//! each one offers. The plan deletes v1's force-directed map outright
//! (`docs/v2-PLAN.md` line 401): a role's box had a second way to be placed
//! there, and this board has no geometry to disagree with.

mod cluster;
mod faults;
mod io;
mod render;
mod rows;
mod run;
mod socket;
mod state;
mod task;
mod update;

pub use run::run;
