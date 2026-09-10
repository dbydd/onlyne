//! Global flags shared by every message verb and every bare admin verb.

use clap::builder::ArgAction;
use clap::{Args, ValueEnum};
use std::path::PathBuf;

/// Default per-operation socket bound in milliseconds.
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// Default sender role for a client-surface message verb.
pub const DEFAULT_ROLE: &str = "cli";

/// Environment variable holding the sender role of a client-surface connection.
pub const ROLE_ENV: &str = "ONLYNE_ROLE";

/// `--as <surface>` selection, applied to a `--socket` path that carries no other hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum AsArg {
    /// Infer from the path suffix `.onlyne/run/s`.
    #[default]
    Auto,
    /// Treat the socket as the local admin surface.
    Admin,
    /// Treat the socket as a client surface.
    Client,
}

/// Flags every verb accepts. Declared once and flattened into each subcommand
/// so `--socket`, `--timeout`, and `--request` behave the same everywhere.
#[derive(Debug, Clone, Args)]
pub struct GlobalFlags {
    /// Unix socket path, used verbatim.
    #[arg(long, global = true)]
    pub socket: Option<PathBuf>,
    /// Server root; the admin socket is `<dir>/.onlyne/run/s`.
    #[arg(long, global = true)]
    pub server_root: Option<PathBuf>,
    /// Role workspace; the client socket is `<dir>/.onlyne/run/s`, searched upward.
    #[arg(long, global = true)]
    pub workspace: Option<PathBuf>,
    /// Surface hint for a `--socket` path with no other hint.
    #[arg(
        long = "as",
        global = true,
        default_value = "auto",
        value_enum
    )]
    pub surface_hint: AsArg,
    /// Bound for every socket operation, in milliseconds.
    #[arg(long = "timeout", alias = "timeout-ms", global = true, default_value_t = DEFAULT_TIMEOUT_MS)]
    pub timeout_ms: u64,
    /// Print the answer with two-space indentation.
    #[arg(long, global = true)]
    pub pretty: bool,
    /// Print only the payload object of the answer.
    #[arg(long, global = true)]
    pub quiet: bool,
    /// Accepted for script legibility. Every verb already prints json; only
    /// `cluster export-prose` changes behaviour, wrapping the prose in an object.
    #[arg(long, global = true, action = ArgAction::SetTrue)]
    pub json: bool,
    /// Replace the constructed args body verbatim with this JSON object.
    /// `send`, `reply`, `complete`, and `handoff` consume it, and each one
    /// validates the envelope the object carries before opening the socket.
    /// `ping` and `control` refuse it: `ControlArgs` and `AdminControl` carry
    /// no envelope, so there is nothing meaningful to override. Every other
    /// verb ignores it.
    #[arg(long, global = true)]
    pub request: Option<String>,
}

impl GlobalFlags {
    /// The local sender role, read from `ONLYNE_ROLE`, defaulting to `cli`.
    pub fn local_role(&self) -> String {
        std::env::var(ROLE_ENV).unwrap_or_else(|_| DEFAULT_ROLE.to_string())
    }

    /// Parse a replacement op body from `--request`, or hand back the built one.
    pub fn override_args<T: serde::de::DeserializeOwned>(&self, built: T) -> Result<T, String> {
        let Some(raw) = &self.request else {
            return Ok(built);
        };
        let value = match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(v) => v,
            Err(error) => return Err(error.to_string()),
        };
        serde_json::from_value(value).map_err(|error| error.to_string())
    }
}
