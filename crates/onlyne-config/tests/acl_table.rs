//! Table-driven reference for the §3 `MsgKind` rules, §5 ACL lists, and the
//! §5 line 274 decision point.
//!
//! Fixture: `tests/fixtures/spec-acl.toml`, registered roles `planner`,
//! `builder`, `reviewer`, `silent`, `isolated`, `_supervisor`, `scout`.
use onlyne_config::{AclEdge, MsgKindClass, Spec};

const FIXTURE: &str = include_str!("fixtures/spec-acl.toml");

/// The minimal fragment `onlyne-client init` writes for a self-delivering role.
const INIT_FRAGMENT: &str = r#"[server]
name = "cluster-min"
listen = "0.0.0.0:7811"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"

[[client]]
role = "planner"
key = "ed25519/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
allowed_senders = ["*", "planner"]
allowed_targets = ["planner"]
"#;

/// The same role with the wildcard sender list and no explicit self name.
const INIT_FRAGMENT_NO_SELF: &str = r#"[server]
name = "cluster-min"
listen = "0.0.0.0:7811"
cert_pin = "sha256/0000000000000000000000000000000000000000000000000000000000000000"

[[client]]
role = "planner"
key = "ed25519/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
allowed_senders = ["*"]
allowed_targets = ["planner"]
"#;

fn spec() -> Spec {
    Spec::parse_str(FIXTURE).expect("ACL fixture parses")
}

#[test]
fn edges_never_carry_a_wildcard_endpoint() {
    let edges = spec().acl_edges();
    assert!(
        edges.iter().all(|edge| edge.from != "*" && edge.to != "*"),
        "wildcard reached the table"
    );
}

#[test]
fn every_endpoint_names_a_registered_role() {
    let spec = spec();
    let roles = spec.role_names();
    assert!(
        spec.acl_edges()
            .iter()
            .all(|edge| roles.contains(&edge.from) && roles.contains(&edge.to))
    );
}

#[test]
fn init_fragment_self_delivery_holds_with_and_without_the_explicit_entries() {
    // The shipped fragment keeps `allowed_senders = ["*", "<self>"]` and
    // `allowed_targets = ["<self>"]` as belt-and-braces. The unconditional self
    // edge makes the row present either way, and the pair set stays identical.
    let with_self = Spec::parse_str(INIT_FRAGMENT).expect("init fragment parses");
    let wildcard_only = Spec::parse_str(INIT_FRAGMENT_NO_SELF).expect("fragment parses");
    for spec in [&with_self, &wildcard_only] {
        assert!(has_edge(
            &spec.acl_edges(),
            "planner",
            "planner",
            MsgKindClass::Any
        ));
    }
    assert_eq!(with_self.acl_edges(), wildcard_only.acl_edges());
}

/// One expected permission lookup.
struct Case {
    name: &'static str,
    from: &'static str,
    to: &'static str,
    kind: MsgKindClass,
    expected: bool,
}

/// Cases in the order the audit brief lists them.
const CASES: &[Case] = &[
    // 1. Empty `allowed_targets`: zero outbound while still receiving.
    Case {
        name: "empty targets: no outbound to planner",
        from: "silent",
        to: "planner",
        kind: MsgKindClass::Any,
        expected: false,
    },
    Case {
        name: "self edge exists for a role with no grants at all",
        from: "silent",
        to: "silent",
        kind: MsgKindClass::Any,
        expected: true,
    },
    Case {
        name: "empty targets: still receives from planner",
        from: "planner",
        to: "silent",
        kind: MsgKindClass::Any,
        expected: true,
    },
    // 2. Empty `allowed_senders`: zero inbound while still sending.
    Case {
        name: "empty senders: no inbound from planner",
        from: "planner",
        to: "isolated",
        kind: MsgKindClass::Any,
        expected: false,
    },
    Case {
        name: "empty senders: still sends to planner",
        from: "isolated",
        to: "planner",
        kind: MsgKindClass::Any,
        expected: true,
    },
    // 3. The self edge is unconditional; the wildcard covers every other role.
    Case {
        name: "explicit self name does not change the self row",
        from: "planner",
        to: "planner",
        kind: MsgKindClass::Any,
        expected: true,
    },
    Case {
        name: "self edge exists for builder despite wildcard senders",
        from: "builder",
        to: "builder",
        kind: MsgKindClass::Any,
        expected: true,
    },
    Case {
        name: "self edge exists for a role that refuses all senders",
        from: "isolated",
        to: "isolated",
        kind: MsgKindClass::Any,
        expected: true,
    },
    Case {
        name: "self edge exists for the aggregate role in the control class",
        from: "_supervisor",
        to: "_supervisor",
        kind: MsgKindClass::Control,
        expected: true,
    },
    Case {
        name: "wildcard covers other roles",
        from: "planner",
        to: "reviewer",
        kind: MsgKindClass::Any,
        expected: true,
    },
    Case {
        name: "wildcard covers builder from planner",
        from: "planner",
        to: "builder",
        kind: MsgKindClass::Any,
        expected: true,
    },
    // 4. Aggregate role naming one parent-visible role.
    Case {
        name: "aggregate role reaches planner",
        from: "_supervisor",
        to: "planner",
        kind: MsgKindClass::Any,
        expected: true,
    },
    Case {
        name: "aggregate role cannot reach builder",
        from: "_supervisor",
        to: "builder",
        kind: MsgKindClass::Any,
        expected: false,
    },
    // 5. Control class rows exist; the admin half rides on the edge flag.
    Case {
        name: "non-admin control row is permission-bearing",
        from: "planner",
        to: "planner",
        kind: MsgKindClass::Control,
        expected: true,
    },
    Case {
        name: "admin control row is permission-bearing",
        from: "_supervisor",
        to: "planner",
        kind: MsgKindClass::Control,
        expected: true,
    },
    // 6. Note class rows exist; offline refusal is the server's runtime rule.
    Case {
        name: "note row exists to a probably-offline role",
        from: "planner",
        to: "silent",
        kind: MsgKindClass::Note,
        expected: true,
    },
    // 7. Control follows the same pair permission.
    Case {
        name: "control unreachable without target grant",
        from: "builder",
        to: "_supervisor",
        kind: MsgKindClass::Control,
        expected: false,
    },
    Case {
        name: "control reaches planner from scout",
        from: "scout",
        to: "planner",
        kind: MsgKindClass::Control,
        expected: true,
    },
];

#[test]
fn fixture_parses_and_registers_document_order() {
    assert_eq!(
        spec().role_names(),
        vec![
            "planner",
            "builder",
            "reviewer",
            "silent",
            "isolated",
            "_supervisor",
            "scout"
        ]
    );
}

#[test]
fn table_driven_acl_cases() {
    let edges = spec().acl_edges();
    for case in CASES {
        let rows = edges
            .iter()
            .filter(|edge| edge.from == case.from && edge.to == case.to && edge.kind == case.kind)
            .count();
        assert_eq!(rows, usize::from(case.expected), "{}", case.name);
    }
}

#[test]
fn empty_targets_role_reaches_only_itself_and_still_receives() {
    let edges = spec().acl_edges();
    let outbound: Vec<&str> = edges
        .iter()
        .filter(|edge| edge.from == "silent")
        .map(|edge| edge.to.as_str())
        .collect();
    assert!(!outbound.is_empty());
    assert!(outbound.iter().all(|to| *to == "silent"));
    assert!(
        edges
            .iter()
            .any(|edge| edge.to == "silent" && edge.from != "silent")
    );
}

#[test]
fn empty_senders_role_admits_only_itself_and_still_sends() {
    let edges = spec().acl_edges();
    let inbound: Vec<&str> = edges
        .iter()
        .filter(|edge| edge.to == "isolated")
        .map(|edge| edge.from.as_str())
        .collect();
    assert!(!inbound.is_empty());
    assert!(inbound.iter().all(|from| *from == "isolated"));
    assert!(
        edges
            .iter()
            .any(|edge| edge.from == "isolated" && edge.to != "isolated")
    );
}

#[test]
fn wildcard_expands_to_every_other_role_and_explicit_self_adds_its_own() {
    let spec = spec();
    let edges = spec.acl_edges();
    let mut planner_targets: Vec<&str> = edges
        .iter()
        .filter(|edge| edge.from == "planner" && edge.kind == MsgKindClass::Any)
        .map(|edge| edge.to.as_str())
        .collect();
    planner_targets.sort_unstable();
    planner_targets.dedup();
    // `isolated` is absent because its empty `allowed_senders` refuses every
    // sender, so the wildcard grant never becomes a row.
    assert_eq!(
        planner_targets,
        vec![
            "_supervisor",
            "builder",
            "planner",
            "reviewer",
            "scout",
            "silent"
        ]
    );
    let mut isolated_targets: Vec<&str> = edges
        .iter()
        .filter(|edge| edge.from == "isolated" && edge.kind == MsgKindClass::Any)
        .map(|edge| edge.to.as_str())
        .collect();
    isolated_targets.sort_unstable();
    isolated_targets.dedup();
    // The wildcard grant becomes a row only where the receiver accepts
    // `isolated`; `reviewer` and `_supervisor` name only `planner`. The self row
    // arrives from the unconditional rule.
    assert_eq!(
        isolated_targets,
        vec!["builder", "isolated", "planner", "scout", "silent"]
    );
}

#[test]
fn every_registered_role_reaches_itself_with_every_class() {
    let spec = spec();
    let edges = spec.acl_edges();
    for role in spec.role_names() {
        for kind in [MsgKindClass::Any, MsgKindClass::Note, MsgKindClass::Control] {
            assert!(
                has_edge(&edges, &role, &role, kind),
                "missing self edge for {role} in {kind:?}"
            );
        }
    }
}

#[test]
fn an_explicit_self_name_does_not_duplicate_the_self_row() {
    let edges = spec().acl_edges();
    // `planner` lists itself in both lists and `silent` lists nothing.
    for role in ["planner", "silent"] {
        let count = edges
            .iter()
            .filter(|edge| edge.from == role && edge.to == role)
            .count();
        assert_eq!(count, 3, "{role} self rows");
    }
}

#[test]
fn wildcard_covers_every_other_registered_role() {
    let edges = spec().acl_edges();
    assert!(has_edge(&edges, "isolated", "silent", MsgKindClass::Any));
    assert!(has_edge(&edges, "planner", "reviewer", MsgKindClass::Any));
}

/// The table is the only decision surface this crate publishes; permit
/// questions belong to `onlyne_net::acl_allows`.
fn has_edge(edges: &[AclEdge], from: &str, to: &str, kind: MsgKindClass) -> bool {
    edges
        .iter()
        .any(|edge| edge.from == from && edge.to == to && edge.kind == kind)
}

#[test]
fn aggregate_role_reaches_itself_and_the_parent_visible_role() {
    let mut targets: Vec<String> = spec()
        .acl_edges()
        .iter()
        .filter(|edge| edge.from == "_supervisor" && edge.kind == MsgKindClass::Any)
        .map(|edge| edge.to.clone())
        .collect();
    targets.sort();
    targets.dedup();
    assert_eq!(targets, vec!["_supervisor", "planner"]);
}

#[test]
fn aggregate_annotation_contributes_no_edges() {
    let with_annotation = spec();
    let without = Spec::parse_str(&FIXTURE.replace("aggregate = \"cluster-b\"\n", ""))
        .expect("fixture parses without the annotation");
    assert_eq!(with_annotation.acl_edges(), without.acl_edges());
}

#[test]
fn admin_flag_rides_on_every_row_of_the_sending_role() {
    let edges = spec().acl_edges();
    let planner_rows: Vec<&AclEdge> = edges.iter().filter(|edge| edge.from == "planner").collect();
    let supervisor_rows: Vec<&AclEdge> = edges
        .iter()
        .filter(|edge| edge.from == "_supervisor")
        .collect();
    assert!(!planner_rows.is_empty());
    assert!(!supervisor_rows.is_empty());
    assert!(planner_rows.iter().all(|edge| !edge.admin));
    assert!(supervisor_rows.iter().all(|edge| edge.admin));

    // A non-admin sender may still hold a `Control` row. §3 allows that only
    // when the sender owns the task, a fact the caller supplies.
    assert!(edges.iter().any(|edge| edge.from == "planner"
        && edge.to == "planner"
        && edge.kind == MsgKindClass::Control
        && !edge.admin));
}

#[test]
fn unregistered_names_are_dropped() {
    assert!(spec().acl_edges().iter().all(|edge| edge.to != "ghost"));
}

#[test]
fn note_queue_defaults_to_false_so_offline_notes_are_refused() {
    let spec = spec();
    assert!(!spec.server.note_queue);
    // The `Note` row is permission. The server refuses offline delivery while
    // `note_queue` stays false, which is the default asserted above.
    assert!(spec.acl_edges().iter().any(|edge| edge.from == "planner"
        && edge.to == "silent"
        && edge.kind == MsgKindClass::Note));
}

#[test]
fn every_permitted_pair_carries_three_class_rows() {
    let edges = spec().acl_edges();
    let pairs: std::collections::BTreeSet<(&str, &str)> = edges
        .iter()
        .map(|edge| (edge.from.as_str(), edge.to.as_str()))
        .collect();
    // 13 cross-role pairs plus 7 unconditional self pairs over 7 roles.
    assert_eq!(pairs.len(), 20);
    assert_eq!(edges.len(), pairs.len() * 3);
    assert_eq!(edges.len(), 60);
    for (from, to) in &pairs {
        for kind in [MsgKindClass::Any, MsgKindClass::Note, MsgKindClass::Control] {
            assert!(
                edges
                    .iter()
                    .any(|edge| edge.from == *from && edge.to == *to && edge.kind == kind),
                "{from} -> {to} missing {kind:?}"
            );
        }
    }
}
