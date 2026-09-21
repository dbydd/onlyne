//! Foreground role runtime: one authenticated server link, four tasks, and the
//! adapter socket that keeps serving plugins across reconnects.
//!
//! The transport handles death and redialing behind one handle, so this loop
//! follows its readiness. Every pass from `Reconnecting` back to `Ready` runs
//! the order the plan fixes for a reconnect: welcome, intent flush, event
//! resume, pull.

mod config;
mod link;
mod run;
mod sessions;

#[cfg(test)]
mod test_support;

pub use config::{
    ClientInit, DEFAULT_INTENT_ATTEMPTS, EVENT_CURSOR_KEY, FLUSH_PAUSE_MS, NOT_READY_PAUSE_MS,
    OUTCOME_POLL_MS, PULL_HOLD_MS, PULL_LIMIT, PULL_PAUSE_MS, READINESS_POLL_MS,
    RECONNECT_LADDER_SECONDS, RunState, SHUTDOWN_CLOSE_BUDGET, acp_options, default_intent_backoff,
    reconnect_backoff,
};
pub use run::run;
pub use sessions::accept_path;
