//! Plan checks for the three listing reads, and the audit table beside them.
//!
//! Both listings run on a timer — a server poll or a TUI refresh reads them about
//! once a second — so the shape of their query plan decides a constant write
//! rate. An order SQLite cannot satisfy from an index makes it scan the table and
//! spill a sorter into a temporary file on every read, which measured at
//! megabytes per second of writes nobody asked for. These assert the plan rather
//! than a duration, because the plan is the fact that decides it.

use super::*;

/// Enough rows that a sorter has to spill. SQLite keeps a small sorter in
/// memory, and five thousand rows carrying a payload column is past the point it
/// reaches for a temporary file.
const SEED: u32 = 5000;

fn seeded() -> (tempfile::TempDir, ServerLedger) {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = ServerLedger::open(dir.path().join("state.db"), 7).expect("open the store");
    {
        let conn = ledger.conn().expect("connection");
        conn.execute_batch(&format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<{SEED})
             INSERT INTO sessions(task_id,role,session_id,generation,seq,agent_state,delivery_state,resource_state,recovery_substate,desired_json,observed_json,mismatch_count,updated_at)
             SELECT 't'||i,'planner','s'||i,1,i,'idle','none','attached','none','null','{{\"lifecycle\":\"working\"}}',0,printf('2026-09-22T%02d:00:00Z',i%24) FROM n;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<{SEED})
             INSERT INTO ledger(msg_id,op_id,kind,from_json,to_json,attempt,state,enqueued_at)
             SELECT 'm'||i,'op'||i,'note','{{}}','{{}}',0,'queued',printf('2026-09-22T%02d:00:00Z',i%24) FROM n;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<{SEED})
             INSERT INTO ghost_sweeps(task_id,role,session_id,generation,seq_before,seq_after,outcome,evidence,swept_at)
             SELECT 't'||i,'planner','s'||i,1,i,i+1,'done','task_settled:acked',printf('2026-09-22T%02d:00:00Z',i%24) FROM n;"
        ))
        .expect("seed");
    }
    (dir, ledger)
}

fn plan(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .expect("prepare the plan");
    // The listings end in `LIMIT ?`, and a plan still checks its parameters, so
    // bind one placeholder per parameter the statement declares.
    let params = vec![SqlValue::Integer(0); stmt.parameter_count()];
    stmt.query_map(params_from_iter(params), |row| row.get::<_, String>(3))
        .expect("plan rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("plan")
}

/// The filter the server's scan uses for a lifecycle, exactly as `list_sessions`
/// builds it: the key lives inside the published projection, and bytes that do
/// not parse read back as `created`.
const LIFECYCLE_FILTER: &str = "IFNULL(CASE WHEN json_valid(observed_json) THEN json_extract(observed_json,'$.lifecycle') END,'created')='working'";

/// Neither listing may fall back to a sorter, filtered or unfiltered.
#[test]
fn the_listing_reads_are_served_by_an_index() {
    let (_dir, ledger) = seeded();
    let conn = ledger.conn().expect("connection");
    for sql in [
        sessions_list_sql(""),
        sessions_list_sql(&format!(" WHERE {LIFECYCLE_FILTER}")),
        ledger_list_sql(""),
        ledger_list_sql(" WHERE state='queued'"),
        ghost_sweeps_list_sql(),
    ] {
        let lines = plan(&conn, &sql);
        assert!(
            lines.iter().all(|line| !line.contains("TEMP B-TREE")),
            "this read spills a sorter into a temporary file on every execution: {lines:?}\n{sql}"
        );
        assert!(
            lines.iter().any(|line| line.contains("INDEX")),
            "this read does not use an index: {lines:?}\n{sql}"
        );
    }
}

/// One audit row per settlement, read back in the order the listing promises.
///
/// A pass settles several rows inside one second, so the listing carries two
/// order keys: the stamp, and the insertion counter that breaks the ties inside
/// it. The round trip covers every column, the outcome and its evidence text
/// included.
#[test]
fn a_ghost_sweep_round_trips_through_the_audit_table() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = ServerLedger::open(dir.path().join("state.db"), 7).expect("open the store");
    let base = 1_789_000_000;
    let record = |task: &str, outcome: Outcome, evidence: &str, at: i64| {
        let id = ledger
            .record_ghost_sweep(&GhostSweepRow {
                id: 0,
                task_id: task.to_string(),
                role: "builder".to_string(),
                session_id: "sess-1".to_string(),
                generation: 3,
                seq_before: 11,
                seq_after: 12,
                outcome,
                evidence: evidence.to_string(),
                swept_at: at,
            })
            .expect("record the sweep");
        (id, at)
    };

    let (oldest, oldest_at) = record("t-old", Outcome::Done, "task_settled:acked", base);
    let (middle, middle_at) = record(
        "t-middle",
        Outcome::Failed,
        "task_settled:rejected",
        base + 60,
    );
    let (newest, newest_at) = record("t-new", Outcome::Failed, "task_settled:expired", base + 60);
    assert_eq!(middle_at, newest_at, "one pass lands inside one second");

    let rows = ledger.list_ghost_sweeps(10).expect("list the sweeps");
    assert_eq!(
        rows.iter()
            .map(|row| row.task_id.as_str())
            .collect::<Vec<_>>(),
        vec!["t-new", "t-middle", "t-old"],
        "the stamp orders the rows and the insertion counter breaks its ties"
    );
    assert_eq!(
        rows[0],
        GhostSweepRow {
            id: newest,
            task_id: "t-new".to_string(),
            role: "builder".to_string(),
            session_id: "sess-1".to_string(),
            generation: 3,
            seq_before: 11,
            seq_after: 12,
            outcome: Outcome::Failed,
            evidence: "task_settled:expired".to_string(),
            swept_at: newest_at,
        },
        "every column comes back as it went in"
    );
    assert_eq!(rows[1].id, middle);
    assert_eq!(rows[2].id, oldest);
    assert_eq!(rows[2].outcome, Outcome::Done);
    assert_eq!(rows[2].swept_at, oldest_at);

    let capped = ledger.list_ghost_sweeps(1).expect("list the sweeps");
    assert_eq!(
        capped
            .iter()
            .map(|row| row.task_id.as_str())
            .collect::<Vec<_>>(),
        vec!["t-new"],
        "the limit cuts the older rows off the end"
    );
}
