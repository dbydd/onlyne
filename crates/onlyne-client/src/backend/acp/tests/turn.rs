//! What a turn journals and how it settles.

use super::*;

#[test]
fn a_nudge_is_a_turn_of_its_own_and_the_journal_says_which_it_was() {
    use crate::session::dispatch::NUDGE_TEXT;

    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let session = backend.spawn(fake.spec("t-nudge")).unwrap();
    let feed = backend
        .outcomes()
        .expect("an acp backend reports its own endings");
    let events = journal(&session, "events");

    // A turn the agent ends without a completion is the ending §3c's rule reads,
    // and the rule's own answer to it is one sentence on the same conversation.
    backend
        .deliver(&session, "t-nudge", "MARK:noanswer look at the tests")
        .expect("the first turn");
    let first = await_outcome(&feed, "t-nudge");
    assert_eq!(first.outcome, NO_VERDICT);

    backend
        .nudge(&session, "t-nudge", NUDGE_TEXT)
        .expect("a nudge is a turn");
    let second = await_outcome(&feed, "t-nudge");
    assert_eq!(second.outcome, NO_VERDICT);
    assert_eq!(
        fake.traced().matches("prompt ").count(),
        2,
        "the nudge reached the agent as a prompt of its own: {}",
        fake.traced()
    );

    // The journal separates the assignment from the nudge: a reader of the
    // session's record has to be able to tell what the client asked for from
    // what the rule asked again.
    let lines = jsonl(&events);
    assert_eq!(onlyne_records(&lines, "dispatch").len(), 1, "{lines:?}");
    let nudges = onlyne_records(&lines, "nudge");
    assert_eq!(nudges.len(), 1, "{lines:?}");
    assert_eq!(nudges[0]["task_id"], "t-nudge");
    assert_eq!(nudges[0]["prompt"], NUDGE_TEXT);
    finish(&backend, &fake, &[&session]);
}

#[test]
fn a_turn_journals_its_asking_its_answer_and_its_ending() {
    let fake = Fake::new();
    let backend = AcpBackend::new(AcpOptions::default());
    let session = backend.spawn(fake.spec("t-journal")).unwrap();
    let (outcome, lines, log) = run_turn(&backend, &session, "t-journal", "fix the bug");

    assert_eq!(outcome.outcome, NO_VERDICT);
    assert_eq!(outcome.head.as_deref(), Some("I edited hello.py."));
    assert!(outcome.note.is_none());
    assert!(outcome.refusals.is_none());

    // Our two records bracket the agent's stream, in the order a reader needs:
    // what was asked, everything the agent sent, how the turn ended.
    let dispatch = onlyne_records(&lines, "dispatch");
    assert_eq!(dispatch.len(), 1, "{lines:?}");
    assert_eq!(dispatch[0]["task_id"], "t-journal");
    // The dispatch record's own field is the prompt, and the prompt is the task's prose
    // alone: this backend appends no directive of its own.
    let prompt = dispatch[0]["prompt"].as_str().expect("a prompt is text");
    assert_eq!(prompt, "fix the bug");
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
    assert_eq!(outcome.outcome, NO_VERDICT);
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
            Some(TaskState::Cancelled),
            "cancelled",
            "cancelled",
            Some("cancelled"),
        ),
        (
            "MARK:maxtokens",
            Some(TaskState::Failed),
            "over-tokens",
            "max_tokens",
            Some("max_tokens"),
        ),
        (
            "MARK:refusal",
            Some(TaskState::Failed),
            "refused",
            "refusal",
            Some("refusal"),
        ),
        (
            "MARK:weird",
            Some(TaskState::Failed),
            "unknown-reason",
            "stopped_by_hook",
            Some("stopped_by_hook"),
        ),
        (
            "MARK:nostop",
            Some(TaskState::Failed),
            "no-stop-reason",
            "(absent)",
            Some("(absent)"),
        ),
        (
            "MARK:noanswer MARK:nostop",
            Some(TaskState::Failed),
            "silent-and-cut",
            "(absent)",
            Some("no answer"),
        ),
        (
            "MARK:noanswer",
            NO_VERDICT,
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
    assert_eq!(outcome.outcome, NO_VERDICT);
    assert_eq!(outcome.head.as_deref(), Some("I edited hello.py."));
    finish(&backend, &fake, &[&session]);
}
