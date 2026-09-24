//! The `onlyne report` family as its two audiences meet it: an agent writing its
//! own closing report and an operator checking one. Every verb in the family is
//! a filesystem operation and nothing else, so these cases drive the real binary
//! with no daemon anywhere and pin the three things a session can observe from a
//! refusal — the verdict it reads back, the line it is told it broke, and the
//! exit code its script branches on.

use onlyne_proto::payload::{Handoff, PayloadV2, parse};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const EXIT_OK: i32 = 0;
const EXIT_VALIDATION: i32 = 2;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_onlyne"))
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn root_before_suffix(path: &Path, suffix: &Path) -> PathBuf {
    let components: Vec<_> = path.components().collect();
    let suffix_len = suffix.components().count();
    components[..components.len() - suffix_len].iter().collect()
}

/// Run one `report` verb inside a directory of its own. `ONLYNE_SOCKET` is
/// stripped because a session inheriting one from a running client gets the same
/// local answer: the family resolves no socket, and a verb that began to depend on
/// one would fail here first.
fn report(dir: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .current_dir(dir)
        .env_remove("ONLYNE_SOCKET")
        .arg("report")
        .args(args)
        .output()
        .unwrap()
}

/// The whole point of `report write` is that the file it renames into place is
/// the file the client's parser reads, so the round trip is asserted through the
/// parser the client actually runs. A writer that shipped a report the router
/// would cancel over is the failure this case exists to catch.
#[test]
fn write_products_a_report_the_parser_reads_back() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("written-report.md");
    let output = report(
        dir.path(),
        &[
            "write",
            "--path",
            file.to_str().unwrap(),
            "--verdict",
            "done",
            "--head",
            "patch landed and gated",
            "--handoff",
            "reviewer|gate the patch",
            "--handoff",
            "builder",
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "a well-formed report writes: {}",
        stderr_of(&output)
    );
    // The verb answers with the path it wrote, and that path is the artifact an
    // operator opens next. It is read back by name, so a printed path that
    // disagrees with the file on disk fails here.
    let printed = stdout_of(&output).trim().to_string();
    assert_eq!(
        Path::new(&printed).file_name(),
        Some(std::ffi::OsStr::new("written-report.md")),
        "the answer names the report file, got {printed:?}"
    );
    let text = std::fs::read_to_string(&printed).expect("the printed path is a readable file");
    let payload = parse(&text);
    let PayloadV2::Done { head, handoffs } = &payload else {
        panic!("the written file must parse as done, got {payload:?}: {text:?}");
    };
    assert_eq!(head, "patch landed and gated");
    assert_eq!(
        handoffs,
        &vec![
            Handoff {
                to_role: "reviewer".into(),
                text: Some("gate the patch".into()),
            },
            Handoff {
                to_role: "builder".into(),
                text: None,
            },
        ],
        "one line per `--handoff`, in the order given"
    );
    // A bare role is the grammar's shorthand for "hand on the verdict text",
    // which is what the router delivers. The writer must keep that reading.
    assert_eq!(handoffs[1].text_or(head), head);
    assert_eq!(payload.head_kind(), Some("done"));

    // The same file read back through `check` is the report's whole end to end:
    // the writer's parts in, the client's verdict out, one rendered line per
    // field the operator's script reads. A drift between `--verdict done` and
    // the printed kind, or between the two handoff spellings, shows here.
    let checked = report(dir.path(), &["check", "--path", file.to_str().unwrap()]);
    assert_eq!(
        checked.status.code(),
        Some(EXIT_OK),
        "the file the verb wrote checks clean: {}",
        stderr_of(&checked)
    );
    let rendered = stdout_of(&checked);
    for line in [
        "kind: done",
        "head: patch landed and gated",
        "handoff: reviewer | gate the patch",
        "handoff: builder",
    ] {
        assert!(
            rendered.lines().any(|printed| printed == line),
            "the check answer prints {line:?} on its own line, got:\n{rendered}"
        );
    }
}

/// `--head` carries one line, and the grammar reads one line per prefix. A head
/// holding a newline would split the report into a verdict plus a line no prefix
/// owns, so the client would cancel the task over a report its own tool produced.
/// The refusal lands before anything touches the target path, which is how a
/// re-run over a live report stays safe.
#[test]
fn write_refuses_a_head_spanning_two_lines() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("existing-report.md");
    let draft = "hop-done: earlier draft\n";
    std::fs::write(&file, draft).unwrap();
    let output = report(
        dir.path(),
        &[
            "write",
            "--path",
            file.to_str().unwrap(),
            "--verdict",
            "failed",
            "--head",
            "build broke\nthe tests did not run",
        ],
    );
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    assert!(
        stderr_of(&output).contains("--head must be one line"),
        "the refusal names the flag that broke: {}",
        stderr_of(&output)
    );
    assert!(
        stdout_of(&output).is_empty(),
        "a refused write prints no path"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        draft,
        "a refused write leaves the report that was already in the slot"
    );

    // A head that only ends in a newline is what a shell substitution hands
    // over, and the writer trims it, so the file keeps the one line the grammar
    // reads. `report check` is the reader that proves it.
    let trimmed = dir.path().join("trailing-newline.md");
    let output = report(
        dir.path(),
        &[
            "write",
            "--path",
            trimmed.to_str().unwrap(),
            "--verdict",
            "done",
            "--head",
            "landed\n",
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "a trailing newline reaches the file as a one-line report: {}",
        stderr_of(&output)
    );
    assert_eq!(
        parse(&std::fs::read_to_string(&trimmed).unwrap()),
        PayloadV2::Done {
            head: "landed".into(),
            handoffs: Vec::new(),
        },
        "the written file is a one-line report"
    );
}

/// The role token is one word: the router looks it up verbatim. A handoff built
/// from prose would otherwise ship a file whose recipient is a sentence, and the
/// delivery would come back as an unknown role after the task already settled.
#[test]
fn write_refuses_a_handoff_role_containing_spaces() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("spaced-role.md");
    let output = report(
        dir.path(),
        &[
            "write",
            "--path",
            file.to_str().unwrap(),
            "--verdict",
            "done",
            "--head",
            "work done",
            "--handoff",
            "bad role with spaces",
        ],
    );
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("needs one role token"),
        "the refusal says what a handoff line needs: {stderr}"
    );
    assert!(
        stderr.contains("bad role with spaces"),
        "the refusal echoes the argument that broke: {stderr}"
    );
    assert!(!file.exists(), "a refused write leaves no report file");
}

/// A file the grammar cannot read must name the line it stopped on, because the
/// author is an agent in a session with no editor open: `line 2` is the whole
/// diagnostic. The refusal rides stderr with the grammar behind it, and the exit
/// code is the shared local-validation code.
#[test]
fn check_names_the_line_an_invalid_file_broke_on() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("invalid-report.md");
    std::fs::write(
        &file,
        "hop-done: patch landed\nhop-skipped: nothing here\nhandoff: reviewer\n",
    )
    .unwrap();
    let output = report(dir.path(), &["check", "--path", file.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    let stderr = stderr_of(&output);
    // The refusal is the first thing on stderr. The grammar block that follows
    // it quotes line shapes of its own, so an error line found anywhere in the
    // text can be a match against the help.
    let first = stderr.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("onlyne: line 2:"),
        "the refusal is the first stderr line and names the line it stopped on: {stderr}"
    );
    assert!(
        first.contains("unknown report prefix"),
        "and the category of the break: {first}"
    );
    assert!(
        stderr.contains("Result report grammar (payload-v2)"),
        "the grammar follows the refusal so the author can fix it blind: {stderr}"
    );
    assert!(
        stdout_of(&output).is_empty(),
        "an invalid report prints no verdict"
    );
}

/// The most common reason `check` finds nothing is a report that was never
/// written, and a task parked waiting on one looks identical to a slow session.
/// The absent file answers with its own word and the validation code.
#[test]
fn check_reports_an_absent_file_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("never-written.md");
    let output = report(dir.path(), &["check", "--path", file.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(EXIT_VALIDATION));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("absent"),
        "a missing report is named as missing: {stderr}"
    );
    assert!(
        stderr.contains("never-written.md"),
        "and the refusal carries the path it looked at: {stderr}"
    );
    assert!(
        stdout_of(&output).is_empty(),
        "the absent case prints no verdict"
    );
}

/// `report validate --from -` runs the same parser over a heredoc, which is how
/// an agent checks a draft before it renames a file into the task's slot. The
/// stdin path and the file path share `print_report`, so this case pins the
/// verdict shape the author reads back: kind, head, one line per handoff.
#[test]
fn validate_reads_standard_input_and_prints_the_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(bin())
        .current_dir(dir.path())
        .env_remove("ONLYNE_SOCKET")
        .args(["report", "validate", "--from", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(b"hop-done: draft checks out\nhandoff: reviewer | gate the draft\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "a valid draft answers 0: {}",
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("kind: done"),
        "the verdict kind is echoed: {stdout}"
    );
    assert!(
        stdout.contains("head: draft checks out"),
        "the head is echoed: {stdout}"
    );
    assert!(
        stdout.contains("handoff: reviewer | gate the draft"),
        "every handoff line is echoed in the grammar's own spelling: {stdout}"
    );
    assert!(
        stderr_of(&output).is_empty(),
        "a valid draft prints no refusal"
    );
}

/// `report path` is the one shipped command naming the four files a running task
/// leaves on disk, and an ACP session owns no terminal to find them another way, so
/// these four lines are the whole visible surface of a task for both the agent and
/// the operator. This case pins everything a caller can observe: the four labels in
/// order, the relative shape of each path under the workspace, and the fact that all
/// four hang off one workspace root. Paths are compared as platform paths by their
/// trailing components because a temporary tree is reachable on macOS through both
/// `/var` and `/private/var`, and the verb prints the canonical spelling. Windows
/// canonical paths retain their verbatim prefix in those components.
#[test]
fn path_prints_the_four_labeled_surfaces_of_one_task() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_str().unwrap();
    let task = "close-out";
    let output = report(
        dir.path(),
        &["path", "--task", task, "--workspace", workspace],
    );
    assert_eq!(
        output.status.code(),
        Some(EXIT_OK),
        "a well-formed task id answers 0: {}",
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        4,
        "the verb prints exactly four lines:\n{stdout}"
    );

    let (label_report, report_path) = lines[0]
        .split_once(": ")
        .expect("the report line is labeled");
    let (label_log, log_path) = lines[1].split_once(": ").expect("the log line is labeled");
    let (label_events, events_path) = lines[2]
        .split_once(": ")
        .expect("the events line is labeled");
    let (label_content, content_path) = lines[3]
        .split_once(": ")
        .expect("the content line is labeled");
    assert_eq!(label_report, "report", "the first line is the report file");
    assert_eq!(label_log, "log", "the second line is the session log");
    assert_eq!(
        label_events, "events",
        "the third line is the event journal"
    );
    assert_eq!(
        label_content, "content",
        "the fourth line is the content index"
    );

    let tail_report = Path::new(".onlyne").join("out").join(format!("{task}.md"));
    let tail_log = Path::new(".onlyne")
        .join("logs")
        .join(format!("session-{task}.log"));
    let tail_events = Path::new(".onlyne")
        .join("logs")
        .join(format!("session-{task}.events.jsonl"));
    let tail_content = Path::new(".onlyne")
        .join("logs")
        .join("content.index.jsonl");
    assert!(
        Path::new(report_path).ends_with(&tail_report),
        "the report line names the task's closing file: {report_path}"
    );
    assert!(
        Path::new(log_path).ends_with(&tail_log),
        "the log line names the rendered transcript: {log_path}"
    );
    assert!(
        Path::new(events_path).ends_with(&tail_events),
        "the events line names the raw journal: {events_path}"
    );
    assert!(
        Path::new(content_path).ends_with(&tail_content),
        "the content line names the role-wide index: {content_path}"
    );

    let root_report = root_before_suffix(Path::new(report_path), &tail_report);
    let root_log = root_before_suffix(Path::new(log_path), &tail_log);
    let root_events = root_before_suffix(Path::new(events_path), &tail_events);
    let root_content = root_before_suffix(Path::new(content_path), &tail_content);
    assert!(
        !root_report.as_os_str().is_empty(),
        "the shared workspace root is a real directory path"
    );
    assert!(
        [root_log, root_events, root_content]
            .iter()
            .all(|r| *r == root_report),
        "all four paths hang off one workspace root: {}",
        root_report.display()
    );
}

/// A task id is exactly one file name: the report it selects is
/// `<workspace>/.onlyne/out/<task>.md`, so a value carrying a path separator or
/// standing empty would name a file no client can ever open. The verb refuses both
/// before it prints any line, and the refusal names the `--task` flag the caller
/// broke alongside the rule it enforces.
#[test]
fn path_refuses_a_task_id_that_is_not_one_file_name() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_str().unwrap();
    for bad in ["nested/task", ""] {
        let output = report(
            dir.path(),
            &["path", "--task", bad, "--workspace", workspace],
        );
        assert_eq!(
            output.status.code(),
            Some(EXIT_VALIDATION),
            "a malformed task id is a usage error: {bad:?}"
        );
        let stderr = stderr_of(&output);
        assert!(
            stderr.contains("--task"),
            "the refusal names the flag it broke: {stderr}"
        );
        assert!(
            stderr.contains("must name one file without a path"),
            "the refusal states the rule the value broke: {stderr}"
        );
        assert!(
            stdout_of(&output).is_empty(),
            "a refused task id prints no path lines: {stderr}"
        );
    }
}
