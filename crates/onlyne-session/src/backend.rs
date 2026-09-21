// The parts below open with `use super::*`, so this module keeps the imports
// the single-file version shared with them: the std and serde names every part
// builds on, plus the two seams read through `super::` — the CLI-command
// helpers the process backends drive, and the host probe the herdr backend
// reads.
use crate::lifecycle::TaskState;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod command;
mod outcome;
mod port;
mod select;
mod spec;

pub mod acp;
pub mod exec;
pub mod fake;
pub mod herdr;
pub mod orca;
pub mod zellij;

#[cfg(test)]
mod tests;

use command::{failure_code, run_checked, run_json, unsupported};
use select::herdr_host_present;

pub use acp::{AcpBackend, AcpOptions};
pub use command::CommandFailure;
pub use orca::WorktreePolicy;
pub use outcome::{OutcomeFeed, OutcomeSink};
pub use port::{CommandOutput, ProcessRunner, Runner, SessionBackend};
pub use select::{
    backend_by_name, backend_for, backend_for_env, default_backend, detect_host, doctor_report,
    process_env, select_backend, select_backend_from_env,
};
pub use spec::{
    BACKEND_NAMES, BackendName, Capabilities, CloseReason, HostDetection, NO_SUPPORTED_HOST,
    NoSupportedHost, PanePlacement, ResourceProbe, SelectionSource, SessionOutcome, SessionRef,
    SpawnSpec, SplitDirection,
};
