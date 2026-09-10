//! `onlyne` — one socket, one protocol, three sibling binaries.
//!
//! Message verbs (`send`, `reply`, `complete`, `handoff`, `control`, `who`,
//! `ping`) and bare admin nouns (`status`, `roles`, `sessions`, `ledger`,
//! `faults`, `watch`, `history`, `spec_diff`, `reload`, `wait-ready`,
//! `repair`, `cluster export-prose`) share one resolution path: resolve a
//! socket, write one frame, print one JSON line, return one exit code. The
//! admin nouns keep that path inside this process, so `onlyne server roles`
//! and `onlyne roles` issue the same frame.
//!
//! `onlyne server init|run|start|stop|status|generate|reload`, `onlyne
//! client`, and `onlyne gateway run|list|auth` exec a sibling binary and
//! inherit stdio.

mod admin;
mod flags;
mod forward;
mod ledger;
mod media;
mod render;
mod runtime;
mod socket;
mod verbs;
mod wire;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

use crate::flags::GlobalFlags;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "onlyne",
    bin_name = "onlyne",
    version,
    about = "One socket, one protocol, three sibling binaries.",
    subcommand_negates_reqs = true,
)]
struct Cli {
    #[command(flatten)]
    flags: GlobalFlags,

    #[command(subcommand)]
    verb: Option<Verb>,
}

#[derive(Subcommand, Debug, Clone)]
enum Verb {
    /// Run the onlyne server; lifecycle verbs exec, admin nouns query here.
    Server(ServerCmd),
    /// Run the onlyne client, forwarding every remaining argument.
    Client(RestArgs),
    /// Run the onlyne gateway; platform verbs exec, `status` queries here.
    Gateway(GatewayCmd),
    /// Deliver a message to a role.
    Send(SendCmd),
    /// Answer an envelope by its msg id.
    Reply(ReplyCmd),
    /// Report a task's terminal outcome.
    Complete(CompleteCmd),
    /// Hand a task to another role.
    Handoff(HandoffCmd),
    /// Drive a control op against a task.
    Control(ControlCmd),
    /// Query the roles the server knows.
    Who,
    /// Probe the socket with a ping frame and print the pong.
    Ping,
    /// Report server status.
    Status,
    /// List roles, optionally filtered by `--role`.
    Roles(AdminRolesCmd),
    /// List sessions.
    Sessions(SessionsCmd),
    /// List ledger rows.
    Ledger(LedgerCmd),
    /// List recorded faults.
    Faults(FaultsCmd),
    /// Stream event frames; `--follow` keeps the connection open.
    Watch(WatchCmd),
    /// Replay recorded envelopes.
    History(HistoryCmd),
    /// Diff the running spec against the configuration on disk.
    #[command(name = "spec_diff", alias = "spec-diff")]
    SpecDiff,
    /// Ask the server to re-read its configuration.
    Reload,
    /// Render a spec fragment into a workspace, forwarding to onlyne-server.
    Generate(GenerateCmd),
    /// Poll status until the server answers ok.
    WaitReady(WaitReadyCmd),
    /// Drive the recovery verbs.
    Repair(RepairCmd),
    /// Cluster-level verbs.
    Cluster(ClusterCmd),
    /// Print the version and each sibling's path.
    Version,
    /// Emit a shell completion script.
    Completions(CompletionsCmd),
}

#[derive(clap::Args, Debug, Clone)]
struct RestArgs {
    /// Arguments forwarded to the sibling, verbatim, flags included.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

/// The `server` group: the lifecycle verbs exec `onlyne-server`, and the
/// admin query and repair nouns resolve against the admin socket here.
#[derive(clap::Args, Debug, Clone)]
struct ServerCmd {
    #[command(subcommand)]
    verb: Option<ServerVerb>,
}

#[derive(Subcommand, Debug, Clone)]
enum ServerVerb {
    /// Create the server root and its `[server]` spec template.
    Init(RestArgs),
    /// Bind the listeners and serve the cluster.
    Run(RestArgs),
    /// Spawn a detached daemon and wait for the admin socket.
    Start(RestArgs),
    /// Signal the recorded daemon and wait for it to exit.
    Stop(RestArgs),
    /// Report process state for a server root.
    Status(RestArgs),
    /// Render role workspaces from the templates under the server root.
    Generate(RestArgs),
    /// Re-read the spec on disk.
    Reload(RestArgs),
    /// List roles, optionally filtered by `--role`.
    Roles(AdminRolesCmd),
    /// List sessions.
    Sessions(SessionsCmd),
    /// List ledger rows.
    Ledger(LedgerCmd),
    /// List recorded faults.
    Faults(FaultsCmd),
    /// Stream event frames; `--follow` keeps the connection open.
    Watch(WatchCmd),
    /// Replay recorded envelopes.
    History(HistoryCmd),
    /// Drive the recovery verbs.
    Repair(RepairCmd),
    /// A verb outside the server vocabulary.
    #[command(external_subcommand)]
    Unknown(Vec<String>),
}

/// The `gateway` group: the platform verbs exec `onlyne-gateway`, and `status`
/// reads the registered gateways from the admin socket here.
#[derive(clap::Args, Debug, Clone)]
struct GatewayCmd {
    #[command(subcommand)]
    verb: Option<GatewayVerb>,
}

#[derive(Subcommand, Debug, Clone)]
enum GatewayVerb {
    /// Serve one platform: `run <telegram|feishu|qqbot|weixin>`.
    Run(RestArgs),
    /// List the gateways declared under the server root.
    List(RestArgs),
    /// Platform onboarding, the former `auth` verb.
    Auth(RestArgs),
    /// Report the registered gateways and their capabilities.
    Status,
}

#[derive(clap::Args, Debug, Clone)]
struct SendCmd {
    #[command(flatten)]
    sender: verbs::SenderArgs,
    #[command(flatten)]
    args: verbs::SendArgs,
}

#[derive(clap::Args, Debug, Clone)]
struct ReplyCmd {
    #[command(flatten)]
    sender: verbs::SenderArgs,
    #[command(flatten)]
    args: verbs::ReplyArgs,
}

#[derive(clap::Args, Debug, Clone)]
struct CompleteCmd {
    #[command(flatten)]
    sender: verbs::SenderArgs,
    #[command(flatten)]
    args: verbs::CompleteArgs,
}

#[derive(clap::Args, Debug, Clone)]
struct HandoffCmd {
    #[command(flatten)]
    sender: verbs::SenderArgs,
    #[command(flatten)]
    args: verbs::HandoffArgs,
}

#[derive(clap::Args, Debug, Clone)]
struct ControlCmd {
    #[command(flatten)]
    sender: verbs::SenderArgs,
    /// Task id the control op targets.
    #[arg(long)]
    task: String,
    /// Role the control op targets; omitted means the role that owns the task.
    #[arg(long)]
    to: Option<String>,
    /// Reason, required for recycle and cancel.
    #[arg(long)]
    reason: Option<String>,
    /// The control op to drive.
    #[command(subcommand)]
    verb: ControlVerb,
}

#[derive(Subcommand, Debug, Clone)]
enum ControlVerb {
    /// Stop the task and let a fresh session pick it up.
    Recycle,
    /// Ask the session whether the task is still alive.
    Probe,
    /// Snapshot the task's state.
    Snapshot,
    /// Cancel the task.
    Cancel,
}

#[derive(clap::Args, Debug, Clone)]
struct AdminRolesCmd {
    #[command(flatten)]
    args: admin::RolesArgs,
}

#[derive(clap::Args, Debug, Clone)]
struct SessionsCmd {
    #[command(flatten)]
    args: admin::SessionsArgs,
}

#[derive(clap::Args, Debug, Clone)]
struct LedgerCmd {
    #[command(flatten)]
    args: admin::LedgerArgs,
}

#[derive(clap::Args, Debug, Clone)]
struct FaultsCmd {
    #[command(flatten)]
    args: admin::FaultsArgs,
}

#[derive(clap::Args, Debug, Clone)]
struct WatchCmd {
    #[command(flatten)]
    args: admin::WatchArgs,
}

#[derive(clap::Args, Debug, Clone)]
struct HistoryCmd {
    #[command(flatten)]
    args: admin::HistoryArgs,
}

#[derive(clap::Args, Debug, Clone)]
struct WaitReadyCmd {
    #[command(flatten)]
    args: admin::WaitReadyArgs,
}

#[derive(clap::Args, Debug, Clone)]
struct RepairCmd {
    /// The recovery verb to drive.
    #[command(subcommand)]
    verb: admin::RepairVerb,
}

#[derive(clap::Args, Debug, Clone)]
struct ClusterCmd {
    #[command(subcommand)]
    verb: ClusterVerb,
}

#[derive(Subcommand, Debug, Clone)]
enum ClusterVerb {
    /// Print one role's prose, raw by default so it can be pasted into a
    /// TOML multi-line string.
    ExportProse(admin::ExportProseArgs),
}

#[derive(clap::Args, Debug, Clone)]
struct GenerateCmd {
    /// Server root, forwarded as `--root`; `--server-root` works too.
    #[arg(long = "root")]
    root: Option<PathBuf>,
    /// Template paths, passed through verbatim.
    #[arg(long)]
    template: Vec<String>,
    /// Role scoping, passed through verbatim.
    #[arg(long)]
    role: Vec<String>,
    /// Output directory; the server defaults to `<server-root>/.onlyne/ws`.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Overwrite files that already exist.
    #[arg(long)]
    force: bool,
}

#[derive(clap::Args, Debug, Clone)]
struct CompletionsCmd {
    /// Shell to emit completions for.
    #[arg(value_enum)]
    shell: Shell,
}

fn run() -> i32 {
    let cli = Cli::parse();
    let Some(verb) = cli.verb else {
        let mut stderr = std::io::stderr();
        let _ = Cli::command().write_help(&mut stderr);
        return runtime::EXIT_VALIDATION;
    };
    let flags = &cli.flags;
    match verb {
        Verb::Server(cmd) => server(flags, cmd),
        Verb::Client(rest) => forward::exec("onlyne-client", &rest.args),
        Verb::Gateway(cmd) => gateway(flags, cmd),
        Verb::Send(cmd) => verbs::send(flags, &cmd.sender, cmd.args),
        Verb::Reply(cmd) => verbs::reply(flags, &cmd.sender, cmd.args),
        Verb::Complete(cmd) => verbs::complete(flags, &cmd.sender, cmd.args),
        Verb::Handoff(cmd) => verbs::handoff(flags, &cmd.sender, cmd.args),
        Verb::Control(cmd) => {
            let name = match cmd.verb {
                ControlVerb::Recycle => "recycle",
                ControlVerb::Probe => "probe",
                ControlVerb::Snapshot => "snapshot",
                ControlVerb::Cancel => "cancel",
            };
            let op = match verbs::build_control_op(name, cmd.task, cmd.reason.clone()) {
                Ok(op) => op,
                Err(message) => return runtime::usage_error(message),
            };
            verbs::control(
                flags,
                &cmd.sender,
                verbs::ControlVerbArgs {
                    to: cmd.to,
                    reason: cmd.reason,
                    op,
                },
            )
        }
        Verb::Who => verbs::who(flags),
        Verb::Ping => verbs::ping(flags),
        Verb::Status => admin::status(flags),
        Verb::Roles(cmd) => admin::roles(flags, cmd.args),
        Verb::Sessions(cmd) => admin::sessions(flags, cmd.args),
        Verb::Ledger(cmd) => admin::ledger(flags, cmd.args),
        Verb::Faults(cmd) => admin::faults(flags, cmd.args),
        Verb::Watch(cmd) => admin::watch(flags, cmd.args),
        Verb::History(cmd) => admin::history(flags, cmd.args),
        Verb::SpecDiff => admin::spec_diff(flags),
        Verb::Reload => admin::reload(flags),
        Verb::Generate(cmd) => generate(flags, cmd),
        Verb::WaitReady(cmd) => admin::wait_ready(flags, cmd.args),
        Verb::Repair(cmd) => admin::repair(flags, cmd.verb),
        Verb::Cluster(cmd) => match cmd.verb {
            ClusterVerb::ExportProse(args) => admin::export_prose(flags, args),
        },
        Verb::Version => {
            println!("{}", render::version_json());
            runtime::EXIT_OK
        }
        Verb::Completions(cmd) => {
            let mut stdout = std::io::stdout();
            clap_complete::generate(
                cmd.shell,
                &mut Cli::command(),
                "onlyne",
                &mut stdout,
            );
            runtime::EXIT_OK
        }
    }
}

/// `generate` execs `onlyne-server generate`, inheriting stdio so the spec
/// fragment reaches stdout and the progress lines reach stderr.
fn generate(flags: &GlobalFlags, cmd: GenerateCmd) -> i32 {
    let Some(root) = cmd.root.clone().or_else(|| flags.server_root.clone()) else {
        return runtime::usage_error("onlyne: generate requires --server-root or --root");
    };
    let mut args = vec![
        "generate".to_string(),
        "--root".to_string(),
        root.to_string_lossy().to_string(),
    ];
    for template in &cmd.template {
        args.push("--template".to_string());
        args.push(template.clone());
    }
    for role in &cmd.role {
        args.push("--role".to_string());
        args.push(role.clone());
    }
    if let Some(out) = &cmd.out {
        args.push("--out".to_string());
        args.push(out.to_string_lossy().to_string());
    }
    if cmd.force {
        args.push("--force".to_string());
    }
    forward::exec("onlyne-server", &args)
}

/// The `server` group. The lifecycle verbs exec `onlyne-server`; the admin
/// query and repair nouns resolve against the admin socket in this process.
fn server(flags: &GlobalFlags, cmd: ServerCmd) -> i32 {
    let Some(verb) = cmd.verb else {
        return forward::exec("onlyne-server", &[]);
    };
    match verb {
        ServerVerb::Init(rest) => sibling_exec("onlyne-server", "init", &rest),
        ServerVerb::Run(rest) => sibling_exec("onlyne-server", "run", &rest),
        ServerVerb::Start(rest) => sibling_exec("onlyne-server", "start", &rest),
        ServerVerb::Stop(rest) => sibling_exec("onlyne-server", "stop", &rest),
        ServerVerb::Status(rest) => sibling_exec("onlyne-server", "status", &rest),
        ServerVerb::Generate(rest) => sibling_exec("onlyne-server", "generate", &rest),
        ServerVerb::Reload(rest) => sibling_exec("onlyne-server", "reload", &rest),
        ServerVerb::Roles(cmd) => admin::roles(flags, cmd.args),
        ServerVerb::Sessions(cmd) => admin::sessions(flags, cmd.args),
        ServerVerb::Ledger(cmd) => admin::ledger(flags, cmd.args),
        ServerVerb::Faults(cmd) => admin::faults(flags, cmd.args),
        ServerVerb::Watch(cmd) => admin::watch(flags, cmd.args),
        ServerVerb::History(cmd) => admin::history(flags, cmd.args),
        ServerVerb::Repair(cmd) => admin::repair(flags, cmd.verb),
        ServerVerb::Unknown(args) => unknown_server_verb(&args),
    }
}

/// Run a sibling daemon's `<verb>` with the remaining arguments verbatim,
/// flags included.
fn sibling_exec(bin: &str, verb: &str, rest: &RestArgs) -> i32 {
    let mut args = Vec::with_capacity(rest.args.len() + 1);
    args.push(verb.to_string());
    args.extend(rest.args.iter().cloned());
    forward::exec(bin, &args)
}

/// The refusal for a verb outside the server vocabulary.
fn unknown_server_verb(args: &[String]) -> i32 {
    let name = args.first().map(String::as_str).unwrap_or_default();
    runtime::usage_error(format!("onlyne: unknown server verb {name}"))
}

/// The `gateway` group. The platform verbs exec `onlyne-gateway`; `status`
/// reports the registered gateways, read from `AdminOp::Status`.
fn gateway(flags: &GlobalFlags, cmd: GatewayCmd) -> i32 {
    let Some(verb) = cmd.verb else {
        return forward::exec("onlyne-gateway", &[]);
    };
    match verb {
        GatewayVerb::Run(rest) => sibling_exec("onlyne-gateway", "run", &rest),
        GatewayVerb::List(rest) => sibling_exec("onlyne-gateway", "list", &rest),
        GatewayVerb::Auth(rest) => sibling_exec("onlyne-gateway", "auth", &rest),
        GatewayVerb::Status => admin::status(flags),
    }
}

fn main() {
    std::process::exit(run());
}
