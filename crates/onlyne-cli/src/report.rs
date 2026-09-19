//! `onlyne report` — local validation of a payload-v2 completion report.
//!
//! Every verb in this family is a filesystem operation on the workspace
//! directory and nothing else: no socket is resolved, no frame is written, so
//! the verbs answer inside a session whose client has not started yet and on a
//! machine that serves no cluster. The parse itself is not here; the family
//! runs exactly the parser the client will run when it reads the report,
//! `onlyne_proto::payload::parse`, and prints the grammar constant beside any
//! refusal so the author sees the rule the file broke.

use clap::{Args, Subcommand, ValueEnum};
use onlyne_layout::RoleWorkspace;
use onlyne_proto::payload::{GRAMMAR_V2, Handoff, PayloadV2, parse};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::flags::GlobalFlags;
use crate::runtime::{self, EXIT_ANSWER_FAILED, EXIT_OK, EXIT_VALIDATION};

/// The verdict line every `report write` opens with, by kind.
const VERDICT_PREFIXES: [(&str, &str); 3] = [
    ("done", "hop-done:"),
    ("failed", "hop-failed:"),
    ("blocked", "hop-blocked:"),
];

/// What the family is, printed above the grammar in `onlyne report --help`.
const FAMILY_INTRO: &str = "\
Validate a completion report without asking any daemon. The four verbs
read and write files under the workspace directory alone; no socket opens.";

/// What `report check` decides, printed above the grammar.
const CHECK_INTRO: &str = "\
Parse one report file and print its verdict, its head or reason, and every
handoff line it carries. Exit codes: 0 for a valid report, and 2 for a report
the verb cannot answer with: an invalid file (the refusal names the line it
broke, and the grammar follows on stderr), a file that is not there, or one
that cannot be read.";

/// The family's long help: the intro and the one grammar constant, in full.
pub fn family_long_about() -> String {
    format!("{FAMILY_INTRO}\n\n{GRAMMAR_V2}")
}

/// The `check` verb's long help: what its exit codes mean, then the grammar.
pub fn check_long_about() -> String {
    format!("{CHECK_INTRO}\n\n{GRAMMAR_V2}")
}

/// `onlyne report <verb>`: the subcommand group registered under `Verb::Report`.
#[derive(Args, Debug, Clone)]
pub struct ReportCmd {
    #[command(subcommand)]
    pub verb: ReportVerb,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ReportVerb {
    /// Print the absolute report path one task's verdict travels on.
    Path(ReportPathArgs),
    /// Parse a report file and print its verdict and handoffs.
    #[command(long_about = check_long_about())]
    Check(ReportCheckArgs),
    /// Write a valid report file from its parts, atomically.
    Write(ReportWriteArgs),
    /// Parse a string or standard input without touching any file.
    Validate(ReportValidateArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ReportPathArgs {
    /// Task id naming the file `<workspace>/.onlyne/out/<task>.md`.
    #[arg(long)]
    pub task: String,
}

#[derive(Args, Debug, Clone)]
pub struct ReportCheckArgs {
    /// Task id whose report file is checked; the file is
    /// `<workspace>/.onlyne/out/<task>.md`.
    #[arg(long)]
    pub task: Option<String>,
    /// Report file given directly; outranks `--task`.
    #[arg(long)]
    pub path: Option<PathBuf>,
}

/// The verdict kind `report write` constructs, one per grammar prefix.
#[derive(ValueEnum, Debug, Clone, Copy)]
pub enum VerdictArg {
    Done,
    Failed,
    Blocked,
}

impl VerdictArg {
    /// The grammar's verdict prefix for this kind, colon included.
    fn prefix(self) -> &'static str {
        let name = match self {
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
        };
        VERDICT_PREFIXES
            .iter()
            .find(|(kind, _)| *kind == name)
            .map(|(_, prefix)| *prefix)
            .expect("every kind names one grammar prefix")
    }
}

#[derive(Args, Debug, Clone)]
pub struct ReportWriteArgs {
    /// Task id naming the file `<workspace>/.onlyne/out/<task>.md`.
    #[arg(long)]
    pub task: Option<String>,
    /// Report file given directly; outranks `--task`.
    #[arg(long)]
    pub path: Option<PathBuf>,
    /// Verdict kind: `done`, `failed`, or `blocked`.
    #[arg(long, value_enum)]
    pub verdict: VerdictArg,
    /// The one-line conclusion, reason, or blocker.
    #[arg(long)]
    pub head: String,
    /// One handoff line as `<role>` or `<role>|<one-line text>`; repeatable.
    /// A bare role ships the verdict line as the handed-off body.
    #[arg(long)]
    pub handoff: Vec<String>,
}

#[derive(Args, Debug, Clone)]
pub struct ReportValidateArgs {
    /// The report text to parse, verbatim.
    #[arg(long)]
    pub text: Option<String>,
    /// Where to read the text from; only `-` (standard input) is accepted.
    #[arg(long)]
    pub from: Option<String>,
}

pub fn run(flags: &GlobalFlags, verb: ReportVerb) -> i32 {
    match verb {
        ReportVerb::Path(args) => path(flags, &args.task),
        ReportVerb::Check(args) => check(flags, &args),
        ReportVerb::Write(args) => write(flags, &args),
        ReportVerb::Validate(args) => validate(&args),
    }
}

/// Print the report file and the three journal surfaces of one task, each on
/// its labeled line. The operators and the agent read these paths blind: the
/// ACP session owns no terminal, so the files under `.onlyne` are the whole
/// visible surface of a running task, and no other shipped command names them.
fn path(flags: &GlobalFlags, task: &str) -> i32 {
    let (report, workspace) = match task_path(flags, task) {
        Ok(pair) => pair,
        Err(message) => return runtime::usage_error(message),
    };
    let layout = RoleWorkspace::resolve(&workspace);
    println!("report: {}", report.display());
    println!("log: {}", layout.session_log_path(task).display());
    println!("events: {}", layout.session_events_path(task).display());
    println!("content: {}", layout.content_index_path().display());
    EXIT_OK
}

/// Read one report file and print what the client's parser makes of it.
fn check(flags: &GlobalFlags, args: &ReportCheckArgs) -> i32 {
    let file = match target_file(flags, args.task.as_deref(), args.path.as_deref()) {
        Ok(file) => file,
        Err(message) => return runtime::usage_error(message),
    };
    let bytes = match std::fs::read(&file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("onlyne: absent: {}", file.display());
            return EXIT_VALIDATION;
        }
        Err(error) => {
            eprintln!("onlyne: cannot read {}: {error}", file.display());
            return EXIT_VALIDATION;
        }
    };
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            return print_report(&PayloadV2::Invalid {
                error: format!("{}: report file is not valid UTF-8", file.display()),
            });
        }
    };
    print_report(&parse(&text))
}

/// Build one valid report from its parts and rename it into place.
fn write(flags: &GlobalFlags, args: &ReportWriteArgs) -> i32 {
    let file = match target_file(flags, args.task.as_deref(), args.path.as_deref()) {
        Ok(file) => file,
        Err(message) => return runtime::usage_error(message),
    };
    let head = args.head.trim();
    if head.is_empty() {
        return runtime::usage_error("onlyne: --head must carry the one-line verdict body");
    }
    if head.contains(['\n', '\r']) {
        return runtime::usage_error("onlyne: --head must be one line");
    }
    let mut lines = vec![format!("{} {head}", args.verdict.prefix())];
    for raw in &args.handoff {
        let (role, text) = match raw.split_once('|') {
            Some((role, text)) => (role.trim(), Some(text.trim())),
            None => (raw.trim(), None),
        };
        if role.is_empty() || role.contains(char::is_whitespace) {
            return runtime::usage_error(format!(
                "onlyne: --handoff needs one role token before the `|`: {raw}"
            ));
        }
        match text {
            None | Some("") => lines.push(format!("handoff: {role}")),
            Some(text) => {
                if text.contains(['\n', '\r']) {
                    return runtime::usage_error(format!(
                        "onlyne: --handoff text must be one line: {raw}"
                    ));
                }
                lines.push(format!("handoff: {role} | {text}"));
            }
        }
    }
    let text = format!("{}\n", lines.join("\n"));
    // The construction above is total, so an invalid result is a disagreement
    // with the parser the client will run. Refuse the write rather than ship a
    // file the client would cancel the task over.
    if let PayloadV2::Invalid { error } = parse(&text) {
        return runtime::usage_error(format!(
            "onlyne: refuses to write an invalid report: {error}"
        ));
    }
    let Some(parent) = file.parent() else {
        return runtime::usage_error(format!(
            "onlyne: {} carries no directory to write beside",
            file.display()
        ));
    };
    if let Err(error) = std::fs::create_dir_all(parent) {
        return runtime::usage_error(format!(
            "onlyne: cannot create {}: {error}",
            parent.display()
        ));
    }
    let leaf = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("report.md");
    let temporary = parent.join(format!(".{leaf}.{}.tmp", std::process::id()));
    if let Err(error) = std::fs::write(&temporary, &text) {
        return report_io_failure(format!(
            "onlyne: cannot write {}: {error}",
            temporary.display()
        ));
    }
    if let Err(error) = std::fs::rename(&temporary, &file) {
        let _ = std::fs::remove_file(&temporary);
        return report_io_failure(format!(
            "onlyne: cannot rename onto {}: {error}",
            file.display()
        ));
    }
    println!("{}", file.display());
    EXIT_OK
}

/// Parse a string or standard input through the client's own parser.
fn validate(args: &ReportValidateArgs) -> i32 {
    let text = match (&args.text, &args.from) {
        (Some(text), None) => text.clone(),
        (None, Some(source)) => {
            if source != "-" {
                return runtime::usage_error(format!(
                    "onlyne: report validate reads --text or --from -, not a file: {source}"
                ));
            }
            let mut text = String::new();
            if let Err(error) = std::io::stdin().read_to_string(&mut text) {
                return report_io_failure(format!("onlyne: cannot read standard input: {error}"));
            }
            text
        }
        (Some(_), Some(_)) => {
            return runtime::usage_error(
                "onlyne: report validate takes --text or --from, not both",
            );
        }
        (None, None) => {
            return runtime::usage_error(
                "onlyne: report validate needs --text <payload> or --from -",
            );
        }
    };
    print_report(&parse(&text))
}

/// Print one parsed payload: the verdict shape on stdout for a valid report,
/// the exact refusal and the whole grammar on stderr for an invalid one.
fn print_report(payload: &PayloadV2) -> i32 {
    match payload {
        PayloadV2::Done { head, handoffs } => {
            print_verdict("done", "head", head, handoffs);
            EXIT_OK
        }
        PayloadV2::Failed { reason, handoffs } => {
            print_verdict("failed", "reason", reason, handoffs);
            EXIT_OK
        }
        PayloadV2::Blocked { reason, handoffs } => {
            print_verdict("blocked", "reason", reason, handoffs);
            EXIT_OK
        }
        PayloadV2::Invalid { error } => {
            eprintln!("onlyne: {error}");
            eprintln!("{GRAMMAR_V2}");
            EXIT_VALIDATION
        }
    }
}

/// Echo one valid verdict back in the grammar's own spelling, so the author
/// reads what the client will route.
fn print_verdict(kind: &str, label: &str, detail: &str, handoffs: &[Handoff]) {
    println!("kind: {kind}");
    println!("{label}: {detail}");
    for handoff in handoffs {
        match &handoff.text {
            Some(text) => println!("handoff: {} | {text}", handoff.to_role),
            None => println!("handoff: {}", handoff.to_role),
        }
    }
}

/// The report file one verb acts on: `--path` verbatim, else the task's file
/// under the resolved workspace.
fn target_file(
    flags: &GlobalFlags,
    task: Option<&str>,
    path: Option<&Path>,
) -> Result<PathBuf, String> {
    match (path, task) {
        (Some(file), _) => Ok(onlyne_layout::absolute_path(file)),
        (None, Some(task)) => Ok(task_path(flags, task)?.0),
        (None, None) => Err("onlyne: report needs --task <id> or --path <file>".to_string()),
    }
}

/// The task's report file under the workspace, absolute, beside the workspace
/// root it was resolved from.
fn task_path(flags: &GlobalFlags, task: &str) -> Result<(PathBuf, PathBuf), String> {
    if task.is_empty()
        || task == "."
        || task == ".."
        || task.contains('/')
        || task.contains('\\')
        || task.contains('\0')
    {
        return Err(format!(
            "onlyne: --task must name one file without a path: {task}"
        ));
    }
    let workspace = workspace_dir(flags);
    Ok((
        RoleWorkspace::resolve(&workspace).report_path(task),
        workspace,
    ))
}

/// Resolve the workspace directory, following the socket-resolution
/// convention: `--workspace` names the start, an absent flag starts at the
/// current directory, and the walk upward stops at the first directory that
/// owns a `.onlyne/config.toml`. A start that discovers no tree is used
/// verbatim, so the verbs speak about a workspace before its client runs.
fn workspace_dir(flags: &GlobalFlags) -> PathBuf {
    let start = flags
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    match RoleWorkspace::discover(&start) {
        Ok(workspace) => onlyne_layout::absolute_path(workspace.root()),
        Err(_) => onlyne_layout::absolute_path(&start),
    }
}

/// An I/O failure on a path the operator owns: the exact message already names
/// the path and the OS reason, and the code is neither a valid answer nor a
/// usage mistake.
fn report_io_failure(message: impl Into<String>) -> i32 {
    eprintln!("{}", message.into());
    EXIT_ANSWER_FAILED
}
