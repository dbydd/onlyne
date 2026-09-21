use super::*;
use onlyne_proto::Presence;

fn slice(max: u32) -> RoleSlice {
    RoleSlice {
        command: vec!["pi".into()],
        max_sessions: max,
        relay_required: Vec::new(),
        relay_count: None,
    }
}

fn info(max: u32) -> RoleInfo {
    RoleInfo {
        name: "planner".into(),
        admin: false,
        max_sessions: max,
        session_command: vec!["pi".into()],
        spec_hash: "h".into(),
        prose: None,
        state: Presence::Online,
        sessions: 0,
        queued: 0,
        detail: None,
        edges: Vec::new(),
        aggregate: None,
        relay_required: None,
        relay_count: None,
    }
}

#[test]
fn a_changed_max_sessions_is_applied() {
    let current = slice(1);
    let next = RoleSlice::from_role_info(&info(2), &current);
    let (applied, fields) = apply_if_changed(&current, next).expect("a change");
    assert_eq!(applied.max_sessions, 2);
    assert_eq!(fields, ["max_sessions"]);
}

#[test]
fn an_identical_slice_is_a_no_op() {
    let current = slice(2);
    let next = RoleSlice::from_role_info(&info(2), &current);
    assert!(apply_if_changed(&current, next).is_none());
}

#[test]
fn command_is_compared() {
    let current = slice(1);
    let next = RoleSlice {
        command: vec!["other".into()],
        max_sessions: 1,
        relay_required: Vec::new(),
        relay_count: None,
    };
    let fields = slice_diff(&current, &next);
    assert_eq!(fields, ["session_command"]);
}

#[test]
fn a_changed_relay_policy_is_compared() {
    let current = slice(1);
    let armed = RoleSlice {
        relay_required: vec!["writer".into()],
        relay_count: Some(2),
        ..current.clone()
    };
    assert_eq!(
        slice_diff(&current, &armed),
        ["relay_required", "relay_count"],
        "a list, a count, and the pair each report the keys they moved"
    );

    let counted = RoleSlice {
        relay_count: Some(2),
        ..current.clone()
    };
    assert_eq!(slice_diff(&current, &counted), ["relay_count"]);

    // The role row the client adopts carries the same policy it would see
    // in a welcome, so a reload arms a live session's next spawn.
    let from_row = RoleSlice::from_role_info(
        &RoleInfo {
            relay_required: Some(vec!["writer".into()]),
            relay_count: Some(2),
            ..info(1)
        },
        &current,
    );
    assert_eq!(from_row.relay_required, vec!["writer".to_string()]);
    assert_eq!(
        slice_diff(&current, &from_row),
        ["relay_required", "relay_count"]
    );
}
