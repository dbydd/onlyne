//! `onlyne ls`: every Onlyne surface registered on this machine.
//!
//! The answer comes from the runtime directory's `<digest>.json` registration
//! files through [`onlyne_wire::socket::list_registrations`], which already skips
//! files that are not registrations. Nothing here re-filters, and nothing here
//! decides whether an entry is live: a reader asking what is running needs the
//! leftovers as much as the current, so liveness is reported as two separate
//! facts — whether the socket file is there, and whether the pid answers — and
//! no entry is ever dropped for looking stale.

use onlyne_wire::socket::{
    RegistrationFile, RegistrationKind, list_registrations, runtime_dir_path,
};
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::flags::GlobalFlags;
use crate::runtime::{EXIT_ANSWER_FAILED, EXIT_OK};

/// What the two liveness columns mean, spelled at the foot of `onlyne ls
/// --help` so nobody reads them as a verdict.
pub const LS_AFTER_HELP: &str = "\
`SOCKET` reports whether the endpoint file the registration names is present on
disk, and `ALIVE` reports whether that process id still answers. They are reported
separately and neither is decided for you: a file left behind by a killed daemon
reads `yes`/`no`, and a pid that was recycled reads `yes` for a process that is
not the one that registered. Judge freshness from the two facts together.

Nothing is filtered out. An entry whose daemon is gone stays in the listing,
because the question being asked is what is on this machine, not what is
currently answering. A registration file that does not parse is skipped by the
reader rather than aborting the listing, and the count goes to stderr.

`ROOT` is the canonical owner tree: pass it to `--server-root` or `--workspace`
to address that surface. `RUNTIME` names the client's session backend, and
`PLACEMENT` names where the machine displays it.";

/// One row of the listing, and the `--json` shape: the registration's own fields
/// plus the two liveness facts and the socket path they describe.
#[derive(Debug, Serialize)]
struct Row {
    kind: &'static str,
    role: Option<String>,
    root: PathBuf,
    pid: u32,
    version: String,
    runtime: Option<String>,
    placement: Option<String>,
    socket: PathBuf,
    socket_exists: bool,
    pid_alive: bool,
}

/// Print the listing, and exit 0 whether it is full or empty.
///
/// An unreadable runtime directory is the one failure here, and it is a runtime
/// error (exit 1): an absent directory is already an empty machine, because
/// [`list_registrations`] answers that with no registrations rather than an
/// error.
pub fn run(flags: &GlobalFlags) -> i32 {
    let dir = runtime_dir_path();
    let rows = match read_rows() {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!("onlyne: cannot read {}: {error}", dir.display());
            return EXIT_ANSWER_FAILED;
        }
    };

    if let Some(skipped) = skipped_count(&dir, rows.len()) {
        eprintln!(
            "onlyne: skipped {skipped} unreadable registration file(s) in {}",
            dir.display()
        );
    }

    if flags.json {
        println!("{}", crate::render::encode(flags.pretty, &rows));
    } else if rows.is_empty() {
        println!("no registrations in {}", dir.display());
    } else {
        print_table(&rows);
    }
    EXIT_OK
}

/// Every registration in the runtime directory, as rows, in the order the
/// reader returned them: sorted by file path, so one root is one line and the
/// order does not move between two runs of an unchanged machine.
fn read_rows() -> std::io::Result<Vec<Row>> {
    Ok(list_registrations()?
        .iter()
        .map(|(path, reg)| row(path, reg))
        .collect())
}

/// One row: the registration's own facts, the socket the entry's own file name
/// names (`.json` one leaf from `.sock`), and both liveness facts.
fn row(path: &Path, reg: &RegistrationFile) -> Row {
    let socket = path.with_extension("sock");
    Row {
        kind: match reg.kind {
            RegistrationKind::Server => "server",
            RegistrationKind::Client => "client",
        },
        role: reg.role.clone(),
        root: reg.root.clone(),
        pid: reg.pid,
        version: reg.version.clone(),
        runtime: reg.runtime.clone(),
        placement: reg.placement.clone(),
        socket_exists: std::fs::symlink_metadata(&socket).is_ok(),
        pid_alive: pid_alive(reg.pid),
        socket,
    }
}

/// How many `.json` files in `dir` the reader could not turn into a row, so the
/// skipping the listing already did is visible without a second filter on the
/// rows themselves. `None` when nothing was skipped, which is the ordinary case.
fn skipped_count(dir: &Path, listed: usize) -> Option<usize> {
    let entries = std::fs::read_dir(dir).ok()?;
    let candidates = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .to_str()
                .is_some_and(|name| name.ends_with(".json"))
        })
        .count();
    (candidates > listed).then_some(candidates - listed)
}

/// One header line, then one line per entry, columns padded to the widest cell
/// and `ROOT` given the room it needs: it is the field a person copies into the
/// next command, so it is the one column that must not be truncated.
fn print_table(rows: &[Row]) {
    let root_width = rows
        .iter()
        .map(|row| row.root.display().to_string().chars().count())
        .max()
        .unwrap_or(0)
        .max(MIN_ROOT_WIDTH);
    let pid_width = rows
        .iter()
        .map(|row| row.pid.to_string().len())
        .max()
        .unwrap_or(0);

    println!(
        "{:<6} {:<8} {:<root_width$}  {:>pid_width$}  {:<7} {:<7} {:<7} {:<9} VERSION",
        "KIND", "ROLE", "ROOT", "PID", "SOCKET", "ALIVE", "RUNTIME", "PLACEMENT",
    );
    for row in rows {
        println!(
            "{:<6} {:<8} {:<root_width$}  {:>pid_width$}  {:<7} {:<7} {:<7} {:<9} {}",
            row.kind,
            cell(row.role.as_deref()),
            row.root.display(),
            row.pid,
            yes_no(row.socket_exists),
            yes_no(row.pid_alive),
            cell(row.runtime.as_deref()),
            cell(row.placement.as_deref()),
            row.version,
        );
    }
}

/// The width floor for `ROOT`, so a listing of short roots in one narrow
/// terminal still reads as a table.
const MIN_ROOT_WIDTH: usize = 24;

fn cell(value: Option<&str>) -> &str {
    value.unwrap_or("-")
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// Whether `pid` still answers a liveness probe.
///
/// Signal 0 answers the question without touching the process, so this reports
/// the fact and nothing about it. The answer is `true` for a pid this user may
/// not signal, which is why the listing calls the column a report rather than a
/// verdict.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    use std::process::{Command, Stdio};
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE as u32
    }
}
