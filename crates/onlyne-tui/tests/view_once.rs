//! End-to-end: `onlyne-view --once` renders a real journal file.
//!
//! The fixture is the shape `onlyne-client` writes while an ACP turn runs — one
//! JSON object per line, our `onlyne`-keyed records beside the agent's
//! `sessionUpdate` ones — laid down in a temporary workspace whose `.onlyne/run`
//! the viewer never touches. The binary runs as the built executable, so this
//! covers argument parsing, the journal path, the grouping, both verbosity
//! modes, the missing journal, and the `--once` text path.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Receiver;

const TASK: &str = "t-1";

/// One dispatch, a thought, an answer split across chunks, two tool calls whose
/// updates land after the fact, a second thought mid-flight, the agent's own
/// `<status>` stamp, and the closing turn record.
const JOURNAL: &str = concat!(
    "{\"onlyne\":{\"kind\":\"dispatch\",\"task_id\":\"t-1\",\"prose\":\"Add a docstring to greet().\",\"at\":\"2026-09-18T10:00:00Z\"}}\n",
    "{\"sessionUpdate\":\"agent_thought_chunk\",\"content\":{\"type\":\"text\",\"text\":\"The file has one function.\"}}\n",
    "{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"Sure. I will \"}}\n",
    "{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"read the file first.\"}}\n",
    "{\"sessionUpdate\":\"tool_call\",\"toolCallId\":\"call_1\",\"status\":\"pending\",\"title\":\"Read hello.py\",\"kind\":\"read\",\"locations\":[{\"path\":\"hello.py\"}]}\n",
    "{\"sessionUpdate\":\"available_commands_update\",\"availableCommands\":[{\"name\":\"/init\",\"description\":\"a dump nobody wants\"}]}\n",
    "{\"sessionUpdate\":\"tool_call_update\",\"toolCallId\":\"call_1\",\"status\":\"completed\"}\n",
    "{\"sessionUpdate\":\"tool_call\",\"toolCallId\":\"call_2\",\"status\":\"in_progress\",\"title\":\"Edit hello.py\",\"kind\":\"edit\"}\n",
    "{\"sessionUpdate\":\"agent_thought_chunk\",\"content\":{\"type\":\"text\",\"text\":\"The docstring needs the args.\"}}\n",
    "{\"sessionUpdate\":\"tool_call_update\",\"toolCallId\":\"call_2\",\"status\":\"completed\"}\n",
    "{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"Added the docstring.<status>done\"}}\n",
    "{\"onlyne\":{\"kind\":\"turn\",\"task_id\":\"t-1\",\"stop_reason\":\"end_turn\",\"head\":\"Added the docstring.\",\"at\":\"2026-09-18T10:01:00Z\"}}\n",
);

/// The body of the framed render: each content row with its pane borders
/// stripped, which is exactly what the viewer shows.
///
/// The second thought reads as it arrived: after the `Edit` call's line, because
/// a tool entry holds the place its `tool_call` opened and the thinking landed
/// between that call and its update.
const FULL_BODY: &[&str] = &[
    "> Add a docstring to greet().",
    "",
    "~ The file has one function.",
    "",
    "Sure. I will read the file first.",
    "",
    "  [read] Read hello.py (completed)",
    "",
    "  [edit] Edit hello.py (completed)",
    "",
    "~ The docstring needs the args.",
    "",
    "Added the docstring.",
];

const COMPACT_BODY: &[&str] = &[
    "> Add a docstring to greet().",
    "",
    "Sure. I will read the file first.",
    "",
    "  Read hello.py",
    "",
    "  Edit hello.py",
    "",
    "Added the docstring.",
];

fn journal_path(dir: &Path) -> PathBuf {
    dir.join(format!(".onlyne/logs/session-{TASK}.events.jsonl"))
}

/// Lay down the fixture journal and return its path.
fn write_journal(dir: &Path) -> PathBuf {
    let path = journal_path(dir);
    std::fs::create_dir_all(path.parent().expect("logs dir")).expect("logs dir");
    std::fs::write(&path, JOURNAL).expect("journal");
    path
}

fn view(dir: &Path, args: &[&str]) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_onlyne-view"));
    command.args(["--workspace"]);
    command.arg(dir);
    command.args(["--task", TASK]);
    command.args(args);
    let output = command.output().expect("run onlyne-view");
    assert!(
        output.status.success(),
        "exit {} with stderr {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The rendered content rows, borders gone.
fn body(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| line.starts_with('│'))
        .map(|line| line.strip_prefix('│').unwrap_or(line))
        .map(|line| line.strip_suffix('│').unwrap_or(line))
        .map(str::trim_end)
        .collect()
}

#[test]
fn help_names_the_flags_the_client_needs() {
    let output = Command::new(env!("CARGO_BIN_EXE_onlyne-view"))
        .arg("--help")
        .output()
        .expect("run onlyne-view --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for flag in [
        "--workspace <WORKSPACE>",
        "--task <TASK>",
        "--once",
        "--follow",
        "--mode <MODE>",
        "--socket <SOCKET>",
    ] {
        assert!(stdout.contains(flag), "{flag} missing from\n{stdout}");
    }
}

#[test]
fn once_prints_the_turn_in_full_by_default() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_journal(dir.path());
    let full = view(dir.path(), &["--once"]);
    assert_eq!(
        body(&full),
        FULL_BODY,
        "full is user text, answer, reasoning, and one summary line per call\n{full}"
    );
    assert_eq!(
        full,
        view(dir.path(), &["--once", "--mode", "full"]),
        "full is the mode the view opens in"
    );
    assert!(full.contains("onlyne session t-1"), "{full}");
    assert!(full.contains("turn end_turn"), "{full}");
    assert!(
        !full.contains("a dump nobody wants"),
        "available_commands_update is recorded and dropped\n{full}"
    );
    assert!(
        !full.contains("<status>"),
        "the agent's own marker never reaches the operator\n{full}"
    );
    let footer = full
        .lines()
        .find(|line| line.contains("mode full"))
        .expect("the footer names the mode");
    assert!(
        footer.contains(".onlyne/logs/session-t-1.events.jsonl"),
        "the footer names the journal\n{footer}"
    );
    assert!(
        footer.contains("m mode"),
        "the footer teaches the toggle key\n{footer}"
    );
}

#[test]
fn compact_keeps_the_answer_and_the_tool_names() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_journal(dir.path());
    let compact = view(dir.path(), &["--once", "--mode", "compact"]);
    assert_eq!(
        body(&compact),
        COMPACT_BODY,
        "compact is the same reading minus the reasoning and each call's kind \
         and status\n{compact}"
    );
    assert!(compact.contains("mode compact"), "{compact}");
    assert!(
        !compact.contains("The docstring needs the args"),
        "{compact}"
    );
}

#[test]
fn a_workspace_without_the_journal_says_so() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(dir.path().join(".onlyne/logs")).expect("logs dir");
    let text = view(dir.path(), &["--once"]);
    let notice = format!("no journal at {}", journal_path(dir.path()).display());
    assert_eq!(
        body(&text),
        vec![notice.as_str()],
        "the page names the file it looked for rather than showing a blank board"
    );
    assert!(
        !text.contains("Add a docstring"),
        "nothing is invented\n{text}"
    );
}

/// The viewer follows the file it was pointed at: a record appended after the
/// first frame shows up without a restart, and a line the writer has not
/// finished does not.
#[test]
fn follow_prints_the_lines_the_journal_grows_by() {
    use std::io::BufRead;
    let dir = tempfile::tempdir().expect("temp dir");
    let journal = write_journal(dir.path());
    let mut child = Command::new(env!("CARGO_BIN_EXE_onlyne-view"))
        .args(["--once", "--follow"])
        .arg("--workspace")
        .arg(dir.path())
        .args(["--task", TASK])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the follower");

    // A reader thread, so a line that never arrives fails the test instead of
    // hanging it.
    let (sender, receiver) = std::sync::mpsc::channel::<String>();
    let pipe = child.stdout.take().expect("stdout pipe");
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(pipe);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    if sender.send(line).is_err() {
                        return;
                    }
                }
            }
        }
    });

    let frame = drain(&receiver, std::time::Duration::from_secs(2));
    assert!(
        frame.contains("Edit hello.py"),
        "the first frame is the whole journal\n{frame}"
    );
    assert!(
        !frame.contains("a closing note"),
        "the frame precedes the append it does not contain\n{frame}"
    );

    append(
        &journal,
        r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"and a closing note."}}"#,
    );
    let grown = drain(&receiver, std::time::Duration::from_secs(2));
    assert!(
        grown.contains("and a closing note."),
        "the follower printed only {grown:?}"
    );

    // The truncated tail: a line the writer has not finished. It sits on its own
    // line, so the record before it stays read.
    append_partial(
        &journal,
        r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"cut off mid-flow"#,
    );
    let partial = drain(&receiver, std::time::Duration::from_millis(600));
    assert!(
        !partial.contains("cut off mid-flow"),
        "a half-written line is not shown until it is finished\n{partial}"
    );
    append_partial(&journal, r#""}}"#);
    append_partial(&journal, "\n");
    let finished = drain(&receiver, std::time::Duration::from_secs(2));
    assert!(
        finished.contains("cut off mid-flow"),
        "finishing the line lands it without a restart\n{finished}"
    );

    child.kill().expect("stop the follower");
    child.wait().expect("reap the follower");
}

/// Everything that arrives before the pipe goes quiet.
fn drain(receiver: &Receiver<String>, quiet: std::time::Duration) -> String {
    let mut out = String::new();
    while let Ok(line) = receiver.recv_timeout(quiet) {
        out.push_str(&line);
    }
    out
}

/// Write one whole record, the way the client appends it.
fn append(path: &Path, record: &str) {
    append_raw(path, &format!("{record}\n"));
}

/// Write bytes with no line ending: a record the writer has not finished.
fn append_partial(path: &Path, bytes: &str) {
    append_raw(path, bytes);
}

fn append_raw(path: &Path, bytes: &str) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open the journal");
    file.write_all(bytes.as_bytes()).expect("append");
    file.flush().expect("flush");
}
