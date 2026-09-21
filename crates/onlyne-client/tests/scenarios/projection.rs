//! The session projection the client publishes: readiness ordering, the heartbeat
//! publish on lifecycle writes, and what a no-op beat republishes.

use crate::common::{
    RecordingOutbox, plugin_beat, projection_publishes_of, published_projection, spawn_ready,
};
use onlyne_client::session::dispatch::{DispatchState, on_plugin_report};
use onlyne_proto::ClientOp;
use onlyne_session::SessionLedger;
use onlyne_session::backend::fake::FakeBackend;
use onlyne_store::ClientStore;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;

/// The published frame's `projection` payload for a working session that has
/// taken its first delivery, captured off the wire before the projection publish
/// moved onto the heartbeat report. The bytes are the contract: the server mirrors
/// this object into the row it stores, so a republished projection that moves a
/// key or a word is a projection change, not a frame change.
const PUBLISHED_WORKING_PROJECTION: &str = concat!(
    r#"{"lifecycle":"working","agent":"running","delivery":"pending","#,
    r#""resource":"attached","recovery":"none","observed":{"agent":"running","#,
    r#""delivery":"pending","generation_live":true,"isolate_after":1,"mismatch_count":0,"#,
    r#""recovery":"none","resource":"attached","#,
    r#""terminate_after":3,"version":{"generation":1,"seq":7}}}"#
);

#[tokio::test]
async fn ready_is_reported_before_assign() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        backend,
        store,
    );
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());

    let (task_id, mut record_rx) = spawn_ready(&state, "task 1").await;
    let recorded = tokio::time::timeout(Duration::from_secs(2), record_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recorded, format!("assign:{}", task_id));

    let kinds = outbox.kinds().await;
    assert_eq!(
        kinds.first(),
        Some(&"report"),
        "the ready report leaves first: {kinds:?}"
    );
    assert!(
        !outbox.projection_publishes().await.is_empty(),
        "the projection follows the report: {kinds:?}"
    );
}

#[tokio::test]
async fn lifecycle_write_publishes_its_projection_in_a_heartbeat() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        backend,
        store.clone(),
    );
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());
    let (task_id, _assigns) = spawn_ready(&state, "task 1").await;

    // A heartbeat whose version is the stored watermark plus one, and whose
    // body is the stored observation with the agent idle: a legal transition.
    let row = store.get_session(&task_id).unwrap().unwrap();
    let mut body: onlyne_session::Observation = serde_json::from_str(&row.observed_json).unwrap();
    body.agent = onlyne_session::AgentState::Idle;
    body.version = onlyne_session::Version::new(row.generation as u64, row.seq as u64 + 1);
    on_plugin_report(
        &state,
        None,
        plugin_beat(
            &task_id,
            row.generation as u64,
            row.seq as u64 + 1,
            serde_json::to_value(body).unwrap(),
        ),
    )
    .await
    .unwrap();

    let frames = outbox.frames().await;
    let published = projection_publishes_of(&frames);
    let Some(published) = published.last() else {
        panic!("a lifecycle write must publish its projection")
    };
    assert_eq!(published.task_id, task_id);
    assert_eq!(published.projection.agent, onlyne_proto::AgentPhase::Idle);
    assert_eq!(
        store.get_session(&task_id).unwrap().unwrap().agent_state,
        "idle"
    );
}

/// The publish on the wire, byte for byte.
///
/// One seeded working row, one frame out of `sync_session`. The frame is a
/// `report` heartbeat — there is no projection verb left to send — and its
/// `projection` payload is the text the standalone projection frame carried for
/// the same row. An aggregate role's stamp stays off this frame too: the publish
/// has never named an origin cluster.
#[tokio::test]
async fn the_publish_is_a_heartbeat_carrying_the_stored_projection() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        backend,
        store.clone(),
    );
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());
    state.set_cluster_ref("cluster-b");

    let task_id = "11111111-1111-4111-8111-111111111111".to_string();
    let observation = onlyne_session::Observation::build(
        onlyne_session::Version::new(1, 7),
        true,
        onlyne_session::DEFAULT_ISOLATE_AFTER,
        onlyne_session::DEFAULT_TERMINATE_AFTER,
        0,
        onlyne_session::AgentState::Running,
        onlyne_session::DeliveryState::Pending,
        onlyne_session::ResourceState::Attached,
        onlyne_session::RecoveryState::None,
    );
    store
        .upsert_session(
            &task_id,
            &onlyne_session::to_versioned(&observation, "pane-1", "null").unwrap(),
        )
        .unwrap();

    onlyne_client::session::dispatch::sync_session(&state, &task_id)
        .await
        .unwrap();

    let frames = outbox.frames().await;
    let [ClientOp::Report(onlyne_proto::Report::Heartbeat { .. })] = frames.as_slice() else {
        panic!("one heartbeat report publishes the session: {frames:?}");
    };
    let value = serde_json::to_value(&frames[0]).unwrap();
    assert_eq!(value["op"], "report");
    assert_eq!(value["args"]["kind"], "heartbeat");
    assert!(
        value["args"]["data"].get("cluster_ref").is_none(),
        "the publish names no origin cluster: {value}"
    );

    let published = projection_publishes_of(&frames);
    assert_eq!(published.len(), 1);
    let published = published.first().expect("one publish");
    assert_eq!(published.task_id, task_id);
    assert_eq!(published.session_id, task_id);
    assert_eq!((published.generation, published.seq), (1, 7));
    // The tuple the row holds is the reducer's own observation: no verdict and
    // no public view are in it, so the `lifecycle` in the frame below is
    // something `project` answered, not something storage was read for.
    let stored_tuple: serde_json::Value =
        serde_json::from_str(&store.get_session(&task_id).unwrap().unwrap().observed_json)
            .expect("the stored tuple is JSON");
    for claim in ["outcome", "public", "lifecycle"] {
        assert!(
            stored_tuple.get(claim).is_none(),
            "the session row claims no {claim}: {stored_tuple}"
        );
    }
    assert_eq!(
        serde_json::to_string(&published.projection).unwrap(),
        PUBLISHED_WORKING_PROJECTION,
        "the published projection is the bytes the server mirrored before"
    );
    // The beat's own observation and the projection's are the same tuple, so a
    // reader of either field sees one truth.
    assert_eq!(
        published.observed,
        published.projection.observed.clone().unwrap()
    );
    assert_eq!(
        published.projection,
        published_projection(&store, &task_id),
        "the frame is the same answer the two durable rows give"
    );
    assert_eq!(published.cluster_ref, None);
}

#[tokio::test]
async fn noop_heartbeats_republish_the_projection() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        backend,
        store.clone(),
    );
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());
    let (task_id, _assigns) = spawn_ready(&state, "task 1").await;
    outbox.clear().await;

    let row = store.get_session(&task_id).unwrap().unwrap();
    let mut body: onlyne_session::Observation = serde_json::from_str(&row.observed_json).unwrap();
    let generation = row.generation as u64;
    let seq_one = row.seq as u64 + 1;
    let seq_two = row.seq as u64 + 2;
    body.version = onlyne_session::Version::new(generation, seq_one);
    on_plugin_report(
        &state,
        None,
        plugin_beat(
            &task_id,
            generation,
            seq_one,
            serde_json::to_value(&body).unwrap(),
        ),
    )
    .await
    .unwrap();
    body.version = onlyne_session::Version::new(generation, seq_two);
    on_plugin_report(
        &state,
        None,
        plugin_beat(
            &task_id,
            generation,
            seq_two,
            serde_json::to_value(&body).unwrap(),
        ),
    )
    .await
    .unwrap();

    let syncs = outbox.projection_publishes().await;
    eprintln!(
        "DBG row_after_junk={}",
        store.get_session(&task_id).unwrap().unwrap().observed_json
    );
    for (i, s) in syncs.iter().enumerate() {
        eprintln!(
            "DBG sync[{i}] v=({},{}) observed={}",
            s.generation, s.seq, s.observed
        );
    }
    assert_eq!(
        syncs.len(),
        2,
        "each accepted no-op heartbeat publishes one projection: {syncs:?}"
    );
    assert_eq!((syncs[0].generation, syncs[0].seq), (generation, seq_one));
    assert_eq!((syncs[1].generation, syncs[1].seq), (generation, seq_two));
    assert_eq!(syncs[0].projection, syncs[1].projection);
    let stored = store.get_session(&task_id).unwrap().unwrap();
    assert_eq!(syncs[0].projection, published_projection(&store, &task_id));
    assert_eq!(
        (stored.generation as u64, stored.seq as u64),
        (generation, seq_two)
    );
}

#[tokio::test]
async fn stale_heartbeat_does_not_republish() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        backend,
        store.clone(),
    );
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());
    let (task_id, _assigns) = spawn_ready(&state, "task 1").await;
    outbox.clear().await;

    let row = store.get_session(&task_id).unwrap().unwrap();
    let mut body: onlyne_session::Observation = serde_json::from_str(&row.observed_json).unwrap();
    let generation = row.generation as u64;
    let seq = row.seq as u64 + 1;
    body.version = onlyne_session::Version::new(generation, seq);
    on_plugin_report(
        &state,
        None,
        plugin_beat(
            &task_id,
            generation,
            seq,
            serde_json::to_value(&body).unwrap(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(outbox.projection_publishes().await.len(), 1);

    body.version = onlyne_session::Version::new(generation, seq);
    on_plugin_report(
        &state,
        None,
        plugin_beat(
            &task_id,
            generation,
            seq,
            serde_json::to_value(&body).unwrap(),
        ),
    )
    .await
    .unwrap();
    body.version = onlyne_session::Version::new(generation, seq.saturating_sub(1));
    on_plugin_report(
        &state,
        None,
        plugin_beat(
            &task_id,
            generation,
            seq.saturating_sub(1),
            serde_json::to_value(&body).unwrap(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        outbox.projection_publishes().await.len(),
        1,
        "a beat at or below the published watermark adds no frame"
    );
}

#[tokio::test]
async fn heartbeat_junk_in_the_client_dimensions_writes_none_of_it() {
    let dir = tempdir().unwrap();
    let store = ClientStore::open(dir.path().join("client.db")).unwrap();
    let backend = Arc::new(FakeBackend::new());
    let state = DispatchState::new(
        "planner",
        dir.path(),
        vec!["echo".into()],
        2,
        backend,
        store.clone(),
    );
    let outbox = Arc::new(RecordingOutbox::default());
    state.attach_outbox(outbox.clone());
    let (task_id, _assigns) = spawn_ready(&state, "task 1").await;
    outbox.clear().await;

    let row = store.get_session(&task_id).unwrap().unwrap();
    let mut junk: onlyne_session::Observation = serde_json::from_str(&row.observed_json).unwrap();
    let tuning = (
        junk.isolate_after,
        junk.terminate_after,
        junk.mismatch_count,
    );
    junk.isolate_after = 0;
    junk.terminate_after = 0;
    junk.mismatch_count = 7;
    junk.version = onlyne_session::Version::new(row.generation as u64, row.seq as u64 + 1);
    on_plugin_report(
        &state,
        None,
        plugin_beat(
            &task_id,
            row.generation as u64,
            row.seq as u64 + 1,
            serde_json::to_value(&junk).unwrap(),
        ),
    )
    .await
    .unwrap();
    // Composed away, the beat said what the row already said, so it is a no-op on
    // the tuple — and a no-op is still liveness: the watermark moves and the client
    // republishes, with its own tuning and never the junk the body carried.
    let after_junk = store.get_session(&task_id).unwrap().unwrap();
    assert_eq!(
        (after_junk.generation, after_junk.seq),
        (row.generation, row.seq + 1),
        "a discarded claim is still a heartbeat the server times"
    );
    let syncs = outbox.projection_publishes().await;
    assert_eq!(
        syncs.len(),
        1,
        "the discarded beat still republished: {syncs:?}"
    );
    let published: onlyne_session::Observation =
        serde_json::from_value(syncs[0].observed.clone()).unwrap();
    assert_eq!(
        (
            published.isolate_after,
            published.terminate_after,
            published.mismatch_count
        ),
        tuning,
        "the wire carries the client's tuning, not the body's"
    );
    assert!(
        onlyne_session::is_legal(&published),
        "an illegal tuple must not reach the publish: {published:?}"
    );
    let mut legal: onlyne_session::Observation =
        serde_json::from_str(&after_junk.observed_json).unwrap();
    let generation = after_junk.generation as u64;
    let seq = after_junk.seq as u64 + 1;
    legal.agent = onlyne_session::AgentState::Idle;
    legal.version = onlyne_session::Version::new(generation, seq);
    on_plugin_report(
        &state,
        None,
        plugin_beat(
            &task_id,
            generation,
            seq,
            serde_json::to_value(&legal).unwrap(),
        ),
    )
    .await
    .unwrap();
    let syncs = outbox.projection_publishes().await;
    assert_eq!(
        syncs.len(),
        2,
        "the next legal beat still publishes: {syncs:?}"
    );
    assert_eq!((syncs[1].generation, syncs[1].seq), (generation, seq));
    let published: onlyne_session::Observation =
        serde_json::from_value(syncs[1].observed.clone()).unwrap();
    assert_eq!(
        published.agent,
        onlyne_session::AgentState::Idle,
        "the agent the plugin witnessed is still taken as sent"
    );
    assert_eq!(
        (
            published.isolate_after,
            published.terminate_after,
            published.mismatch_count
        ),
        tuning,
        "and the discard did not swallow the rest of the body"
    );
}
