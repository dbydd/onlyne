//! The journal an ACP session keeps on disk: the raw update stream and the
//! rendered log, plus the exact drain that turns one turn's pipe traffic into
//! both.
//!
//! Nothing here decides an outcome. It reads what the agent sent, formats it,
//! and appends it; a write that fails is a warning, never a failed turn.

use crate::content::ContentWriter;
use chrono::SecondsFormat;
use onlyne_acp::{Event, Update};
use onlyne_layout::RoleWorkspace;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

pub(super) fn or_dash(text: String) -> String {
    if text.is_empty() {
        "-".to_string()
    } else {
        text
    }
}

/// Everything one turn left on the pipe, split by who reads it.
#[derive(Default)]
pub(super) struct Drained {
    /// Raw update params, verbatim, in arrival order, for the JSONL journal.
    pub(super) lines: Vec<Value>,
    /// The same updates rendered for the operator's log.
    pub(super) log: String,
    /// Assistant text, concatenated.
    pub(super) message: String,
    /// The exit detail, when the process left during the turn.
    pub(super) exited: Option<String>,
}

/// Read what the agent sent for one session.
///
/// The crate's single reader thread routes a notification before it wakes the
/// parked request that follows it on the wire, so once `prompt` returns every
/// update of that turn is already in this receiver and one drain is exact: no
/// collector thread and no wait-and-see heuristic. Fan-out is per process, so
/// another session's updates are dropped here — they belong to that turn's
/// journal and that turn's head.
pub(super) fn drain(events: &Receiver<Event>, session_id: &str) -> Drained {
    let mut drained = Drained::default();
    let mut render = Render::default();
    while let Ok(event) = events.try_recv() {
        match event {
            Event::Update {
                session_id: id,
                update,
            } if id == session_id => {
                drained.lines.push(update.raw().clone());
                match &update {
                    Update::AgentMessageChunk(chunk) => {
                        drained.message.push_str(&chunk.text);
                        render.chunk(MESSAGE, &chunk.text);
                    }
                    Update::AgentThoughtChunk(chunk) => render.chunk(THOUGHT, &chunk.text),
                    Update::ToolCall(value) | Update::ToolCallUpdate(value) => {
                        render.line(tool_line(&update, value));
                    }
                    // `available_commands_update` is a multi-kilobyte dump of the
                    // agent's slash commands, the echoed user message repeats what
                    // this client just sent, and a plan wholesale-replaces a
                    // previous one: all three ride the JSONL and leave the log.
                    Update::UserMessageChunk(_) | Update::Plan(_) | Update::Unknown(_) => {}
                }
            }
            Event::Exited { detail } => drained.exited = Some(detail),
            _ => {}
        }
    }
    render.finish(&mut drained.log);
    drained
}

const MESSAGE: &str = "message";
const THOUGHT: &str = "thought";

/// Builds the rendered log: consecutive chunks of one kind become one block, and
/// a thought block is prefixed so an operator can tell reasoning from the answer.
#[derive(Default)]
struct Render {
    pending: Option<(&'static str, String)>,
    out: String,
}

impl Render {
    fn chunk(&mut self, kind: &'static str, text: &str) {
        match &mut self.pending {
            Some((found, buffer)) if *found == kind => buffer.push_str(text),
            _ => {
                self.flush();
                self.pending = Some((kind, text.to_string()));
            }
        }
    }

    fn line(&mut self, line: String) {
        self.flush();
        self.out.push_str(&line);
        self.out.push('\n');
    }

    fn flush(&mut self) {
        let Some((kind, text)) = self.pending.take() else {
            return;
        };
        let text = strip_status(&text);
        let text = text.trim_end();
        if text.is_empty() {
            return;
        }
        if kind == THOUGHT {
            for line in text.lines() {
                self.out.push_str("> ");
                self.out.push_str(line);
                self.out.push('\n');
            }
            return;
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn finish(mut self, into: &mut String) {
        self.flush();
        into.push_str(&self.out);
    }
}

fn tool_line(update: &Update, value: &Value) -> String {
    let field = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let id = or_dash(field("toolCallId"));
    match update {
        Update::ToolCall(_) => format!(
            "tool {id} {} kind={}",
            or_dash(field("title")),
            or_dash(field("kind"))
        ),
        _ => format!("tool {id} status={}", or_dash(field("status"))),
    }
}

/// Drop the `<status>…</status>` markers an agent stamps into its own answer.
///
/// An agent closes the marker with a matching tag and, in a stream cut short,
/// leaves it open — an unclosed marker runs to the end of the block, which is
/// where the agent puts it.
fn strip_status(text: &str) -> String {
    const OPEN: &str = "<status>";
    const CLOSE: &str = "</status>";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(OPEN) {
        out.push_str(&rest[..open]);
        rest = &rest[open + OPEN.len()..];
        match rest.find(CLOSE) {
            Some(close) => rest = &rest[close + CLOSE.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The line the receiving role reads as this turn's answer: the last non-empty
/// line of the agent's closing message, with its status markers gone.
pub(super) fn completion_head(message: &str) -> Option<String> {
    let stripped = strip_status(message);
    if let Some(line) = stripped
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
    {
        return Some(line.to_string());
    }
    // A turn whose only text was a status marker still said something: report the
    // word the agent put in the marker rather than nothing at all.
    let marker = message.rfind("<status>")?;
    let word = stripped_at(message, marker);
    (!word.is_empty()).then_some(word)
}

/// The text after a status marker, markers stripped and whitespace trimmed.
fn stripped_at(message: &str, from: usize) -> String {
    strip_status(&message[from..]).trim().to_string()
}

/// One turn's journal files, named for the task it ran.
pub(super) struct Journal {
    workspace: PathBuf,
    task_id: String,
    session_id: String,
    pub(super) log: PathBuf,
    pub(super) events: PathBuf,
    content: ContentWriter,
}

impl Journal {
    pub(super) fn new(
        workdir: &Path,
        task_id: &str,
        session_id: &str,
        content: ContentWriter,
    ) -> Self {
        let layout = RoleWorkspace::resolve(workdir);
        Journal {
            workspace: workdir.to_path_buf(),
            task_id: task_id.to_string(),
            session_id: session_id.to_string(),
            log: layout.session_log_path(task_id),
            events: layout.session_events_path(task_id),
            content,
        }
    }

    /// Append one record of our own to the JSONL journal.
    pub(super) fn record(&self, kind: &str, fields: Vec<(&str, Value)>) {
        let at = now();
        let mut onlyne = json!({"kind": kind, "at": at});
        let map = onlyne.as_object_mut().expect("built above");
        for (key, value) in fields {
            map.insert(key.to_string(), value);
        }
        self.event(json!({"onlyne": onlyne}), &at);
    }

    /// Append one raw agent update without decorating the journal object.
    pub(super) fn raw(&self, record: Value) {
        let at = now();
        self.event(record, &at);
    }

    fn event(&self, record: Value, at: &str) {
        if let Err(error) = self.content.append(
            &self.workspace,
            &self.task_id,
            Some(&self.session_id),
            &self.events,
            &record,
            at,
        ) {
            tracing::warn!(
                error = %error,
                file = %self.events.display(),
                "acp: content journal/index write failed"
            );
        }
    }
}

/// Append rendered text to a journal file, warning on failure. A call is a
/// record boundary, so the line terminator belongs here rather than at callers.
pub(super) fn append(path: &Path, text: &str) {
    let body = text.trim_end_matches('\n');
    if body.is_empty() {
        return;
    }
    if let Err(error) = append_all(path, &format!("{body}\n")) {
        tracing::warn!(error = %error, file = %path.display(), "acp: journal write failed");
    }
}

fn append_all(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(text.as_bytes())?;
    file.flush()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
