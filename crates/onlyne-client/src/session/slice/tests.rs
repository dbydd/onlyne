use super::*;
use onlyne_proto::Presence;

fn slice(max: u32) -> RoleSlice {
    RoleSlice {
        drive: onlyne_config::Drive::Plugin,
        command: vec!["pi".into()],
        max_sessions: max,
        required_targets: Vec::new(),
    }
}

fn info(max: u32) -> RoleInfo {
    RoleInfo {
        name: "planner".into(),
        admin: false,
        max_sessions: max,
        runtime: onlyne_proto::RoleRuntime {
            drive: onlyne_proto::Drive::Plugin,
            command: vec!["pi".into()],
        },
        spec_hash: "h".into(),
        prose: None,
        state: Presence::Online,
        sessions: 0,
        queued: 0,
        detail: None,
        edges: Vec::new(),
        aggregate: None,
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
        drive: onlyne_config::Drive::Plugin,
        command: vec!["other".into()],
        max_sessions: 1,
        required_targets: Vec::new(),
    };
    let fields = slice_diff(&current, &next);
    assert_eq!(fields, ["runtime"]);
}

/// A reload that widens or narrows the edges moves the obligation with them:
/// one list answers both questions, so the diff names the spec key that moved.
#[test]
fn a_changed_allowed_targets_is_compared() {
    let current = slice(1);
    let armed = RoleSlice {
        required_targets: vec!["writer".into()],
        ..current.clone()
    };
    assert_eq!(
        slice_diff(&current, &armed),
        ["allowed_targets"],
        "the declaration the guard reads is the one the diff reports"
    );

    // The role row the client adopts carries the same list it would see in a
    // welcome, so a reload moves the obligation of a live session's next spawn.
    let from_row = RoleSlice::from_role_info(
        &RoleInfo {
            edges: vec!["writer".into()],
            ..info(1)
        },
        &current,
    );
    assert_eq!(from_row.required_targets, vec!["writer".to_string()]);
    assert_eq!(slice_diff(&current, &from_row), ["allowed_targets"]);
}
