//! Slice 4 acceptance: the spec surface (`spec_get`, `spec_apply`) and the one
//! streaming admin subscribe.
//!
//! The editing layer's text fidelity is pinned in `spec_edits.rs`'s own unit
//! tests; this file pins the surface against a running server: `spec_get`'s
//! source hash, `spec_apply`'s conflict and invalid refusals (both writing
//! nothing), the `set_targets`-without-restart effect on the ACL, and the
//! subscribe/drop/resume cursor with no gap and no repeat.

use onlyne_config::source_hash;
use onlyne_proto::{
    AdminOp, Body, Envelope, ErrorCode, Event, Frame, MsgKind, Principal, ResBody, SetProse,
    SetTargets, SpecApply, SpecEdit, SpecReloaded, Subscribe, new_envelope,
};
use onlyne_server::router::Session;
use onlyne_server::state::{Server, ServerInit};
use onlyne_server::{admin, events, relay};
use onlyne_wire::socket::{connect_local, socket_path};
use onlyne_wire::{read_frame, write_frame};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

/// A spec whose `planner` may reach only `planner`, so a note to `builder` is
/// refused by the ACL until `set_targets` adds `builder`.
fn spec() -> String {
    let planner = onlyne_net::KeyPair::from_seed([1_u8; 32]).public_str();
    let builder = onlyne_net::KeyPair::from_seed([2_u8; 32]).public_str();
    format!(
        "[server]\n\
         name = \"local\"\n\
         listen = \"127.0.0.1:0\"\n\
         cert_pin = \"sha256/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"\n\
         note_queue = true\n\
         heartbeat_timeout_ms = 1000\n\n\
         [[client]]\n\
         role = \"planner\"\n\
         key = \"{planner}\"\n\
         allowed_senders = [\"planner\"]\n\
         allowed_targets = [\"planner\"]\n\n\
         [[client]]\n\
         role = \"builder\"\n\
         key = \"{builder}\"\n\
         allowed_senders = [\"planner\", \"builder\"]\n\
         allowed_targets = [\"planner\"]\n"
    )
}

struct Fixture {
    _dir: TempDir,
    root: std::path::PathBuf,
    state: Arc<onlyne_server::State>,
}

impl Fixture {
    fn spec_path(&self) -> std::path::PathBuf {
        self.root.join(".onlyne/spec.toml")
    }
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("server");
    std::fs::create_dir_all(root.join(".onlyne")).expect("create root");
    std::fs::write(root.join(".onlyne/spec.toml"), spec()).expect("write spec");
    let state = Server::open(&ServerInit {
        root: root.clone(),
        listen: None,
    })
    .expect("open the server");
    Fixture {
        _dir: dir,
        root,
        state,
    }
}

/// The `spec_get` answer's `data`.
async fn spec_get(state: &Arc<onlyne_server::State>) -> Value {
    let mut session = Session::default();
    let body =
        onlyne_server::router::dispatch_admin(state, &mut session, AdminOp::SpecGet(json!({})))
            .await;
    assert!(body.ok, "spec_get answered an error: {body:?}");
    body.data.expect("spec_get carries data")
}

/// Run one `spec_apply` and hand back its body.
async fn spec_apply(state: &Arc<onlyne_server::State>, apply: SpecApply) -> ResBody {
    let mut session = Session::default();
    onlyne_server::router::dispatch_admin(state, &mut session, AdminOp::SpecApply(apply)).await
}

/// A note envelope from one role to another.
fn note(from: &str, to: &str, text: &str) -> Envelope {
    new_envelope(
        MsgKind::Note,
        Principal::role(from),
        Principal::role(to),
        Body::text(text),
        None,
    )
    .expect("valid note")
}

/// The error a refused send carries.
fn refused(reply: relay::RelayReply) -> relay::RelayReject {
    match reply {
        relay::RelayReply::Rejected(reject) => reject,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn spec_get_hashes_the_files_bytes_not_the_canonical_form() {
    let fixture = fixture().await;
    let before = spec_get(&fixture.state).await;
    let hash = before["source_hash"]
        .as_str()
        .expect("source_hash")
        .to_string();
    let file_bytes = std::fs::read(fixture.spec_path()).expect("read spec");
    assert_eq!(hash, source_hash(&file_bytes), "the byte hash");

    // A comment-only change moves the hash, because the hash is over bytes.
    let with_comment = format!("# a note\n{}", std::str::from_utf8(&file_bytes).unwrap());
    std::fs::write(fixture.spec_path(), &with_comment).expect("write comment");
    let after = spec_get(&fixture.state).await;
    assert_ne!(
        after["source_hash"].as_str().unwrap(),
        hash,
        "a comment moves the source hash"
    );

    // A rewrite with the same bytes does not.
    std::fs::write(fixture.spec_path(), &with_comment).expect("write again");
    let again = spec_get(&fixture.state).await;
    assert_eq!(
        again["source_hash"].as_str().unwrap(),
        after["source_hash"].as_str().unwrap(),
        "identical bytes hash to the same value"
    );
}

#[tokio::test]
async fn a_stale_base_hash_answers_conflict_and_writes_nothing() {
    let fixture = fixture().await;
    let before = std::fs::read(fixture.spec_path()).expect("read spec before");
    let body = spec_apply(
        &fixture.state,
        SpecApply {
            base_hash: "deadbeef".to_string(),
            edits: vec![SpecEdit::SetProse(SetProse {
                role: "planner".to_string(),
                prose: "moved".to_string(),
            })],
        },
    )
    .await;
    assert!(!body.ok, "a stale hash is refused");
    let error = body.error.expect("an error");
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(error.field.as_deref(), Some("base_hash"));
    let after = std::fs::read(fixture.spec_path()).expect("read spec after");
    assert_eq!(before, after, "a conflict writes nothing");
}

#[tokio::test]
async fn an_edit_the_loader_refuses_answers_invalid_naming_the_line() {
    let fixture = fixture().await;
    let view = spec_get(&fixture.state).await;
    let base_hash = view["source_hash"].as_str().unwrap().to_string();
    let before = std::fs::read(fixture.spec_path()).expect("read spec before");
    // `upsert_role` with a key the loader's own check refuses: the document the
    // edits produce parses as TOML but does not parse as a spec, so the refusal
    // is `invalid` naming the file and line the loader's voice uses.
    let body = spec_apply(
        &fixture.state,
        SpecApply {
            base_hash,
            edits: vec![SpecEdit::UpsertRole(onlyne_proto::UpsertRole {
                role: "planner".to_string(),
                key: Some("ed25519/not-a-real-key".to_string()),
                ..onlyne_proto::UpsertRole::default()
            })],
        },
    )
    .await;
    assert!(!body.ok, "an edit the loader refuses is refused");
    let error = body.error.expect("an error");
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(
        error
            .field
            .as_deref()
            .is_some_and(|field| field.ends_with("spec.toml")),
        "the field names the file: {error:?}"
    );
    assert!(
        error.message.contains("spec.toml:"),
        "the message names a line the same voice the loader uses: {error:?}"
    );
    let after = std::fs::read(fixture.spec_path()).expect("read spec after");
    assert_eq!(before, after, "an invalid edit writes nothing");
}

#[tokio::test]
async fn set_targets_takes_effect_without_a_restart_and_publishes_spec_reloaded() {
    let fixture = fixture().await;

    // A note the ACL refuses: planner does not reach builder yet.
    let refused = refused(
        relay::send(
            &fixture.state,
            &note("planner", "builder", "hi"),
            false,
            None,
        )
        .expect("relay"),
    );
    assert_eq!(refused.code, ErrorCode::AclDenied);

    let view = spec_get(&fixture.state).await;
    let base_hash = view["source_hash"].as_str().unwrap().to_string();
    let head_before = fixture.state.event_head();

    let body = spec_apply(
        &fixture.state,
        SpecApply {
            base_hash,
            edits: vec![SpecEdit::SetTargets(SetTargets {
                role: "planner".to_string(),
                targets: vec!["planner".to_string(), "builder".to_string()],
            })],
        },
    )
    .await;
    assert!(body.ok, "set_targets applied: {body:?}");

    // The reload the apply drove publishes a `spec_reloaded` event.
    let reloaded = events::replay(
        &fixture.state,
        head_before as u64,
        &events::EventFilter::default(),
        100,
    )
    .expect("replay")
    .rows
    .into_iter()
    .any(|row| matches!(row.event, Event::SpecReloaded(_)));
    assert!(reloaded, "the apply publishes spec_reloaded");

    // The same note the ACL refused a moment ago is now accepted, with no
    // restart between them — the ACL the reload rebuilt is the one this read.
    let reply = relay::send(
        &fixture.state,
        &note("planner", "builder", "hi"),
        false,
        None,
    )
    .expect("relay");
    assert!(
        matches!(reply, relay::RelayReply::Accepted(_)),
        "the note the ACL refused is accepted after set_targets: {reply:?}"
    );
}

/// One spec_reloaded event with a stable shape, for the cursor test.
fn reloaded(seq: u64) -> Event {
    let _ = seq;
    Event::SpecReloaded(SpecReloaded {
        spec_hash: "h".to_string(),
        roles: 0,
        gateways: 0,
        routes: 0,
    })
}

#[tokio::test]
async fn a_subscriber_that_drops_and_resumes_sees_every_event_once() {
    let fixture = fixture().await;
    let listener = admin::bind(&fixture.state).expect("bind the admin socket");
    let socket = socket_path(&fixture.root).expect("socket path");
    let server = fixture.state.clone();
    let serve = tokio::spawn(admin::serve_socket(server.clone(), listener));

    // Subscribe from the head; the page is empty, and the stream carries what
    // follows.
    let mut first = connect_local(&socket).await.expect("connect first");
    let request = Frame::req(
        "r1".to_string(),
        AdminOp::Subscribe(Subscribe {
            since_seq: 0,
            tiers: vec![],
            kinds: vec![],
            roles: vec![],
        }),
    );
    write_frame(&mut first, &request).await.expect("subscribe");
    let response = read_frame::<_, Frame>(&mut first)
        .await
        .expect("page")
        .expect("a frame");
    let Frame::Res { body, .. } = response else {
        panic!("the first frame is the page: {response:?}");
    };
    assert!(body.ok, "subscribe answered an error: {body:?}");

    // Emit two events; read them off the stream.
    let seq1 = server.emit(reloaded(1)).expect("emit 1");
    let seq2 = server.emit(reloaded(2)).expect("emit 2");
    let frame1 = read_frame::<_, Frame>(&mut first)
        .await
        .expect("read 1")
        .expect("a frame");
    let frame2 = read_frame::<_, Frame>(&mut first)
        .await
        .expect("read 2")
        .expect("a frame");
    let seen1 = event_seq(&frame1);
    let seen2 = event_seq(&frame2);
    assert_eq!(seen1, seq1, "the first event");
    assert_eq!(seen2, seq2, "the second event");

    // Drop the connection, then emit three more.
    drop(first);
    tokio::time::sleep(Duration::from_millis(10)).await;
    let seq3 = server.emit(reloaded(3)).expect("emit 3");
    let seq4 = server.emit(reloaded(4)).expect("emit 4");
    let seq5 = server.emit(reloaded(5)).expect("emit 5");

    // Resume from the last seq the first connection saw.
    let mut second = connect_local(&socket).await.expect("connect second");
    let resume = Frame::req(
        "r2".to_string(),
        AdminOp::Subscribe(Subscribe {
            since_seq: seen2,
            tiers: vec![],
            kinds: vec![],
            roles: vec![],
        }),
    );
    write_frame(&mut second, &resume).await.expect("resume");
    let page = read_frame::<_, Frame>(&mut second)
        .await
        .expect("page 2")
        .expect("a frame");
    let Frame::Res { body, .. } = page else {
        panic!("the first frame is the resumed page: {page:?}");
    };
    assert!(body.ok, "resume answered an error: {body:?}");
    let mut resumed: Vec<u64> = body
        .data
        .expect("data")
        .get("events")
        .expect("events")
        .as_array()
        .expect("an array")
        .iter()
        .map(|row| row["seq"].as_u64().expect("seq"))
        .collect();
    resumed.sort();

    // The drain carries anything the page did not fit; here the page holds all
    // three, so the stream is quiet. Read once with a short timeout to confirm
    // nothing repeats.
    let extra = timeout(
        Duration::from_millis(200),
        read_frame::<_, Frame>(&mut second),
    )
    .await;
    if let Ok(Ok(Some(frame))) = extra {
        if let Some(seq) = event_seq_opt(&frame) {
            resumed.push(seq);
            resumed.sort();
        }
    }
    drop(second);
    serve.abort();

    // No gap: the union is the contiguous range the five events form. No
    // repeat: each seq appears exactly once.
    let mut all = vec![seen1, seen2];
    all.extend(resumed);
    all.sort();
    assert_eq!(
        all,
        vec![seq1, seq2, seq3, seq4, seq5],
        "the subscriber saw every event exactly once"
    );
}

/// The `seq` of one event frame.
fn event_seq(frame: &Frame) -> u64 {
    event_seq_opt(frame).expect("an event frame")
}

fn event_seq_opt(frame: &Frame) -> Option<u64> {
    match frame {
        Frame::Ev { seq, .. } => Some(*seq),
        _ => None,
    }
}
