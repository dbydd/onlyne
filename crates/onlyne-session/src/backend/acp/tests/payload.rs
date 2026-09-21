//! The payload-v2 result report: the path it travels, the file it is read from.

use super::*;

/// One row of the payload matrix: the bytes pre-placed at the report path
/// (nothing for the absent rows, which must behave exactly as a turn
/// without the contract), the prose marker that chooses the agent's stop
/// reason, and everything the ending has to leave behind. The note
/// expectations match as substrings; the head is compared exactly. A row
/// whose `payload_kind` is `invalid` keeps its file; every other row has it
/// consumed.
struct Cell {
    name: &'static str,
    payload: Option<&'static [u8]>,
    marker: &'static str,
    outcome: TaskState,
    head: Option<&'static str>,
    note: Option<&'static str>,
    payload_kind: &'static str,
}

#[test]
fn a_payload_report_replaces_the_head_and_can_only_lower_the_ending() {
    const DONE_TEXT: &str = "payload says the edit landed";
    const FAIL_TEXT: &str = "payload says the hop was abandoned";
    const DONE_FILE: &[u8] = b"hop-done: payload says the edit landed\n";
    const FAIL_FILE: &[u8] = b"hop-failed: payload says the hop was abandoned\n";
    let cells = [
        Cell {
            name: "absent-end",
            payload: None,
            marker: "go",
            outcome: TaskState::Done,
            head: Some("I edited hello.py."),
            note: None,
            payload_kind: "absent",
        },
        Cell {
            name: "absent-cut",
            payload: None,
            marker: "MARK:maxtokens go",
            outcome: TaskState::Failed,
            head: Some("I edited hello.py."),
            note: Some("max_tokens"),
            payload_kind: "absent",
        },
        Cell {
            name: "done-end",
            payload: Some(DONE_FILE),
            marker: "go",
            outcome: TaskState::Done,
            head: Some(DONE_TEXT),
            note: None,
            payload_kind: "done",
        },
        // The heart of the contract: a cheerful report cannot promote a
        // turn the stop reason already vetoed.
        Cell {
            name: "done-cannot-promote",
            payload: Some(DONE_FILE),
            marker: "MARK:maxtokens go",
            outcome: TaskState::Failed,
            head: Some(DONE_TEXT),
            note: Some("max_tokens"),
            payload_kind: "done",
        },
        Cell {
            name: "failed-end",
            payload: Some(FAIL_FILE),
            marker: "go",
            outcome: TaskState::Failed,
            head: Some(FAIL_TEXT),
            note: Some(FAIL_TEXT),
            payload_kind: "failed",
        },
        Cell {
            name: "failed-cut",
            payload: Some(FAIL_FILE),
            marker: "MARK:maxtokens go",
            outcome: TaskState::Failed,
            head: Some(FAIL_TEXT),
            // Both halves of a cut-short turn survive into the one note the
            // fault record carries: what the stop reason proved, then what
            // the agent said.
            note: Some("max_tokens; the agent reported: payload says the hop was abandoned"),
            payload_kind: "failed",
        },
        Cell {
            name: "empty-file",
            payload: Some(b""),
            marker: "go",
            outcome: TaskState::Cancelled,
            head: None,
            note: Some("acp payload invalid: payload is empty"),
            payload_kind: "invalid",
        },
        Cell {
            name: "empty-file-cut",
            payload: Some(b""),
            marker: "MARK:maxtokens go",
            outcome: TaskState::Cancelled,
            head: None,
            note: Some("acp payload invalid: payload is empty"),
            payload_kind: "invalid",
        },
        Cell {
            name: "two-lines",
            payload: Some(b"hop-done: one\nhop-failed: two\n"),
            marker: "go",
            outcome: TaskState::Cancelled,
            head: None,
            note: Some("acp payload invalid: line 2: payload carries more than one verdict line"),
            payload_kind: "invalid",
        },
        Cell {
            name: "unknown-prefix",
            payload: Some(b"hop-tripped: neither form\n"),
            marker: "MARK:maxtokens go",
            outcome: TaskState::Cancelled,
            head: None,
            note: Some("acp payload invalid: line 1: unknown report prefix \"hop-tripped\""),
            payload_kind: "invalid",
        },
        Cell {
            name: "empty-report",
            payload: Some(b"hop-done:\n"),
            marker: "go",
            outcome: TaskState::Cancelled,
            head: None,
            note: Some("acp payload invalid: line 1: hop-done carries no result"),
            payload_kind: "invalid",
        },
        Cell {
            name: "bare-prose",
            payload: Some(b"the agent typed its answer into the file\n"),
            marker: "MARK:maxtokens go",
            outcome: TaskState::Cancelled,
            head: None,
            note: Some("acp payload invalid: line 1: report line carries no prefix"),
            payload_kind: "invalid",
        },
        Cell {
            name: "not-utf8",
            payload: Some(&[0xff, 0xf7, 0x00]),
            marker: "go",
            outcome: TaskState::Cancelled,
            head: None,
            note: Some("acp payload invalid: payload is not valid utf-8"),
            payload_kind: "invalid",
        },
    ];
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    for cell in cells {
        let task = format!("t-{}", cell.name);
        let path = payload_path(fake.root.path(), &task);
        if let Some(bytes) = cell.payload {
            fs::create_dir_all(path.parent().expect("the report path names a file"))
                .expect("create the report directory");
            fs::write(&path, bytes).expect("pre-place the report");
        }
        let session = backend.spawn(fake.spec(&task)).unwrap();
        let (outcome, lines, _log) = run_turn(&backend, &session, &task, cell.marker);
        assert_eq!(outcome.outcome, cell.outcome, "{}", cell.name);
        assert_eq!(outcome.head.as_deref(), cell.head, "{}", cell.name);
        match cell.note {
            None => assert!(outcome.note.is_none(), "{}: {:?}", cell.name, outcome.note),
            Some(want) => assert!(
                outcome
                    .note
                    .as_deref()
                    .is_some_and(|got| got.contains(want)),
                "{}: {:?}",
                cell.name,
                outcome.note
            ),
        }
        // An accepted report is consumed on the read: a requeued task id
        // starts from nothing rather than from the last turn's words. A
        // refused one stays on disk as the evidence for the record below.
        assert_eq!(
            path.exists(),
            cell.payload_kind == "invalid",
            "{}: the report file took the wrong side of its read",
            cell.name
        );
        let records = onlyne_records(&lines, "payload");
        assert_eq!(records.len(), 1, "{}: {lines:?}", cell.name);
        assert_eq!(records[0]["task_id"], task, "{}", cell.name);
        assert_eq!(
            records[0]["payload_kind"], cell.payload_kind,
            "{}",
            cell.name
        );
        assert_eq!(
            records[0]["path"],
            Value::from(path.display().to_string()),
            "{}",
            cell.name
        );
        let head = records[0].get("head").and_then(Value::as_str);
        assert_eq!(head, cell.head, "{}: {lines:?}", cell.name);
        // The turn record closes the journal with the same head.
        assert_eq!(
            onlyne_records(&lines, "turn")[0]
                .get("head")
                .and_then(Value::as_str),
            cell.head,
            "{}",
            cell.name
        );
        backend
            .close(&session, CloseReason::Completed, false)
            .expect("close");
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !backend.state.agents.lock().is_empty() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(backend.state.agents.lock().is_empty());
}

#[test]
fn the_prompt_hands_the_agent_the_path_the_ending_reads() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let session = backend.spawn(fake.spec("t-path")).unwrap();
    let (_outcome, lines, _log) = run_turn(&backend, &session, "t-path", "carry on");
    let path = payload_path(fake.root.path(), "t-path");
    let prose = onlyne_records(&lines, "dispatch")[0]["prose"]
        .as_str()
        .expect("the prompt is recorded")
        .to_string();
    // The directive carries the absolute path this backend will read, and
    // the agent received the same text it is told it received.
    assert!(
        prose.contains(&format!(
            "Result report (write before you stop): {}",
            path.display()
        )),
        "{prose}"
    );
    assert!(
        fake.traced().contains(&format!(
            "Result report (write before you stop): {}",
            path.display()
        )),
        "{}",
        fake.traced()
    );
    // The client makes the directory before the agent is asked to write
    // into it: a fresh workspace has no `.onlyne/out` of its own.
    assert!(
        payload_dir(fake.root.path()).is_dir(),
        "the report directory must stand ready for the agent"
    );
    finish(&backend, &fake, &[&session]);
}

#[test]
fn an_unbuildable_report_directory_costs_only_the_directive() {
    let fake = Fake::new();
    // `out` is a regular file: neither `create_dir_all` nor a report path
    // under it can exist.
    let onlyne = fake.root.path().join(".onlyne");
    fs::create_dir_all(&onlyne).expect("create the workspace state dir");
    fs::write(onlyne.join("out"), "not a directory").expect("block the report dir");
    let backend = AcpBackend::new(AcpOptions::default());
    let session = backend.spawn(fake.spec("t-nowrite")).unwrap();
    let (outcome, lines, _log) = run_turn(&backend, &session, "t-nowrite", "carry on");
    // The turn settles on its stop reason alone, exactly the branch a
    // missing report takes: no cancel conjured by the failed bookkeeping.
    assert_eq!(outcome.outcome, TaskState::Done);
    assert_eq!(outcome.head.as_deref(), Some("I edited hello.py."));
    assert!(outcome.note.is_none(), "{:?}", outcome.note);
    let prose = onlyne_records(&lines, "dispatch")[0]["prose"]
        .as_str()
        .expect("the prompt is recorded")
        .to_string();
    assert_eq!(prose, "carry on", "no directive with an unreachable path");
    let warnings = onlyne_records(&lines, "warning");
    assert_eq!(warnings.len(), 1, "{lines:?}");
    assert!(
        warnings[0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("report directory not created")),
        "{warnings:?}"
    );
    assert_eq!(
        onlyne_records(&lines, "payload")[0]["payload_kind"],
        "absent"
    );
    finish(&backend, &fake, &[&session]);
}
