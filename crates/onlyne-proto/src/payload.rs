//! The closing-report grammar a session's own file speaks: payload-v2.
//!
//! One report is one verdict line naming how the turn ended, followed by zero or
//! more handoff lines naming work to pass on. The grammar is deliberately
//! line-oriented and deliberately unforgiving: a file that is not a report in
//! this shape yields no verdict and no handoff, so a half-written or mistyped
//! report cannot route anything anywhere.
//!
//! This is the only parser of the format. The session backend reads a report
//! through it, the client routes what it returns, and the CLI's `report` verbs
//! print what it rejects. [`GRAMMAR_V2`] is the matching description, shown to
//! the agent that writes the file and to the operator who checks it.

use serde::{Deserialize, Serialize};

/// One `handoff:` line: who takes the work next, and what they are told.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    /// The recipient role, exactly as the report named it.
    pub to_role: String,
    /// The line's own text after `|`. `None` means the report left it out, and
    /// the verdict line's text is what the recipient receives.
    pub text: Option<String>,
}

impl Handoff {
    /// The body to deliver for this handoff: its own text, or `head` when the
    /// line carried none.
    pub fn text_or<'a>(&'a self, head: &'a str) -> &'a str {
        self.text.as_deref().unwrap_or(head)
    }
}

/// What one report file said, or the reason it said nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadV2 {
    /// `hop-done:` — the work ended as the agent says it did.
    Done {
        head: String,
        handoffs: Vec<Handoff>,
    },
    /// `hop-failed:` — the agent's own account of a failure.
    Failed {
        reason: String,
        handoffs: Vec<Handoff>,
    },
    /// `hop-blocked:` — the task could not go on, and the reason names what it
    /// waits on. A blocked task hands nothing on: the work is not finished, so
    /// there is nothing to relay.
    Blocked {
        reason: String,
        handoffs: Vec<Handoff>,
    },
    /// The file is not a report. The error names the category, and the line
    /// where this client stopped reading, so the author can fix it blind.
    Invalid { error: String },
}

impl PayloadV2 {
    /// The word a settled task records as its `head_kind`: which verdict line
    /// the head came from. `None` for an invalid report, which has no head.
    pub fn head_kind(&self) -> Option<&'static str> {
        match self {
            PayloadV2::Done { .. } => Some("done"),
            PayloadV2::Failed { .. } => Some("failed"),
            PayloadV2::Blocked { .. } => Some("blocked"),
            PayloadV2::Invalid { .. } => None,
        }
    }

    /// The verdict line's own text: the head of a done task, the reason of a
    /// failed or blocked one. `None` for an invalid report.
    pub fn verdict(&self) -> Option<&str> {
        match self {
            PayloadV2::Done { head, .. } => Some(head),
            PayloadV2::Failed { reason, .. } | PayloadV2::Blocked { reason, .. } => {
                Some(reason.as_str())
            }
            PayloadV2::Invalid { .. } => None,
        }
    }

    /// The handoffs this report asks for. Empty for an invalid one, which asks
    /// for nothing.
    pub fn handoffs(&self) -> &[Handoff] {
        match self {
            PayloadV2::Done { handoffs, .. }
            | PayloadV2::Failed { handoffs, .. }
            | PayloadV2::Blocked { handoffs, .. } => handoffs,
            PayloadV2::Invalid { .. } => &[],
        }
    }
}

/// Every line of a report past this count is a file this client will not read a
/// verdict from.
pub const MAX_REPORT_LINES: usize = 16;
/// Handoff lines one report may name.
pub const MAX_REPORT_HANDOFFS: usize = 8;

/// The verdict prefixes, longest first so a shorter one never shadows it.
const VERDICTS: &[(&str, &str)] = &[
    ("hop-done:", "done"),
    ("hop-failed:", "failed"),
    ("hop-blocked:", "blocked"),
];
/// The handoff prefix.
const HANDOFF_PREFIX: &str = "handoff:";

/// The grammar as prose, for the agent's prompt and the CLI's help.
pub const GRAMMAR_V2: &str = "\
Result report grammar (payload-v2): one verdict line first, then zero or more \
handoff lines. A line starting with `#` is a comment and blank lines are ignored; \
every other line makes the whole report invalid, and an invalid report hands \
nothing on. At most 16 lines and at most 8 handoff lines per file. A file with a \
single verdict line is a valid report.
  hop-done: <the result in one line>
  hop-failed: <why the task failed, one sentence>
  hop-blocked: <what the task is waiting on, one sentence>
  handoff: <target role> | <one line for that role>
The `|` part of a handoff line is optional: without it the recipient gets the \
verdict line's own text. A role is one word of letters, digits, `.`, `_`, or `-`. \
A `hop-blocked:` report never hands work on, because the task is not finished.";

/// Read one report file.
///
/// Both CRLF and a lone CR normalize to LF before the first line is classified,
/// so an agent that wrote its report from a CRLF tool still speaks this grammar.
/// Anything the grammar cannot read returns [`PayloadV2::Invalid`] with no
/// handoffs at all: a report this client cannot trust routes nothing.
pub fn parse(text: &str) -> PayloadV2 {
    if text.trim().is_empty() {
        return invalid("payload is empty");
    }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.lines().collect();
    if lines.len() > MAX_REPORT_LINES {
        return invalid(format!(
            "payload exceeds the {MAX_REPORT_LINES}-line limit ({} lines)",
            lines.len()
        ));
    }
    let mut verdict: Option<(&'static str, String)> = None;
    let mut handoffs: Vec<Handoff> = Vec::new();
    for (index, raw) in lines.iter().enumerate() {
        // Line numbers count from one, on the file the agent wrote, because that
        // is the file it will re-open to fix.
        let number = index + 1;
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // One pass over the verdict prefixes, and the text after the colon
        // comes back with the match, so no line is sliced twice.
        let matched = VERDICTS
            .iter()
            .find_map(|&(prefix, kind)| Some((prefix, kind, line.strip_prefix(prefix)?)));
        if let Some((prefix, kind, after)) = matched {
            if verdict.is_some() {
                return invalid(format!(
                    "line {number}: payload carries more than one verdict line"
                ));
            }
            if !handoffs.is_empty() {
                return invalid(format!(
                    "line {number}: a verdict line must come before any handoff line"
                ));
            }
            let body = after.trim();
            if body.is_empty() {
                return invalid(format!(
                    "line {number}: {} carries no {}",
                    prefix.trim_end_matches(':'),
                    if kind == "done" { "result" } else { "reason" }
                ));
            }
            verdict = Some((kind, body.to_string()));
            continue;
        }
        let Some(after) = line.strip_prefix(HANDOFF_PREFIX) else {
            return invalid(match line.find(':') {
                Some(at) => format!("line {number}: unknown report prefix {:?}", &line[..at]),
                None => format!("line {number}: report line carries no prefix"),
            });
        };
        if verdict.is_none() {
            return invalid(format!(
                "line {number}: handoff line has no verdict line before it"
            ));
        }
        if handoffs.len() == MAX_REPORT_HANDOFFS {
            return invalid(format!(
                "line {number}: payload carries more than {MAX_REPORT_HANDOFFS} handoff lines"
            ));
        }
        match handoff_line(after) {
            Ok(handoff) => handoffs.push(handoff),
            Err(error) => return invalid(format!("line {number}: {error}")),
        }
    }
    let Some((kind, body)) = verdict else {
        return invalid("payload carries no verdict line");
    };
    match kind {
        "done" => PayloadV2::Done {
            head: body,
            handoffs,
        },
        "failed" => PayloadV2::Failed {
            reason: body,
            handoffs,
        },
        _ => PayloadV2::Blocked {
            reason: body,
            handoffs,
        },
    }
}

/// The target and text of one `handoff:` line, given the text after its prefix.
/// A line that names no text of its own keeps `text: None`, and the reader
/// delivers the verdict line's text in its place.
fn handoff_line(after: &str) -> Result<Handoff, String> {
    let trimmed = after.trim_start();
    let cut = trimmed
        .find(|c: char| c == '|' || c.is_whitespace())
        .unwrap_or(trimmed.len());
    let (role, rest) = trimmed.split_at(cut);
    if role.is_empty() {
        return Err("handoff line carries no target role".to_string());
    }
    if !role
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!("handoff target role {role:?} is not a role name"));
    }
    let tail = rest.trim_start();
    let text = match tail.strip_prefix('|') {
        Some(body) => {
            let body = body.trim();
            (!body.is_empty()).then(|| body.to_string())
        }
        // Anything after the role that is not a `|` separator is a line this
        // client cannot read a recipient and a brief apart from.
        None if !tail.is_empty() => {
            return Err(
                "handoff line has text after the role without a \"|\" separator".to_string(),
            );
        }
        None => None,
    };
    Ok(Handoff {
        to_role: role.to_string(),
        text,
    })
}

/// The one shape every rejection takes.
fn invalid(error: impl Into<String>) -> PayloadV2 {
    PayloadV2::Invalid {
        error: error.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invalid_error(text: &str) -> String {
        match parse(text) {
            PayloadV2::Invalid { error } => error,
            other => panic!("expected an invalid payload, got {other:?}"),
        }
    }

    #[test]
    fn single_done_line_is_a_complete_report() {
        let payload = parse("hop-done: the payload grammar landed\n");

        let PayloadV2::Done { head, handoffs } = payload else {
            panic!("expected a valid done report");
        };
        assert_eq!(head, "the payload grammar landed");
        assert!(handoffs.is_empty());
    }

    #[test]
    fn done_preserves_every_handoff_field_in_written_order() {
        let payload = parse(
            "hop-done: the route is wired\n\
             handoff: reviewer | check the ACL gate\n\
             handoff: scribe\n",
        );

        let PayloadV2::Done { head, handoffs } = payload else {
            panic!("expected a valid done report");
        };
        assert_eq!(head, "the route is wired");
        assert_eq!(
            handoffs,
            vec![
                Handoff {
                    to_role: "reviewer".into(),
                    text: Some("check the ACL gate".into()),
                },
                Handoff {
                    to_role: "scribe".into(),
                    text: None,
                },
            ]
        );
    }

    #[test]
    fn failed_and_blocked_verdicts_expose_their_own_heads() {
        let failed = parse("hop-failed: the adapter dropped the frame");
        let PayloadV2::Failed {
            reason,
            handoffs: _,
        } = &failed
        else {
            panic!("expected a valid failed report");
        };
        assert_eq!(reason, "the adapter dropped the frame");
        assert!(failed.handoffs().is_empty());
        assert_eq!(failed.head_kind(), Some("failed"));
        assert_eq!(failed.verdict(), Some("the adapter dropped the frame"));

        let blocked = parse("hop-blocked: waiting for the credential vault");
        let PayloadV2::Blocked {
            reason,
            handoffs: _,
        } = &blocked
        else {
            panic!("expected a valid blocked report");
        };
        assert_eq!(reason, "waiting for the credential vault");
        assert!(blocked.handoffs().is_empty());
        assert_eq!(blocked.head_kind(), Some("blocked"));
        assert_eq!(blocked.verdict(), Some("waiting for the credential vault"));
    }

    #[test]
    fn missing_verdict_yields_no_route() {
        let error = invalid_error("# only notes\n\nhandoff: reviewer | take the next slice\n");

        assert!(error.contains("no verdict line"), "{error}");
    }

    #[test]
    fn second_verdict_is_rejected_at_the_line_that_broke_order() {
        let error = invalid_error(
            "hop-done: the first verdict stands\nhop-failed: the second verdict is noise\n",
        );

        assert!(error.contains("line 2"), "{error}");
        assert!(
            error.contains("more than one verdict line"),
            "the message must name the grammar violation: {error}"
        );
    }

    #[test]
    fn verdict_requires_body_text() {
        for line in ["hop-done:", "hop-failed:   \n", "hop-blocked:\r\n"] {
            let error = invalid_error(line);
            assert!(error.contains("line 1"), "{error}");
            assert!(
                error.contains("carries no"),
                "empty verdict {line:?} reported {error:?}"
            );
        }
    }

    #[test]
    fn handoff_line_cannot_precede_the_verdict() {
        let error =
            invalid_error("handoff: reviewer | begin first\nhop-done: verdict arrived late\n");

        assert!(error.contains("line 1"), "{error}");
        assert!(
            error.contains("verdict line before it"),
            "the message must identify the ordering violation: {error}"
        );
    }

    #[test]
    fn unknown_prefixed_line_reports_the_offending_prefix_and_line() {
        let error = invalid_error("hop-done: fine\nhop-skipped: this verdict cannot route\n");

        assert!(error.contains("line 2"), "{error}");
        assert!(error.contains("unknown report prefix"), "{error}");
        assert!(error.contains("hop-skipped"), "{error}");
    }

    #[test]
    fn bare_prose_line_without_a_prefix_is_invalid() {
        let error = invalid_error("hop-done: fine\nthe next line forgot its prefix\n");

        assert!(error.contains("line 2"), "{error}");
        assert!(
            error.contains("carries no prefix"),
            "the message must explain the missing prefix: {error}"
        );
    }

    #[test]
    fn comments_and_blanks_are_skipped_while_physical_line_numbers_persist() {
        let payload = parse(
            "# report header\n\nhop-done: line numbering tracks the source file\n\n\
             # a later note\nhandoff: scribe\n",
        );

        let PayloadV2::Done { head, handoffs } = payload else {
            panic!("expected comments and blank lines to remain nonstructural");
        };
        assert_eq!(head, "line numbering tracks the source file");
        assert_eq!(
            handoffs,
            vec![Handoff {
                to_role: "scribe".into(),
                text: None,
            }]
        );

        let error = invalid_error("# comment\n\nbad line\n");
        assert!(error.contains("line 3"), "{error}");
    }

    #[test]
    fn crlf_and_lone_cr_line_endings_normalize_before_parsing() {
        let crlf = parse("hop-done: written by a CRLF tool\r\nhandoff: reviewer | inspect it\r\n");
        let PayloadV2::Done { head, handoffs } = crlf else {
            panic!("CRLF report failed to parse");
        };
        assert_eq!(head, "written by a CRLF tool");
        assert_eq!(
            handoffs,
            vec![Handoff {
                to_role: "reviewer".into(),
                text: Some("inspect it".into()),
            }]
        );

        let lone_cr = parse("hop-done: written by a CR tool\rhandoff: reviewer | inspect it\r");
        let PayloadV2::Done { head, handoffs } = lone_cr else {
            panic!("lone CR report failed to parse");
        };
        assert_eq!(head, "written by a CR tool");
        assert_eq!(
            handoffs,
            vec![Handoff {
                to_role: "reviewer".into(),
                text: Some("inspect it".into()),
            }]
        );
    }

    #[test]
    fn seventeenth_physical_line_is_beyond_the_reader() {
        let text = format!(
            "hop-done: within the limit\n{}",
            (0..16)
                .map(|index| format!("# filler {index}"))
                .collect::<Vec<_>>()
                .join("\n")
        );

        let error = invalid_error(&text);
        assert!(
            error.contains(&format!(
                "payload exceeds the {MAX_REPORT_LINES}-line limit (17 lines)"
            )),
            "{error}"
        );
    }

    #[test]
    fn ninth_handoff_line_reports_the_grammar_limit() {
        let mut lines: Vec<String> = vec!["hop-done: fan out the review".into()];
        lines.extend((1..=9).map(|index| format!("handoff: reviewer-{index}")));
        let text = format!("{}\n", lines.join("\n"));

        let error = invalid_error(&text);
        assert!(error.contains("line 10"), "{error}");
        assert!(
            error.contains(&format!(
                "payload carries more than {MAX_REPORT_HANDOFFS} handoff lines"
            )),
            "{error}"
        );
    }

    #[test]
    fn handoff_role_grammar_rejects_empty_illegal_and_ambiguous_forms() {
        let empty = invalid_error("hop-done: ready\nhandoff:\n");
        assert!(empty.contains("line 2"), "{empty}");
        assert!(empty.contains("no target role"), "{empty}");

        let illegal = invalid_error("hop-done: ready\nhandoff: reviewer@desk | inspect\n");
        assert!(illegal.contains("line 2"), "{illegal}");
        assert!(
            illegal.contains("not a role name"),
            "the message must name the role violation: {illegal}"
        );

        let ambiguous = invalid_error("hop-done: ready\nhandoff: reviewer extra brief\n");
        assert!(ambiguous.contains("line 2"), "{ambiguous}");
        assert!(
            ambiguous.contains("without a \"|\" separator"),
            "the message must identify the missing separator: {ambiguous}"
        );
    }

    #[test]
    fn handoff_without_text_falls_back_to_the_verdict_body() {
        let payload = parse("hop-done: send the shared brief\nhandoff: reviewer\n");
        let PayloadV2::Done { head, handoffs } = payload else {
            panic!("expected a valid done report");
        };
        let handoff = &handoffs[0];
        assert_eq!(handoff.text, None);
        assert_eq!(handoff.text_or(&head), "send the shared brief");
    }

    #[test]
    fn valid_payloads_expose_routes_and_invalid_payloads_expose_none() {
        let done = parse("hop-done: complete\nhandoff: reviewer | inspect\n");
        let failed = parse("hop-failed: broken\nhandoff: scribe | record it\n");
        let blocked = parse("hop-blocked: waiting\nhandoff: planner | resolve it\n");

        assert_eq!(
            done.handoffs(),
            &[Handoff {
                to_role: "reviewer".into(),
                text: Some("inspect".into()),
            }]
        );
        assert_eq!(
            failed.handoffs(),
            &[Handoff {
                to_role: "scribe".into(),
                text: Some("record it".into()),
            }]
        );
        assert_eq!(
            blocked.handoffs(),
            &[Handoff {
                to_role: "planner".into(),
                text: Some("resolve it".into()),
            }]
        );

        let invalid = parse("no report shape here\n");
        assert_eq!(invalid.head_kind(), None);
        assert_eq!(invalid.verdict(), None);
        assert!(invalid.handoffs().is_empty());
    }
}
