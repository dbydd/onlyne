use super::*;
use tempfile::tempdir;

#[derive(Default)]
struct Sink {
    records: Mutex<Vec<ContentRecord>>,
}

impl ContentSink for Sink {
    fn publish(&self, record: ContentRecord) {
        self.records.lock().push(record);
    }
}

#[test]
fn role_cursor_indexes_original_records_across_tasks() {
    let dir = tempdir().unwrap();
    let sink = Arc::new(Sink::default());
    let writer = ContentWriter::default();
    writer.set_sink(sink.clone());
    let logs = dir.path().join(".onlyne/logs");
    let first = serde_json::json!({
        "sessionUpdate": "agent_message_chunk",
        "content": {"text": "one"}
    });
    let second = serde_json::json!({"onlyne": {"kind": "turn", "at": "2026-09-19T00:00:01Z"}});
    writer
        .append(
            dir.path(),
            "task-a",
            Some("session-a"),
            &logs.join("session-task-a.events.jsonl"),
            &first,
            "2026-09-19T00:00:00Z",
        )
        .unwrap();
    writer
        .append(
            dir.path(),
            "task-b",
            Some("session-b"),
            &logs.join("session-task-b.events.jsonl"),
            &second,
            "2026-09-19T00:00:01Z",
        )
        .unwrap();

    let restarted_sink = Arc::new(Sink::default());
    let restarted = ContentWriter::default();
    restarted.set_sink(restarted_sink.clone());
    let third = serde_json::json!({
        "sessionUpdate": "agent_thought_chunk",
        "content": {"text": "three"}
    });
    restarted
        .append(
            dir.path(),
            "task-a",
            Some("session-a"),
            &logs.join("session-task-a.events.jsonl"),
            &third,
            "2026-09-19T00:00:02Z",
        )
        .unwrap();

    let replay = read_content_records(dir.path()).unwrap();
    assert_eq!(
        replay.iter().map(|record| record.seq).collect::<Vec<_>>(),
        [1, 2, 3],
        "a new writer resumes the role cursor from its durable index"
    );
    assert_eq!(replay[0].record, first);
    assert_eq!(replay[1].record, second);
    assert_eq!(replay[2].record, third);
    assert_eq!(sink.records.lock().as_slice(), &replay[..2]);
    assert_eq!(restarted_sink.records.lock().as_slice(), &replay[2..]);
    let task_a = std::fs::read_to_string(logs.join("session-task-a.events.jsonl")).unwrap();
    let journal: Value = serde_json::from_str(task_a.lines().next().unwrap()).unwrap();
    assert_eq!(
        journal, first,
        "the index must not decorate the journal object"
    );
}
