// The parts below open with `use super::*`, so this module keeps the imports
// the single-file version shared with them: the std and serde names every part
// builds on, plus the seams read through `super::` — the CLI-command helpers
// the process backends drive and the host probe.
use anyhow::Result;
use onlyne_proto::lifecycle::TaskState;
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
pub mod external;
pub mod fake;
pub mod orca;
pub mod tern;
pub mod zellij;

use command::{failure_code, run_checked, run_json, unsupported};

pub use acp::{AcpBackend, AcpOptions};
pub use command::CommandFailure;
pub use orca::WorktreePolicy;
pub use outcome::{OutcomeFeed, OutcomeSink};
pub use port::{CommandOutput, ProcessRunner, Runner, SessionBackend};
pub use select::{backend_for, detect_placement, doctor_report, process_env};
pub use spec::{
    Capabilities, CloseReason, PanePlacement, PlacementDetection, ResourceProbe, SelectionSource,
    SessionOutcome, SessionPlacement, SessionRef, SpawnSpec, SplitDirection, UnknownPlacement,
    unknown_placement,
};
pub use tern::TernBackend;
