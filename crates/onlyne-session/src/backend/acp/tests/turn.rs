//! What a turn journals and how it settles.

use super::*;

#[test]
fn a_turn_journals_its_asking_its_answer_and_its_ending() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let session = backend.spawn(fake.spec("t-journal")).unwrap();
    let (outcome, lines, log) = run_turn(&backend, &session, "t-journal", "fix the bug");

    assert_eq!(outcome.outcome, TaskState::Done);
    assert_eq!(outcome.head.as_deref(), Some("I edited hello.py."));
    assert!(outcome.note.is_none());
    assert!(outcome.refusals.is_none());

    // Our two records bracket the agent's stream, in the order a reader needs:
    // what was asked, everything the agent sent, how the turn ended.
    let dispatch = onlyne_records(&lines, "dispatch");
    assert_eq!(dispatch.len(), 1, "{lines:?}");
    assert_eq!(dispatch[0]["task_id"], "t-journal");
    // The dispatch record is the whole prompt: the task's prose with this
    // backend's completion directive appended.
    let prose = dispatch[0]["prose"].as_str().expect("a prompt is text");
    assert!(prose.starts_with("fix the bug"), "{prose}");
    assert!(
        prose.contains("Result report (write before you stop): "),
        "{prose}"
    );
    assert!(dispatch[0]["at"].as_str().unwrap().ends_with('Z'));
    assert!(lines[0].get("onlyne").is_some());
    let turn = onlyne_records(&lines, "turn");
    assert_eq!(turn.len(), 1);
    assert_eq!(turn[0]["stop_reason"], "end_turn");
    assert_eq!(turn[0]["head"], "I edited hello.py.");
    assert_eq!(lines.last().unwrap()["onlyne"]["kind"], "turn");
    assert_eq!(
        lines[1..lines.len() - 1]
            .iter()
            .filter(|line| line.get("onlyne").is_none())
            .count(),
        7,
        "every raw update of the turn is recorded: {lines:?}"
    );
    assert!(lines[1]["sessionUpdate"].is_string());
    // The raw frames keep the agent's own sessionId, which is what lets a
    // reader demultiplex a shared process.
    assert_eq!(lines[1]["sessionId"], "sess-1");
    assert!(
        lines
            .iter()
            .any(|line| line["sessionUpdate"] == "available_commands_update")
    );

    // The rendered surface an operator tails.
    assert!(log.contains("> Let me try.\n"), "{log}");
    assert!(
        log.contains("tool call_9 Edit hello.py kind=edit\n"),
        "{log}"
    );
    assert!(log.contains("tool call_9 status=completed\n"), "{log}");
    assert!(log.contains("I edited hello.py.\n"), "{log}");
    assert!(
        !log.contains("<status>"),
        "markers stay out of the log: {log}"
    );
    assert!(!log.contains("availableCommands"), "{log}");
    assert!(!log.contains("fix the bug"), "the log is the agent's half");
    finish(&backend, &fake, &[&session]);
}

#[test]
fn another_sessions_text_stays_out_of_this_turns_journal() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let session = backend.spawn(fake.spec("t-noise")).unwrap();
    let (outcome, lines, log) = run_turn(&backend, &session, "t-noise", "MARK:noise go");
    assert_eq!(outcome.outcome, TaskState::Done);
    assert!(!log.contains("not this turn"), "{log}");
    assert!(
        !lines.iter().any(|line| line["sessionId"] == "sess-other"),
        "{lines:?}"
    );
    finish(&backend, &fake, &[&session]);
}

#[test]
fn every_stop_reason_that_is_not_an_answer_settles_a_turn() {
    // (prompt, what the ledger records, task suffix, the reason word the turn
    // record carries, and what the fault note has to say — `None` meaning the
    // ending is clean enough to need no explanation)
    let cases = [
        (
            "MARK:cancel",
            TaskState::Cancelled,
            "cancelled",
            "cancelled",
            Some("cancelled"),
        ),
        (
            "MARK:maxtokens",
            TaskState::Failed,
            "over-tokens",
            "max_tokens",
            Some("max_tokens"),
        ),
        (
            "MARK:refusal",
            TaskState::Failed,
            "refused",
            "refusal",
            Some("refusal"),
        ),
        (
            "MARK:weird",
            TaskState::Failed,
            "unknown-reason",
            "stopped_by_hook",
            Some("stopped_by_hook"),
        ),
        (
            "MARK:nostop",
            TaskState::Failed,
            "no-stop-reason",
            "(absent)",
            Some("(absent)"),
        ),
        (
            "MARK:noanswer MARK:nostop",
            TaskState::Failed,
            "silent-and-cut",
            "(absent)",
            Some("no answer"),
        ),
        (
            "MARK:noanswer",
            TaskState::Done,
            "silent-answer",
            "end_turn",
            None,
        ),
    ];
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    for (marker, settled, name, reason, note) in cases {
        let task = format!("t-{name}");
        let session = backend.spawn(fake.spec(&task)).unwrap();
        let (outcome, lines, _log) = run_turn(&backend, &session, &task, marker);
        assert_eq!(outcome.outcome, settled, "{marker}");
        let record = &onlyne_records(&lines, "turn")[0];
        assert_eq!(record["stop_reason"], reason, "{marker}");
        // The head is what the receiving role reads as this turn's answer.
        let head = record.get("head").and_then(Value::as_str);
        if name.starts_with("silent") {
            assert_eq!(head, None, "{marker}");
        } else {
            assert_eq!(head, Some("I edited hello.py."), "{marker}");
        }
        match note {
            None => assert!(outcome.note.is_none(), "{marker}: {:?}", outcome.note),
            Some(want) => assert!(
                outcome
                    .note
                    .as_deref()
                    .is_some_and(|got| got.contains(want)),
                "{marker}: {:?}",
                outcome.note
            ),
        }
        backend
            .close(&session, CloseReason::Completed, false)
            .expect("close");
    }
    // One process served all seven ACP sessions, because the command never
    // changed, and every session has now been handed back.
    assert_eq!(
        fake.traced().matches("new sess-").count(),
        7,
        "{}",
        fake.traced()
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !backend.state.agents.lock().is_empty() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(backend.state.agents.lock().is_empty());
    assert!(
        fake.traced().contains("eof"),
        "the agent left with its client"
    );
}

#[test]
fn a_blocked_journal_never_loses_a_turn() {
    let fake = Fake::new();
    // The rendered log's own path is a directory before the session starts, so
    // every append to it fails.
    let blocked = fake
        .root
        .path()
        .join(".onlyne")
        .join("logs")
        .join("session-t-blocked.log");
    fs::create_dir_all(&blocked).expect("make the log path a directory");
    let backend = AcpBackend::new(AcpOptions::default());
    let session = backend.spawn(fake.spec("t-blocked")).unwrap();
    let (outcome, _lines, _log) = run_turn(&backend, &session, "t-blocked", "carry on");
    assert_eq!(outcome.outcome, TaskState::Done);
    assert_eq!(outcome.head.as_deref(), Some("I edited hello.py."));
    finish(&backend, &fake, &[&session]);
}
