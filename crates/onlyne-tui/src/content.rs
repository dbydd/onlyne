//! Read an ACP session journal and turn it into the text an operator reads.
//!
//! `onlyne-client` appends one JSON object per line to
//! `<workspace>/.onlyne/logs/session-<task>.events.jsonl` while a turn runs: the
//! raw `session/update` notifications the agent sent, plus this project's own
//! `dispatch` and `turn` records, so a reader sees both halves of the
//! conversation instead of the agent's half alone. A line carries an `onlyne`
//! key when the client wrote it and a `sessionUpdate` key when the agent did; a
//! line with neither is skipped, and no other marker is needed.
//!
//! The writer appends while a turn runs, so a reader can catch the last line
//! half-written. That is normal, not a fault: [`parse_line`] answers
//! [`Record::Ignored`] for it and the next reload sees the finished line.
//!
//! [`ContentSource`] is the one seam between the journal and the renderer: the
//! renderer and every page that shows a session consume records, never files.

use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

/// Where a workspace keeps the journals the client writes.
pub const LOGS_RELATIVE: &str = ".onlyne/logs";

/// How much of each block a view shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContentMode {
    /// User text, assistant text, reasoning text, and one summary line per tool
    /// call carrying its title, kind, and final status.
    #[default]
    Full,
    /// User text, assistant text, and the tool name only.
    Compact,
}

impl ContentMode {
    pub fn label(self) -> &'static str {
        match self {
            ContentMode::Full => "full",
            ContentMode::Compact => "compact",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            ContentMode::Full => ContentMode::Compact,
            ContentMode::Compact => ContentMode::Full,
        }
    }
}

/// One journal line, classified by the key that identifies its author.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    /// The task text the client handed the agent.
    Dispatch {
        task_id: String,
        prose: String,
        at: Option<String>,
    },
    /// The turn's terminal stop reason and closing line.
    Turn {
        task_id: String,
        stop_reason: String,
        head: Option<String>,
        at: Option<String>,
    },
    /// An `session/update` notification from the agent.
    Update(Update),
    /// A line that is not ours to show: another author's record, a half-written
    /// tail, or JSON that will never parse.
    Ignored,
}

/// The parts of a `session/update` the content surface reads.
///
/// `available_commands_update` is a multi-kilobyte dump of the agent's
/// slash-command list, so an update the renderer does not show keeps only its
/// name: the payload never reaches the page. The same holds for the fields a
/// tool entry does not need — `locations[].path` (the title already names the
/// file) and `tool_call_update` output blobs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    Message {
        text: String,
    },
    Thought {
        text: String,
    },
    ToolCall {
        id: String,
        title: String,
        kind: String,
        status: Option<String>,
    },
    ToolUpdate {
        id: String,
        status: Option<String>,
    },
    Other {
        kind: String,
    },
}

impl Record {
    fn dispatch(ours: &Value) -> Record {
        Record::Dispatch {
            task_id: string_field(ours, "task_id"),
            prose: string_field(ours, "prose"),
            at: opt_string(ours, "at"),
        }
    }

    fn turn(ours: &Value) -> Record {
        Record::Turn {
            task_id: string_field(ours, "task_id"),
            stop_reason: string_field(ours, "stop_reason"),
            head: opt_string(ours, "head"),
            at: opt_string(ours, "at"),
        }
    }
}

/// Classify one journal line. Never fails: a line the reader cannot use is
/// [`Record::Ignored`], which is what keeps a half-written tail harmless.
pub fn parse_line(line: &str) -> Record {
    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
        return Record::Ignored;
    };
    if let Some(ours) = value.get("onlyne") {
        return match ours.get("kind").and_then(Value::as_str) {
            Some("dispatch") => Record::dispatch(ours),
            Some("turn") => Record::turn(ours),
            _ => Record::Ignored,
        };
    }
    match value.get("sessionUpdate").and_then(Value::as_str) {
        Some(kind) => Record::Update(parse_update(kind, &value)),
        None => Record::Ignored,
    }
}

fn parse_update(kind: &str, value: &Value) -> Update {
    match kind {
        "agent_message_chunk" => Update::Message {
            text: text_of(value.get("content")),
        },
        "agent_thought_chunk" => Update::Thought {
            text: text_of(value.get("content")),
        },
        "tool_call" => Update::ToolCall {
            id: string_field(value, "toolCallId"),
            title: string_field(value, "title"),
            kind: string_field(value, "kind"),
            status: opt_string(value, "status"),
        },
        "tool_call_update" => Update::ToolUpdate {
            id: string_field(value, "toolCallId"),
            status: opt_string(value, "status"),
        },
        other => Update::Other {
            kind: other.to_string(),
        },
    }
}

/// `content.text`, whether the agent sent one block or an array of them.
fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::Object(map)) => map
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Some(Value::Array(rows)) => rows.iter().map(|row| text_of(Some(row))).collect(),
        _ => String::new(),
    }
}

fn string_field(value: &Value, key: &str) -> String {
    opt_string(value, key).unwrap_or_default()
}

fn opt_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|text| text.to_string())
}

/// Where a session's records come from.
///
/// One seam, so a later slice can hand the page a live client-socket feed in
/// place of the journal file without touching the renderer or the page.
pub trait ContentSource {
    /// The journal path for `task_id`, for the footer and the missing-journal
    /// line. Answers even when nothing is there: the path is what an operator
    /// checks.
    fn journal_path(&self, task_id: &str) -> PathBuf;

    /// Every record for `task_id`, oldest first. An unreadable journal yields no
    /// records rather than an error, so a page can say the file is missing
    /// instead of failing.
    ///
    /// A live feed behind this seam has to hand each record over exactly once.
    /// Grouping accumulates chunk text (see [`load`]), so a record that arrives
    /// twice does not print twice — it reads as one doubled sentence. That is
    /// the binding constraint at a file-to-socket handoff: the client's
    /// head-inclusive default duplicates the newest journalled line, the journal
    /// carries no sequence number to filter it with, and `Value` equality is
    /// key-order safe, so the duplicate is dropped by comparing the replayed
    /// record with the last line read, before it reaches the grouping.
    fn records(&self, task_id: &str) -> Box<dyn Iterator<Item = Record> + '_>;

    /// Whether the journal is there to be read at all.
    fn journal_exists(&self, task_id: &str) -> bool {
        self.journal_path(task_id).is_file()
    }
}

/// The journal-file implementation of [`ContentSource`]: read
/// `<workspace>/.onlyne/logs/session-<task>.events.jsonl` line by line.
pub struct JournalSource {
    pub workspace: PathBuf,
}

impl JournalSource {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
        }
    }

    /// `<workspace>/.onlyne/logs`, where the client journals every session.
    pub fn logs_dir(&self) -> PathBuf {
        self.workspace.join(LOGS_RELATIVE)
    }
}

impl ContentSource for JournalSource {
    fn journal_path(&self, task_id: &str) -> PathBuf {
        self.logs_dir()
            .join(format!("session-{task_id}.events.jsonl"))
    }

    fn records(&self, task_id: &str) -> Box<dyn Iterator<Item = Record> + '_> {
        let Ok(file) = std::fs::File::open(self.journal_path(task_id)) else {
            return Box::new(std::iter::empty());
        };
        // `lines` yields a trailing partial line as the last item, so a
        // mid-write tail parses to nothing and is skipped for this reload.
        // An I/O error (a journal replaced under us) ends the read where it is.
        Box::new(
            BufReader::new(file)
                .lines()
                .map_while(Result::ok)
                .map(|line| parse_line(&line)),
        )
    }
}

/// One thing the operator reads: what was asked, what the agent thought, what it
/// said, or what it ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Tool {
        id: String,
        title: String,
        kind: String,
        status: Option<String>,
    },
}

impl Block {
    fn kind(&self) -> LineKind {
        match self {
            Block::User { .. } => LineKind::User,
            Block::Assistant { .. } => LineKind::Assistant,
            Block::Reasoning { .. } => LineKind::Reasoning,
            Block::Tool { .. } => LineKind::Tool,
        }
    }

    fn text(&self, mode: ContentMode) -> String {
        match self {
            Block::User { text } | Block::Assistant { text } | Block::Reasoning { text } => {
                text.clone()
            }
            Block::Tool {
                title,
                kind,
                status,
                ..
            } => match mode {
                // The summary line: what ran, of what sort, and how it ended.
                ContentMode::Full => {
                    let mut line = String::new();
                    if !kind.is_empty() {
                        line.push('[');
                        line.push_str(kind);
                        line.push(']');
                    }
                    if !title.is_empty() {
                        if !line.is_empty() {
                            line.push(' ');
                        }
                        line.push_str(title);
                    }
                    if let Some(status) = status {
                        line.push_str(" (");
                        line.push_str(status);
                        line.push(')');
                    }
                    line
                }
                // The name, nothing else.
                ContentMode::Compact => title.clone(),
            },
        }
    }

    /// The marker that opens a block's first line. Reasoning carries it so an
    /// operator can tell thinking from the answer at a glance, and a tool line
    /// is indented to read as part of the answer it belongs to.
    fn marker(&self) -> &'static str {
        match self.kind() {
            LineKind::User => "> ",
            LineKind::Assistant => "",
            LineKind::Reasoning => "~ ",
            LineKind::Tool => "  ",
            LineKind::Blank | LineKind::Notice => "",
        }
    }
}

/// What a display line is, so a page can style it and a test can name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    User,
    Assistant,
    Reasoning,
    Tool,
    Blank,
    /// Not a block at all: what a page says when the journal has nothing to show.
    Notice,
}

/// One rendered line, marker included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentLine {
    pub kind: LineKind,
    pub text: String,
}

/// The last turn's terminal record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub stop_reason: String,
    pub head: Option<String>,
}

/// Everything one task's journal says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Doc {
    pub task_id: String,
    pub path: PathBuf,
    /// `false` when the journal is not there, which the page has to say rather
    /// than leave as a blank board.
    pub journal_exists: bool,
    pub blocks: Vec<Block>,
    pub turn: Option<Turn>,
}

impl Doc {
    /// The blocks as display lines for `mode`, one blank line apart.
    pub fn lines(&self, mode: ContentMode) -> Vec<ContentLine> {
        let mut out: Vec<ContentLine> = Vec::new();
        for block in &self.blocks {
            if block.kind() == LineKind::Reasoning && mode == ContentMode::Compact {
                continue;
            }
            if !out.is_empty() {
                out.push(ContentLine {
                    kind: LineKind::Blank,
                    text: String::new(),
                });
            }
            push_marked(&mut out, block, mode);
        }
        out
    }

    /// How the turn stands: a stop reason once the client has recorded one, and
    /// `running` while the journal is still growing.
    pub fn turn_note(&self) -> String {
        match &self.turn {
            Some(turn) if turn.stop_reason.is_empty() => "turn closed".to_string(),
            Some(turn) => format!("turn {}", turn.stop_reason),
            None => "turn running".to_string(),
        }
    }
}

/// One block's lines: the marker opens it, continuations indent to match. Line
/// breaks already in the agent's text are kept; column wrapping is the pane's
/// job.
fn push_marked(out: &mut Vec<ContentLine>, block: &Block, mode: ContentMode) {
    let marker = block.marker();
    let pad = " ".repeat(marker.len());
    let text = block.text(mode);
    for (index, line) in text.split('\n').enumerate() {
        out.push(ContentLine {
            kind: block.kind(),
            text: format!("{}{}", if index == 0 { marker } else { &pad }, line),
        });
    }
}

/// The whole document as plain text, the shape `--once` prints and tests assert.
pub fn plain_text(lines: &[ContentLine]) -> String {
    lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The lines a growing journal added since `shown`.
///
/// A journal that lost text it already showed is not a journal that grew, so the
/// whole thing comes back and the caller re-prints it.
pub fn appended<'a>(shown: &[ContentLine], current: &'a [ContentLine]) -> &'a [ContentLine] {
    let shared = shown
        .iter()
        .zip(current.iter())
        .take_while(|(was, now)| was == now)
        .count();
    if shared < shown.len() {
        current
    } else {
        &current[shared..]
    }
}

/// Read a task's journal through the seam and group it into blocks.
pub fn load(source: &dyn ContentSource, task_id: &str) -> Doc {
    let mut grouper = Grouper::default();
    for record in source.records(task_id) {
        grouper.push(record);
    }
    let (blocks, turn) = grouper.finish();
    Doc {
        task_id: task_id.to_string(),
        path: source.journal_path(task_id),
        journal_exists: source.journal_exists(task_id),
        blocks,
        turn,
    }
}

/// Turns the record sequence into blocks in arrival order.
///
/// Chunk text accumulates: consecutive `agent_message_chunk` lines are one
/// assistant block and consecutive `agent_thought_chunk` lines one reasoning
/// block, and the block that is not arriving gets flushed the moment the other
/// kind starts, so the reading order never lies about what came first. Tool
/// calls correlate by `toolCallId`: the entry a `tool_call` opened is updated in
/// place when its `tool_call_update` lands, and an update naming a call nobody
/// announced still gets a line rather than being dropped.
///
/// Chunk text accumulates rather than repeats, which makes the record stream a
/// once-each stream: a chunk handed over twice doubles a sentence instead of
/// duplicating a row, and nothing downstream can tell. Delivering each record
/// once is therefore the source's job — see [`ContentSource::records`].
#[derive(Default)]
struct Grouper {
    blocks: Vec<Block>,
    turn: Option<Turn>,
    message: String,
    thought: String,
}

impl Grouper {
    fn push(&mut self, record: Record) {
        match record {
            Record::Dispatch { prose, .. } => {
                self.flush();
                self.push_block(Block::User {
                    text: prose.trim_end().to_string(),
                });
            }
            Record::Update(Update::Message { text }) => {
                if !self.thought.is_empty() {
                    self.flush_thought();
                }
                self.message.push_str(&text);
            }
            Record::Update(Update::Thought { text }) => {
                if !self.message.is_empty() {
                    self.flush_message();
                }
                self.thought.push_str(&text);
            }
            Record::Update(Update::ToolCall {
                id,
                title,
                kind,
                status,
            }) => {
                self.flush();
                match self.tool_mut(&id) {
                    Some(Block::Tool {
                        title: known,
                        kind: known_kind,
                        status: known_status,
                        ..
                    }) => {
                        if !title.is_empty() {
                            *known = title;
                        }
                        if !kind.is_empty() {
                            *known_kind = kind;
                        }
                        *known_status = status.or(known_status.clone());
                    }
                    _ => self.push_block(Block::Tool {
                        id,
                        title,
                        kind,
                        status,
                    }),
                }
            }
            Record::Update(Update::ToolUpdate { id, status }) => {
                self.flush();
                match self.tool_mut(&id) {
                    Some(Block::Tool { status: known, .. }) => {
                        *known = status.or_else(|| known.clone())
                    }
                    // A completion with no announcement: the operator still
                    // learns a call by that id finished, so it gets its own line.
                    _ => self.push_block(Block::Tool {
                        title: id.clone(),
                        id,
                        kind: String::new(),
                        status,
                    }),
                }
            }
            Record::Update(Update::Other { .. }) => {}
            Record::Turn {
                stop_reason, head, ..
            } => {
                self.flush();
                self.turn = Some(Turn { stop_reason, head });
            }
            Record::Ignored => {}
        }
    }

    /// The tool entry opened by this id, if one is open.
    fn tool_mut(&mut self, id: &str) -> Option<&mut Block> {
        self.blocks
            .iter_mut()
            .rev()
            .find(|block| matches!(block, Block::Tool { id: known, .. } if *known == id))
    }

    fn push_block(&mut self, block: Block) {
        let empty = match &block {
            Block::User { text } | Block::Assistant { text } | Block::Reasoning { text } => {
                text.trim().is_empty()
            }
            Block::Tool { .. } => false,
        };
        if !empty {
            self.blocks.push(block);
        }
    }

    fn flush_message(&mut self) {
        let text = strip_status(&self.message);
        self.message.clear();
        let text = text.trim();
        if !text.is_empty() {
            self.push_block(Block::Assistant {
                text: text.to_string(),
            });
        }
    }

    fn flush_thought(&mut self) {
        let raw = std::mem::take(&mut self.thought);
        let text = raw.trim();
        if !text.is_empty() {
            self.push_block(Block::Reasoning {
                text: text.to_string(),
            });
        }
    }

    fn flush(&mut self) {
        if !self.message.is_empty() {
            self.flush_message();
        }
        if !self.thought.is_empty() {
            self.flush_thought();
        }
    }

    fn finish(mut self) -> (Vec<Block>, Option<Turn>) {
        self.flush();
        (self.blocks, self.turn)
    }
}

/// Drop the `<status>…</status>` markers an agent stamps into its own answer.
///
/// Qoder closes the marker with a matching tag and, in a stream cut short,
/// leaves it open; an unclosed marker runs to the end of the block, which is
/// where the agent puts it.
pub fn strip_status(text: &str) -> String {
    const OPEN: &str = "<status>";
    const CLOSE: &str = "</status>";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(OPEN) {
        out.push_str(&rest[..open]);
        let tail = &rest[open + OPEN.len()..];
        match tail.find(CLOSE) {
            Some(close) => rest = &tail[close + CLOSE.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A journal in the shape the client writes it: one dispatch, a thought, an
    /// answer split around two tool calls, the agent's own `<status>` stamp, and
    /// the closing turn record.
    fn fixture_lines() -> Vec<String> {
        [
            r#"{"onlyne":{"kind":"dispatch","task_id":"t-1","prose":"Add a docstring to greet.","at":"2026-09-18T10:00:00Z"}}"#,
            r#"{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"The file has one function."}}"#,
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Sure. "}}"#,
            r#"{"sessionUpdate":"tool_call","toolCallId":"call_1","status":"pending","title":"Read hello.py","kind":"read","locations":[{"path":"hello.py"}]}"#,
            r#"{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"/init","description":"a very long description that goes on"}]}"#,
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"call_1","status":"completed"}"#,
            r#"{"sessionUpdate":"tool_call","toolCallId":"call_2","status":"in_progress","title":"Edit hello.py","kind":"edit"}"#,
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"call_2","status":"completed"}"#,
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Added the docstring.<status>done"}}"#,
            r#"{"onlyne":{"kind":"turn","task_id":"t-1","stop_reason":"end_turn","head":"Added the docstring.","at":"2026-09-18T10:01:00Z"}}"#,
        ]
        .iter()
        .map(|line| (*line).to_string())
        .collect()
    }

    /// Lay that journal in `dir` as the task's own file.
    fn fixture_journal(dir: &Path, task_id: &str, lines: &[String]) {
        let logs = dir.join(LOGS_RELATIVE);
        std::fs::create_dir_all(&logs).expect("logs dir");
        let body = lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        std::fs::write(logs.join(format!("session-{task_id}.events.jsonl")), body)
            .expect("journal");
    }

    fn fixture_doc(dir: &Path) -> Doc {
        fixture_journal(dir, "t-1", &fixture_lines());
        load(&JournalSource::new(dir), "t-1")
    }

    #[test]
    fn a_full_turn_groups_into_blocks_in_arrival_order() {
        let dir = tempfile::tempdir().expect("temp dir");
        let doc = fixture_doc(dir.path());
        assert!(doc.journal_exists, "the fixture journal is the subject");
        assert_eq!(
            doc.blocks
                .iter()
                .map(|block| block.kind())
                .collect::<Vec<_>>(),
            vec![
                LineKind::User,
                LineKind::Reasoning,
                LineKind::Assistant,
                LineKind::Tool,
                LineKind::Tool,
                LineKind::Assistant,
            ],
            "the two chunks of one answer stay one block, and the tool calls sit where they happened"
        );
        let Block::Assistant { text } = &doc.blocks[2] else {
            panic!("the third block is the opening answer");
        };
        assert_eq!(text, "Sure.");
        let tools: Vec<&Block> = doc
            .blocks
            .iter()
            .filter(|block| block.kind() == LineKind::Tool)
            .collect();
        assert_eq!(tools.len(), 2, "one entry per toolCallId");
        let Block::Tool {
            title,
            kind,
            status,
            ..
        } = tools[1]
        else {
            panic!("filtered for tool entries");
        };
        assert_eq!((title.as_str(), kind.as_str()), ("Edit hello.py", "edit"));
        assert_eq!(status.as_deref(), Some("completed"));
        assert_eq!(doc.turn_note(), "turn end_turn");
    }

    #[test]
    fn full_shows_reasoning_and_tool_detail_compact_shows_the_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let doc = fixture_doc(dir.path());
        let lines = |mode: ContentMode| -> Vec<String> {
            plain_text(&doc.lines(mode))
                .lines()
                .map(|line| line.to_string())
                .collect()
        };

        assert_eq!(
            lines(ContentMode::Full),
            [
                "> Add a docstring to greet.",
                "",
                "~ The file has one function.",
                "",
                "Sure.",
                "",
                "  [read] Read hello.py (completed)",
                "",
                "  [edit] Edit hello.py (completed)",
                "",
                "Added the docstring.",
            ]
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>(),
            "full reads user, reasoning, answer, and one summary line per call"
        );
        assert_eq!(
            lines(ContentMode::Compact),
            [
                "> Add a docstring to greet.",
                "",
                "Sure.",
                "",
                "  Read hello.py",
                "",
                "  Edit hello.py",
                "",
                "Added the docstring.",
            ]
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>(),
            "compact is the same reading minus the reasoning and each call's kind and status"
        );
        assert!(
            !lines(ContentMode::Full).join("\n").contains("/init"),
            "the slash-command dump is recorded and dropped"
        );
    }

    #[test]
    fn a_half_written_tail_line_waits_for_the_writer() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = JournalSource::new(dir.path());
        fixture_journal(dir.path(), "t-1", &fixture_lines());
        let before = plain_text(&load(&source, "t-1").lines(ContentMode::Full));

        // The client is mid-write: the bytes are there, the newline is not.
        append_raw(
            dir.path(),
            "t-1",
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"One more"#,
        );
        let cut = load(&source, "t-1");
        assert_eq!(
            plain_text(&cut.lines(ContentMode::Full)),
            before,
            "a tail the writer has not finished changes nothing already read"
        );
        assert_eq!(cut.turn_note(), "turn end_turn");

        // Finishing that one line is all it takes; nothing restarts.
        append_raw(dir.path(), "t-1", r#" thing."}}"#);
        let grown = load(&source, "t-1");
        let text = plain_text(&grown.lines(ContentMode::Full));
        assert!(
            text.starts_with(&before) && text.ends_with("One more thing."),
            "the finished line lands as its own block\n{text}"
        );
        assert_eq!(grown.blocks.len(), cut.blocks.len() + 1);
    }

    /// Append raw bytes to the fixture journal, the way a live writer does.
    fn append_raw(dir: &Path, task_id: &str, bytes: &str) {
        use std::io::Write;
        let path = JournalSource::new(dir).journal_path(task_id);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("open the journal");
        file.write_all(bytes.as_bytes()).expect("append");
    }

    #[test]
    fn status_markers_never_reach_the_operator() {
        assert_eq!(strip_status("Added it.<status>done"), "Added it.");
        assert_eq!(
            strip_status("Added it.<status>in_progress</status>keep going"),
            "Added it.keep going"
        );
        assert_eq!(strip_status("<status>done"), "");
        assert_eq!(strip_status("no markers"), "no markers");
    }

    #[test]
    fn a_tool_update_for_an_unannounced_call_gets_its_own_entry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let lines = vec![
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"call_orphan","status":"failed","content":[{"content":{"type":"text","text":"no such file"}}]}"#.to_string(),
        ];
        fixture_journal(dir.path(), "t-2", &lines);
        let doc = load(&JournalSource::new(dir.path()), "t-2");
        let full = plain_text(&doc.lines(ContentMode::Full));
        assert!(full.contains("call_orphan"), "{full}");
        assert!(full.contains("(failed)"), "{full}");
        assert!(
            !full.contains("no such file"),
            "an update's output blob is not the summary line\n{full}"
        );
        assert_eq!(
            plain_text(&doc.lines(ContentMode::Compact)),
            "  call_orphan",
            "compact still names the call it knows about"
        );
    }

    #[test]
    fn the_onlyne_key_marks_our_lines_and_session_update_marks_the_agents() {
        assert_eq!(
            parse_line(r#"{"onlyne":{"kind":"dispatch","task_id":"t-1","prose":"hi","at":"x"}}"#),
            Record::Dispatch {
                task_id: "t-1".to_string(),
                prose: "hi".to_string(),
                at: Some("x".to_string())
            }
        );
        assert_eq!(
            parse_line(
                r#"{"onlyne":{"kind":"turn","task_id":"t-1","stop_reason":"refusal","at":"x"}}"#
            ),
            Record::Turn {
                task_id: "t-1".to_string(),
                stop_reason: "refusal".to_string(),
                head: None,
                at: Some("x".to_string())
            }
        );
        assert_eq!(
            parse_line(
                r#"{"sessionUpdate":"agent_thought_chunk","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}"#
            ),
            Record::Update(Update::Thought {
                text: "ab".to_string()
            })
        );
        for ignored in [
            r#"{"jsonrpc":"2.0","method":"session/update"}"#,
            r#"{"onlyne":{"kind":"future"}}"#,
            r#"{"onlyne":{"task_id":"t-1"}}"#,
            r#"{"nope":tru"#,
        ] {
            assert_eq!(parse_line(ignored), Record::Ignored, "{ignored}");
        }
        for dropped in [
            r#"{"sessionUpdate":"plan","entries":[{"priority":"high","content":"x"}]}"#,
            r#"{"sessionUpdate":"usage_update","used":10,"size":200}"#,
            r#"{"sessionUpdate":"current_mode_update","modeId":"auto"}"#,
            r#"{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"/init"}]}"#,
        ] {
            assert!(
                matches!(parse_line(dropped), Record::Update(Update::Other { .. })),
                "an update the page does not show keeps its name and drops its \
                 payload: {dropped}"
            );
        }
    }

    #[test]
    fn a_missing_journal_says_so_with_the_path_it_looked_for() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = JournalSource::new(dir.path());
        let doc = load(&source, "t-9");
        assert!(!doc.journal_exists);
        assert!(doc.blocks.is_empty());
        assert_eq!(
            doc.path,
            dir.path().join(".onlyne/logs/session-t-9.events.jsonl")
        );
    }

    #[test]
    fn appended_reports_only_the_lines_the_journal_grew_by() {
        let lines = |letters: &[&str]| -> Vec<ContentLine> {
            letters
                .iter()
                .map(|letter| ContentLine {
                    kind: LineKind::Assistant,
                    text: letter.to_string(),
                })
                .collect()
        };
        let first = lines(&["a", "b"]);
        assert!(appended(&first, &first).is_empty());
        assert_eq!(
            appended(&first, &lines(&["a", "b", "c"]))
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["c"]
        );
        assert_eq!(
            appended(&first, &lines(&["a"])).len(),
            1,
            "a journal that lost a line is re-rendered whole"
        );
    }
}
