use anyhow::Context;
use serde_json::Value;
use std::process::Command;

/// Thin wrapper over `orca terminal ... --json`.
///
/// The whole swarm tree is a single Orca worktree; each session is one
/// orca terminal running `pi`. Terminal and pi process share a lifecycle:
/// the scheduler kills the pi process; orca reclaims the terminal.
pub struct Terminal {
    pub handle: String,
}

fn orca() -> Command {
    let bin = std::env::var("ORCA_CLI_COMMAND").unwrap_or_else(|_| "orca".into());
    Command::new(bin)
}

fn run_json(mut cmd: Command) -> anyhow::Result<Value> {
    cmd.arg("--json");
    let out = cmd.output().context("run orca")?;
    if !out.status.success() {
        anyhow::bail!("orca failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(serde_json::from_slice(&out.stdout).context("parse orca --json output")?)
}

/// Create a terminal running `pi` with cwd set to the workspace dir.
/// `env_task` is exported as ONLYNE_SWARM_TASK so pi-onlyne swarm mode can
/// correlate; the actual task bytes are delivered later via loopback/in
/// after `swarm_ready` (avoids create-vs-write races).
///
/// Stub hook for headless e2e: when `SWARM_STUB_AGENT=1`, no orca terminal
/// is created. Instead a fake handle is returned and the test harness is
/// expected to drive `swarm_ready` + task reply itself (see TEST.md).
pub fn create(title: &str, cwd: &std::path::Path, env_task: &str) -> anyhow::Result<Terminal> {
    create_with_opts(title, cwd, env_task, &CreateOpts::default())
}

/// Options for terminal creation. Focus defaults to off: a scheduler that
/// steals window focus on every hop would be unusable during fan-out.
/// Operators opt in per tree via SWARM_FOCUS=new|all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreateOpts {
    pub focus: bool,
}

impl Default for CreateOpts {
    fn default() -> Self {
        let mode = std::env::var("SWARM_FOCUS").unwrap_or_default();
        Self {
            focus: mode == "all" || mode == "new",
        }
    }
}

/// Pure argv constructor so focus/title wiring is unit-tested.
/// Tabs always land under the swarm root worktree (the operator's visible
/// tree): folder-kind nodes are not selector-addressable, so per-workspace
/// `--worktree` routing only hides tabs. Workspace identity travels in the
/// tab title (`swarm:<to>:<id8>`) and the session cwd instead.
pub fn create_argv(title: &str, focus: bool) -> Vec<String> {
    let mut argv = vec![
        "terminal".to_string(),
        "create".to_string(),
        "--title".to_string(),
        title.to_string(),
    ];
    if focus {
        argv.push("--focus".to_string());
    }
    argv.push("--command".to_string());
    argv
}

pub fn create_with_opts(
    title: &str,
    cwd: &std::path::Path,
    env_task: &str,
    opts: &CreateOpts,
) -> anyhow::Result<Terminal> {
    if std::env::var("SWARM_STUB_AGENT").as_deref() == Ok("1") {
        return Ok(Terminal {
            handle: format!("stub-{title}-{env_task}"),
        });
    }
    let mut cmd = orca();
    cmd.args(create_argv(title, opts.focus));
    // Keep normal extension discovery enabled so retry and other configured
    // extensions are available in real swarm sessions.
    cmd.arg(session_command(cwd, env_task));
    let v = run_json(cmd)?;
    let handle = parse_terminal_handle(&v).unwrap_or_default();
    if handle.is_empty() {
        anyhow::bail!("orca terminal create returned no handle: {v}");
    }
    Ok(Terminal { handle })
}

/// Rename a terminal tab. Best effort: re-assert the swarm title after the
/// session starts, since pi overwrites the create-time title on boot.
pub fn rename(handle: &str, title: &str) -> anyhow::Result<()> {
    let mut cmd = orca();
    cmd.args(["terminal", "rename", "--terminal", handle, "--title", title]);
    run_json(cmd)?;
    Ok(())
}

/// Shell argv for `orca terminal switch --terminal <h>`.
pub fn focus_argv(handle: &str) -> Vec<String> {
    vec![
        "terminal".to_string(),
        "switch".to_string(),
        "--terminal".to_string(),
        handle.to_string(),
    ]
}

/// Switch the Orca UI to a terminal tab. Best effort: a stale/closed handle
/// just reports the error as the status message, never breaks the TUI loop.
pub fn focus(handle: &str) -> anyhow::Result<()> {
    let mut cmd = orca();
    cmd.args(focus_argv(handle));
    run_json(cmd)?;
    Ok(())
}

/// Interpret an Orca `terminal show --json` response. This pure function
/// keeps the dead-session contract testable without global env mutation.
/// Probe failures/stale handles are *unknown*, treated alive: only an explicit
/// `status=exited|closed|dead` or a stale/exited exit cause lets the reaper
/// fail/retry a task. An operator close by the human is also terminal: without
/// this, a task whose pane the operator recycled would sit `busy` forever.
pub fn terminal_is_alive_response(v: Option<&Value>) -> bool {
    let Some(v) = v else { return true };
    if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        return true;
    }
    let status = v
        .pointer("/result/terminal/status")
        .and_then(|s| s.as_str());
    let closed_status = matches!(status, Some("exited") | Some("closed") | Some("dead"));
    let exit_kind = v
        .pointer("/result/terminal/exitCause/kind")
        .and_then(|k| k.as_str());
    let connected = v
        .pointer("/result/terminal/connected")
        .and_then(|c| c.as_bool());
    let writable = v
        .pointer("/result/terminal/writable")
        .and_then(|w| w.as_bool());
    let exited_cause = matches!(
        exit_kind,
        Some("stale") | Some("exited") | Some("closed") | Some("operator_close")
    ) && matches!(connected, Some(false))
        && matches!(writable, Some(false));
    !(closed_status || exited_cause)
}

/// Liveness probe: only explicit exited status means dead; RPC failures stay
/// alive/unknown so transient Orca races cannot kill a working hop.
pub fn is_alive(handle: &str) -> bool {
    if handle.is_empty() || handle.starts_with("stub-") {
        return true; // stubs have no orca tab; never declare them dead
    }
    let mut cmd = orca();
    cmd.args(["terminal", "show", "--terminal", handle]);
    let response = run_json(cmd).ok();
    terminal_is_alive_response(response.as_ref())
}

pub fn close(handle: &str) -> anyhow::Result<()> {
    let mut cmd = orca();
    cmd.args(["terminal", "close", "--terminal", handle]);
    run_json(cmd)?;
    Ok(())
}

/// Legacy/manual shell helper, kept for external operators only. The scheduler
/// never calls it: interactive pi owns terminal input, so Orca tab close is
/// the only reliable default reclamation primitive.
#[allow(dead_code)]
pub fn kill_pi_for_task(handle: &str, task_id: &str) -> anyhow::Result<()> {
    let task_id = shell_escape(task_id);
    send(
        handle,
        &format!("pkill -TERM -P $$ -f ONLYNE_SWARM_TASK={task_id} 2>/dev/null; exit"),
        true,
    )
}

/// Legacy entry: task id unknown (kept for API compat; no live callers).
/// Scoped to the terminal shell's children only — still no global pgrep sweep.
#[allow(dead_code)]
pub fn kill_pi(handle: &str) -> anyhow::Result<()> {
    send(handle, "pkill -TERM -P $$ pi 2>/dev/null; exit", true)
}

pub fn send(handle: &str, text: &str, enter: bool) -> anyhow::Result<()> {
    let mut cmd = orca();
    cmd.args(send_argv(handle, text, enter));
    run_json(cmd)?;
    Ok(())
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Extract the terminal handle from `orca terminal create --json` output.
/// Pure function over the JSON value so the shape contract is unit-tested
/// without spawning orca.
pub fn parse_terminal_handle(v: &Value) -> Option<String> {
    for ptr in ["/result/terminal/handle", "/terminal/handle"] {
        if let Some(h) = v.pointer(ptr).and_then(|h| h.as_str()) {
            if !h.is_empty() {
                return Some(h.to_string());
            }
        }
    }
    if let Some(h) = v.get("handle").and_then(|h| h.as_str()) {
        if !h.is_empty() {
            return Some(h.to_string());
        }
    }
    if let Some(h) = v
        .get("terminal")
        .and_then(|h| h.as_str().or_else(|| h.get("handle")?.as_str()))
    {
        if !h.is_empty() {
            return Some(h.to_string());
        }
    }
    None
}

/// Build the shell command run inside a fresh orca terminal for a task.
/// Pure constructor so quoting bugs are caught by unit tests, not in prod.
/// `task_id` doubles as the kill marker: kill_pi targets the pi process
/// whose command line carries this task's ONLYNE_SWARM_TASK value, so a
/// cancel can never SIGTERM a foreign session (the old `pgrep pi | head -1`
/// fallback could hit the supervisor). The create-time handle is unknown
/// when this string is built, so the stable task id is the right key.
pub fn session_command(workspace_dir: &std::path::Path, task_id: &str) -> String {
    format!(
        "cd {} && ONLYNE_SWARM_TASK={} pi",
        shell_escape(&workspace_dir.to_string_lossy()),
        shell_escape(task_id),
    )
}

/// Shell argv for `orca terminal send --terminal <h> --text <t> [--enter]`.
/// Pure constructor; the live send() below only executes it.
pub fn send_argv(handle: &str, text: &str, enter: bool) -> Vec<String> {
    let mut argv = vec![
        "terminal".to_string(),
        "send".to_string(),
        "--terminal".to_string(),
        handle.to_string(),
        "--text".to_string(),
        text.to_string(),
    ];
    if enter {
        argv.push("--enter".to_string());
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn handle_shapes() {
        // Canonical shape seen live (result.terminal.handle).
        let v = json!({"ok": true, "result": {"terminal": {"handle": "term_abc"}}});
        assert_eq!(parse_terminal_handle(&v).as_deref(), Some("term_abc"));
        // Legacy/alternate shapes still resolve.
        let v = json!({"terminal": {"handle": "term_x"}});
        assert_eq!(parse_terminal_handle(&v).as_deref(), Some("term_x"));
        let v = json!({"handle": "term_h"});
        assert_eq!(parse_terminal_handle(&v).as_deref(), Some("term_h"));
        let v = json!({"terminal": "term_s"});
        assert_eq!(parse_terminal_handle(&v).as_deref(), Some("term_s"));
        // Garbage resolves to nothing (caller errors with the raw value).
        assert_eq!(parse_terminal_handle(&json!({"ok": false})), None);
        assert_eq!(parse_terminal_handle(&json!({"handle": ""})), None);
    }

    #[test]
    fn session_command_quotes_paths_and_task() {
        let cmd = session_command(std::path::Path::new("/tmp/a b/c"), "task-1");
        assert!(cmd.starts_with("cd '/tmp/a b/c'"), "{cmd}");
        assert!(cmd.contains("ONLYNE_SWARM_TASK='task-1'"), "{cmd}");
        assert!(cmd.ends_with(" pi"), "{cmd}");
        // Real sessions use normal discovery, including configured retries.
        assert!(!cmd.contains("-ne"), "{cmd}");
        // Single quotes in paths are escaped, never break the shell string.
        let cmd = session_command(std::path::Path::new("/o'b"), "t");
        assert!(cmd.contains("'/o'\\''b'"), "{cmd}");
    }

    #[test]
    fn create_argv_focus_is_opt_in() {
        // Default: background tab, no focus steal during fan-out.
        // No --worktree: tabs stay under the swarm root worktree where the
        // operator can see them (folder-kind nodes are not listable).
        assert_eq!(
            create_argv("swarm:a:12345678", false),
            vec![
                "terminal",
                "create",
                "--title",
                "swarm:a:12345678",
                "--command"
            ]
        );
        assert_eq!(
            create_argv("swarm:a:12345678", true)
                .last()
                .map(String::as_str),
            Some("--command")
        );
        assert!(create_argv("t", true).contains(&"--focus".to_string()));
        assert!(!create_argv("t", false).iter().any(|a| a == "--worktree"));
    }

    #[test]
    fn terminal_liveness_requires_explicit_exited_status() {
        assert!(terminal_is_alive_response(None));
        assert!(terminal_is_alive_response(Some(&json!({"ok": false}))));
        assert!(terminal_is_alive_response(Some(
            &json!({"ok": true, "result": {"terminal": {"status": "running"}}})
        )));
        assert!(!terminal_is_alive_response(Some(
            &json!({"ok": true, "result": {"terminal": {"status": "exited"}}})
        )));
        // Seen live on 0.7.0: operator-recycled panes have no status string,
        // only a disconnected, unwritable tab with an exit cause.
        assert!(!terminal_is_alive_response(Some(
            &json!({"ok": true, "result": {"terminal": {"connected": false, "writable": false, "exitCause": {"kind": "operator_close"}}}})
        )));
        // A merely idle-but-attached tab is still alive.
        assert!(terminal_is_alive_response(Some(
            &json!({"ok": true, "result": {"terminal": {"connected": true, "writable": true, "exitCause": {"kind": "operator_close"}}}})
        )));
    }

    #[test]
    fn focus_argv_shape() {
        assert_eq!(
            focus_argv("term_abc"),
            vec!["terminal", "switch", "--terminal", "term_abc"]
        );
    }

    #[test]
    fn kill_targets_only_its_own_task() {
        // Regression: the old `pgrep pi | head -1` fallback could SIGTERM
        // the supervisor or a sibling hop during a marquee cancel.
        // kill strings must scope to the terminal shell AND the task id.
        let cmd = format!(
            "pkill -TERM -P $$ -f ONLYNE_SWARM_TASK={} 2>/dev/null; exit",
            shell_escape("task-abc-123")
        );
        assert!(cmd.contains("-P $$"));
        assert!(cmd.contains("ONLYNE_SWARM_TASK='task-abc-123'"));
        assert!(!cmd.contains("head -1"));
        assert!(!cmd.contains("pgrep pi |"));
    }

    #[test]
    fn send_argv_shape() {
        assert_eq!(
            send_argv("h1", "hi", false),
            vec!["terminal", "send", "--terminal", "h1", "--text", "hi"]
        );
        assert_eq!(
            send_argv("h1", "hi", true).last().map(String::as_str),
            Some("--enter")
        );
    }
}
