//! End-to-end scenarios for the client role runtime, one module per subject.
//!
//! `common` holds the shared fixtures and the recording fakes; every other
//! module pins one behaviour of the client and keeps its tests' `use` lines.

mod backends;
mod capacity;
mod common;
mod control;
mod init;
mod intents;
mod link;
mod plugins;
mod projection;
mod reconnect;
mod retirement;
mod role_socket;
mod settlement;
