//! The board's state: the reducer's [`View`] and the little the screen owns.
//!
//! [`State`] is `(View, UiState)` and nothing else. `View` is
//! `onlyne_proto::view`'s output — every fact about the cluster a page draws —
//! and it is never written here: [`crate::tui::update::update`] reaches it only
//! through `snapshot_to_view` and `view::update`, the reducer's own two
//! functions, which is what keeps the TUI and the web from growing a second
//! way to read the cluster. `UiState` is what only this screen knows: which
//! page, which row is selected, how far a pane is scrolled, and the form an
//! operator has open.
//!
//! ## The outbox
//!
//! `update` is `State -> State`, as Elm's is, so a key that asks for an op —
//! send a task, `focus`, `repair`, `report` — cannot itself open a socket. It
//! records the op in [`UiState::actions`] instead, and the driver that called
//! `update` takes them ([`State::take_actions`]) and hands them to the one IO
//! task. The reducer stays pure, the screen never touches the wire, and a test
//! can assert that a key press asks for exactly one op without a server
//! anywhere.

use onlyne_proto::Outcome;
use onlyne_proto::view::View;

/// One of the three pages the plan lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    /// Roles with their presence, session counts and queue depth, the selected
    /// role's board, and the event tail.
    #[default]
    Cluster,
    /// One task family's path across roles: every delivery, every receipt, and
    /// the session log's tail.
    Task,
    /// The open faults and the repair verb each one offers.
    Faults,
}

impl Page {
    /// Every page, in the order its digit names it.
    pub const ALL: [Page; 3] = [Page::Cluster, Page::Task, Page::Faults];

    /// The page's word, in the header and in the footer.
    pub fn title(self) -> &'static str {
        match self {
            Page::Cluster => "cluster",
            Page::Task => "task",
            Page::Faults => "faults",
        }
    }

    /// The digit that switches to this page.
    pub fn number(self) -> char {
        match self {
            Page::Cluster => '1',
            Page::Task => '2',
            Page::Faults => '3',
        }
    }

    /// The page a digit names, when it names one.
    pub fn from_number(digit: char) -> Option<Page> {
        Page::ALL.into_iter().find(|page| page.number() == digit)
    }

    /// The panes `Tab` walks, in the order it visits them.
    ///
    /// A pane is a list the arrows step or a pane the scroll keys move; it is
    /// named here so the footer can say where the keys are pointed.
    pub fn panes(self) -> &'static [&'static str] {
        match self {
            Page::Cluster => &["roles", "board", "events"],
            Page::Task => &["families", "deliveries", "log"],
            Page::Faults => &["faults", "detail"],
        }
    }

    /// The footer's key line for this page: the ops the plan gives the TUI,
    /// named where the operator reading the screen finds them.
    pub fn legend(self) -> &'static str {
        match self {
            Page::Cluster => "1/2/3 page · tab pane · ↑/↓ select · enter task · s send · f focus",
            Page::Task => "1/2/3 page · tab pane · ↑/↓ select · s send · f focus · r report",
            Page::Faults => "1/2/3 page · tab pane · ↑/↓ select · repair below · ^r refresh",
        }
    }
}

/// What the IO task last said about the admin connection.
///
/// This is the link's own fact and lives here rather than in the `View`: the
/// stream's *content* is the reducer's, and whether the socket is up is the
/// task's. [`View::stale`] is the third state — the link is up and events were
/// lost — and it stays the reducer's, so a page can say "catching up" from the
/// view alone, with no server and no link in the picture at all.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Link {
    /// The first read has not answered yet.
    #[default]
    Connecting,
    /// The snapshot is read and the subscription is carrying events.
    Live,
    /// Nothing is answering: the reason is the operator's next move.
    Offline(String),
}

impl Link {
    /// The word the header prints for this link.
    pub fn word(&self) -> &str {
        match self {
            Link::Connecting => "connecting",
            Link::Live => "live",
            Link::Offline(reason) => reason,
        }
    }
}

/// How loud a notice is: what the ops answered, and how badly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

/// The last thing an op answered, printed on the footer's second line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub level: Level,
    pub text: String,
}

impl Notice {
    /// An op that answered.
    pub fn ok(op: &str, detail: &str) -> Self {
        Notice {
            level: Level::Info,
            text: format!("{op}: {detail}"),
        }
    }

    /// An op that refused, or a form that cannot be sent as filled in.
    pub fn error(op: &str, detail: &str) -> Self {
        Notice {
            level: Level::Error,
            text: format!("{op}: {detail}"),
        }
    }
}

/// One repair verb, and what it needs to name.
///
/// The faults page offers exactly the verbs that apply to the selected fault:
/// `ack` closes the record, and the four task-addressed verbs exist because a
/// fault either names a task or cannot offer them at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repair {
    /// Close the fault record as handled. Every open fault offers this.
    Ack { fault_id: i64, reason: String },
    /// Re-queue the settled or faulted task once.
    Retry { task_id: String, reason: String },
    /// Close the session's resource and settle its task.
    Close { task_id: String, reason: String },
    /// Settle the task as failed.
    Fail { task_id: String, reason: String },
    /// Read the session's reducer state without changing it.
    Inspect { task_id: String },
}

impl Repair {
    /// The key that offers this verb on the faults page.
    pub fn key(&self) -> char {
        match self {
            Repair::Ack { .. } => 'a',
            Repair::Retry { .. } => 't',
            Repair::Close { .. } => 'c',
            Repair::Fail { .. } => 'F',
            Repair::Inspect { .. } => 'i',
        }
    }

    /// The verb's own word, as the wire op and the footer print it.
    pub fn word(&self) -> &'static str {
        match self {
            Repair::Ack { .. } => "repair_ack",
            Repair::Retry { .. } => "repair_retry",
            Repair::Close { .. } => "repair_close",
            Repair::Fail { .. } => "repair_fail",
            Repair::Inspect { .. } => "repair_inspect",
        }
    }

    /// One line saying what the verb does, for the repair pane.
    pub fn describe(&self) -> &'static str {
        match self {
            Repair::Ack { .. } => "close the record as handled",
            Repair::Retry { .. } => "re-queue the task once",
            Repair::Close { .. } => "close the session and settle its task",
            Repair::Fail { .. } => "settle the task failed",
            Repair::Inspect { .. } => "read the session's reducer state",
        }
    }

    /// True when the verb refuses an empty `reason`.
    pub fn needs_reason(&self) -> bool {
        matches!(self, Repair::Ack { .. } | Repair::Fail { .. })
    }

    /// Every verb one fault offers, in the order the page lists them.
    ///
    /// `ack` is always offered: a record with no task is exactly the fault a
    /// person has to close by hand. The four task-addressed verbs exist only
    /// for a fault that names a task, because a repair aimed at no task has
    /// nothing to move.
    pub fn offered(fault: &onlyne_proto::FaultEvent) -> Vec<Repair> {
        let mut verbs = vec![Repair::Ack {
            fault_id: fault.id,
            reason: String::new(),
        }];
        if let Some(task_id) = fault.task_id.clone() {
            verbs.push(Repair::Retry {
                task_id: task_id.clone(),
                reason: String::new(),
            });
            verbs.push(Repair::Close {
                task_id: task_id.clone(),
                reason: String::new(),
            });
            verbs.push(Repair::Fail {
                task_id: task_id.clone(),
                reason: String::new(),
            });
            verbs.push(Repair::Inspect { task_id });
        }
        verbs
    }
}

/// One op the operator asked for, carried from the screen to the IO task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Send a task envelope on `from`'s behalf.
    Send {
        from: String,
        to: String,
        body: String,
    },
    /// Bring the named task's live session to the front of `to`'s host.
    Focus {
        from: String,
        to: String,
        task_id: String,
    },
    /// One repair verb.
    Repair(Repair),
    /// File a task's terminal outcome on `from`'s behalf.
    Report {
        from: String,
        task_id: String,
        outcome: Outcome,
        head: String,
    },
    /// Re-read the snapshot without moving the stream's cursor.
    Refresh,
}

impl Action {
    /// The op's word, for the notice the answer raises.
    pub fn word(&self) -> &'static str {
        match self {
            Action::Send { .. } => "send",
            Action::Focus { .. } => "focus",
            Action::Repair(repair) => repair.word(),
            Action::Report { .. } => "report",
            Action::Refresh => "refresh",
        }
    }
}

/// A form the operator is filling in, and the op it will send.
///
/// One variant per operation the plan gives the TUI, each carrying its own
/// fields: the shape of the form is the shape of the op, so a field can never
/// be read for an op that has no place to put it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// Send a task: who it comes from, who it goes to, and what it says.
    Send {
        from: String,
        to: String,
        body: String,
        field: usize,
    },
    /// Focus: the sender, the target role the operator names explicitly, and
    /// the task whose session is to be brought forward.
    Focus {
        from: String,
        to: String,
        task_id: String,
        field: usize,
    },
    /// One repair verb, waiting on the reason a couple of them require.
    Repair { verb: Repair, reason: String },
    /// Report: who files it, which task, the verdict, and one display line.
    Report {
        from: String,
        task_id: String,
        outcome: String,
        head: String,
        field: usize,
    },
}

impl Prompt {
    /// The prompt for a fresh send.
    pub fn send(sender: &str, to: Option<&str>) -> Self {
        Prompt::Send {
            from: sender.to_string(),
            to: to.unwrap_or_default().to_string(),
            body: String::new(),
            field: 1,
        }
    }

    /// The prompt for a focus, with the target and the task already named.
    pub fn focus(sender: &str, to: Option<&str>, task_id: Option<&str>) -> Self {
        Prompt::Focus {
            from: sender.to_string(),
            to: to.unwrap_or_default().to_string(),
            task_id: task_id.unwrap_or_default().to_string(),
            // The caret opens on `to`: the plan makes the target the operator's
            // explicit answer rather than something read off the selection.
            field: 1,
        }
    }

    /// The prompt for a repair verb.
    pub fn repair(verb: Repair) -> Self {
        Prompt::Repair {
            verb,
            reason: String::new(),
        }
    }

    /// The prompt for a report on one task.
    pub fn report(sender: &str, task_id: Option<&str>) -> Self {
        Prompt::Report {
            from: sender.to_string(),
            task_id: task_id.unwrap_or_default().to_string(),
            outcome: Outcome::Done.as_str().to_string(),
            head: String::new(),
            field: if task_id.is_some() { 2 } else { 1 },
        }
    }

    /// The block's title.
    pub fn title(&self) -> &'static str {
        match self {
            Prompt::Send { .. } => "send a task",
            Prompt::Focus { .. } => "focus a session",
            Prompt::Repair { verb, .. } => verb.word(),
            Prompt::Report { .. } => "report a task",
        }
    }

    /// The line of help drawn under the fields.
    pub fn help(&self) -> &'static str {
        match self {
            Prompt::Send { .. } => "enter send · tab field · esc cancel",
            Prompt::Focus { .. } => "enter focus · tab field · esc cancel",
            Prompt::Repair { verb, .. } if verb.needs_reason() => {
                "enter run · the reason is required · esc cancel"
            }
            Prompt::Repair { .. } => "enter run · esc cancel",
            Prompt::Report { .. } => {
                "enter report · outcome is done, failed, cancelled or blocked · esc cancel"
            }
        }
    }

    /// The fields, in the order they are drawn and stepped.
    pub fn fields(&self) -> Vec<(&'static str, &str)> {
        match self {
            Prompt::Send { from, to, body, .. } => {
                vec![("from", from), ("to", to), ("body", body)]
            }
            Prompt::Focus {
                from, to, task_id, ..
            } => vec![("from", from), ("to", to), ("task", task_id)],
            Prompt::Repair { reason, .. } => vec![("reason", reason)],
            Prompt::Report {
                from,
                task_id,
                outcome,
                head,
                ..
            } => vec![
                ("from", from),
                ("task", task_id),
                ("outcome", outcome),
                ("head", head),
            ],
        }
    }

    /// The field the caret is in.
    pub fn caret(&self) -> usize {
        match self {
            Prompt::Send { field, .. }
            | Prompt::Focus { field, .. }
            | Prompt::Report { field, .. } => *field,
            Prompt::Repair { .. } => 0,
        }
    }

    /// The field the caret is in, as the footer names it.
    pub fn focus_label(&self) -> &'static str {
        let fields = self.fields();
        fields
            .get(self.caret())
            .map(|(label, _)| *label)
            .unwrap_or("")
    }

    /// The field the caret edits.
    fn field(&mut self) -> &mut String {
        match self {
            Prompt::Send {
                from,
                to,
                body,
                field,
                ..
            } => match field {
                0 => from,
                1 => to,
                _ => body,
            },
            Prompt::Focus {
                from,
                to,
                task_id,
                field,
            } => match field {
                0 => from,
                1 => to,
                _ => task_id,
            },
            Prompt::Repair { reason, .. } => reason,
            Prompt::Report {
                from,
                task_id,
                outcome,
                head,
                field,
            } => match field {
                0 => from,
                1 => task_id,
                2 => outcome,
                _ => head,
            },
        }
    }

    /// The fields a caret can stand in.
    fn field_count(&self) -> usize {
        match self {
            Prompt::Repair { .. } => 1,
            other => other.fields().len(),
        }
    }

    /// The caret's field index, kept inside the fields there are.
    fn set_focus(&mut self, focus: usize) {
        let count = self.field_count();
        let focus = focus.min(count.saturating_sub(1));
        match self {
            Prompt::Send { field, .. }
            | Prompt::Focus { field, .. }
            | Prompt::Report { field, .. } => *field = focus,
            Prompt::Repair { .. } => {}
        }
    }

    /// Type one character into the caret's field.
    pub fn insert(&mut self, character: char) {
        self.field().push(character);
    }

    /// Drop the caret field's last character.
    pub fn backspace(&mut self) {
        self.field().pop();
    }

    /// Move the caret to the next field, wrapping.
    pub fn next_field(&mut self, delta: isize) {
        let count = self.field_count() as isize;
        if count <= 1 {
            return;
        }
        let focus = (self.caret() as isize + delta).rem_euclid(count);
        self.set_focus(focus as usize);
    }

    /// The op this form asks for, or why it cannot be sent as filled in.
    ///
    /// `sender` is updated with the role the operator sent as, so the next form
    /// opens on it: the operator who speaks as `alice` once speaks as `alice`
    /// until they say otherwise.
    pub fn submit(&self, sender: &mut String) -> Result<Action, String> {
        let required = |label: &str, value: &str| -> Result<(), String> {
            if value.trim().is_empty() {
                Err(format!("{label} is required"))
            } else {
                Ok(())
            }
        };
        match self {
            Prompt::Send { from, to, body, .. } => {
                required("from", from)?;
                required("to", to)?;
                required("body", body)?;
                *sender = from.clone();
                Ok(Action::Send {
                    from: from.clone(),
                    to: to.clone(),
                    body: body.clone(),
                })
            }
            Prompt::Focus {
                from, to, task_id, ..
            } => {
                required("from", from)?;
                required("to", to)?;
                required("task", task_id)?;
                *sender = from.clone();
                Ok(Action::Focus {
                    from: from.clone(),
                    to: to.clone(),
                    task_id: task_id.clone(),
                })
            }
            Prompt::Repair { verb, reason } => {
                if verb.needs_reason() {
                    required("reason", reason)?;
                }
                Ok(Action::Repair(match verb {
                    Repair::Ack { fault_id, .. } => Repair::Ack {
                        fault_id: *fault_id,
                        reason: reason.clone(),
                    },
                    Repair::Retry { task_id, .. } => Repair::Retry {
                        task_id: task_id.clone(),
                        reason: reason.clone(),
                    },
                    Repair::Close { task_id, .. } => Repair::Close {
                        task_id: task_id.clone(),
                        reason: reason.clone(),
                    },
                    Repair::Fail { task_id, .. } => Repair::Fail {
                        task_id: task_id.clone(),
                        reason: reason.clone(),
                    },
                    Repair::Inspect { task_id } => Repair::Inspect {
                        task_id: task_id.clone(),
                    },
                }))
            }
            Prompt::Report {
                from,
                task_id,
                outcome,
                head,
                ..
            } => {
                required("from", from)?;
                required("task", task_id)?;
                *sender = from.clone();
                let outcome =
                    outcome_word(outcome).ok_or_else(|| format!("unknown outcome {outcome:?}"))?;
                Ok(Action::Report {
                    from: from.clone(),
                    task_id: task_id.clone(),
                    outcome,
                    head: head.clone(),
                })
            }
        }
    }
}

/// The four words a report's outcome may carry.
fn outcome_word(raw: &str) -> Option<Outcome> {
    [
        Outcome::Done,
        Outcome::Failed,
        Outcome::Cancelled,
        Outcome::Blocked,
    ]
    .into_iter()
    .find(|outcome| outcome.as_str() == raw.trim())
}

/// The board's state: the reducer's view, and what only this screen knows.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    /// The reducer's output. Written only by `update`, through the reducer's
    /// own two functions.
    pub view: View,
    /// The screen's own state: page, selection, scroll, form, link, outbox.
    pub ui: UiState,
}

impl State {
    /// A board that has read nothing yet.
    pub fn new(sender: impl Into<String>) -> Self {
        State {
            view: View::default(),
            ui: UiState::new(sender),
        }
    }

    /// Take the ops the reducer recorded, leaving the outbox empty.
    ///
    /// This is Elm's `Cmd`, flattened: `update` cannot perform an effect, so it
    /// writes one down and the driver carries it to the IO task.
    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.ui.actions)
    }
}

/// What only this screen knows.
#[derive(Debug, Clone, PartialEq)]
pub struct UiState {
    /// The page being drawn.
    pub page: Page,
    /// The pane `Tab` last moved to, indexed into [`Page::panes`].
    pub pane: usize,
    /// The selected role, by name.
    pub role: Option<String>,
    /// The selected delivery on the cluster page, by `msg_id`.
    pub card: Option<String>,
    /// The selected task family on the task page.
    pub family: Option<String>,
    /// The selected delivery on the task page, by `msg_id`.
    pub delivery: Option<String>,
    /// The selected fault, by id.
    pub fault: Option<i64>,
    /// How far the page's tail pane is scrolled back from the newest line.
    pub scroll: usize,
    /// The form the operator has open, if any.
    pub prompt: Option<Prompt>,
    /// What the last op answered.
    pub notice: Option<Notice>,
    /// What the IO task last said about the connection.
    pub link: Link,
    /// The role a form opens as the sender, remembered across forms.
    pub sender: String,
    /// The ops the reducer asked for, waiting for the driver to take them.
    pub actions: Vec<Action>,
    /// The operator asked to leave.
    pub quit: bool,
}

impl UiState {
    /// A board on its first page with nothing selected.
    pub fn new(sender: impl Into<String>) -> Self {
        UiState {
            page: Page::default(),
            pane: 0,
            role: None,
            card: None,
            family: None,
            delivery: None,
            fault: None,
            scroll: 0,
            prompt: None,
            notice: None,
            link: Link::default(),
            sender: sender.into(),
            actions: Vec::new(),
            quit: false,
        }
    }
}
