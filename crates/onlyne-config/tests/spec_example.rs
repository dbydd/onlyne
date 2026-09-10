//! Guards for the repository's full-coverage example spec and the ACL fixtures.
//!
//! `.onlyne.example/spec.toml` is the only file that exercises every key in
//! `docs/v1-PLAN.md` §5. Nothing loaded it, so a key the plan's `[server]` block
//! does not contain sat in the file unnoticed until an operator copied it and
//! `deny_unknown_fields` refused the start. These tests hold the example and the
//! fixture set to one rule.
//!
//! The example carries `REPLACE_ME` credentials by design, so `Spec::load`
//! always stops in credential validation. The schema rule, which is where a
//! stale key fails, runs through `Spec::parse_str`; `Spec::load` is held to the
//! same rule on the fixtures, whose keys are shaped.

use onlyne_config::Spec;
use std::{net::SocketAddr, path::PathBuf, str::FromStr};

/// `<repo>/.onlyne.example/spec.toml`, independent of the working directory.
fn example_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.onlyne.example/spec.toml")
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The four invariants every spec file in this repository must satisfy.
fn check_invariants(label: &str, spec: &Spec) {
    assert!(
        !spec.server.name.is_empty(),
        "{label}: [server].name is empty; docs/v1-PLAN.md §5 line 222 requires it"
    );
    assert!(
        SocketAddr::from_str(&spec.server.listen).is_ok(),
        "{label}: [server].listen is not host:port shaped: {}",
        spec.server.listen
    );
    assert!(
        spec.server.cert_pin.starts_with("sha256/"),
        "{label}: [server].cert_pin must start with sha256/: {}",
        spec.server.cert_pin
    );

    let roles = spec.role_names();
    for edge in spec.acl_edges() {
        assert!(
            edge.from != "*" && edge.to != "*",
            "{label}: acl_edges emitted a wildcard endpoint: {} -> {}",
            edge.from,
            edge.to
        );
        assert!(
            roles.contains(&edge.from) && roles.contains(&edge.to),
            "{label}: acl_edges endpoint is not a registered role: {} -> {}",
            edge.from,
            edge.to
        );
    }

    let gateways: Vec<&str> = spec.gateway.iter().map(|entry| entry.id.as_str()).collect();
    for route in &spec.route {
        assert!(
            roles.contains(&route.to.role),
            "{label}: route targets unregistered role `{}`; the file is the only truth",
            route.to.role
        );
        assert!(
            gateways.contains(&route.gateway.as_str()),
            "{label}: route references unregistered gateway `{}`",
            route.gateway
        );
    }
}

/// The example ships `REPLACE_ME` credentials on purpose, so a raw load always
/// stops in credential validation. Substituting shaped credentials leaves the
/// file's key set untouched, which is what this guard checks: a stale key still
/// fails `deny_unknown_fields` with `unknown field`.
fn with_shaped_credentials(text: &str) -> String {
    text.replace(
        "sha256/REPLACE_ME",
        "sha256/0000000000000000000000000000000000000000000000000000000000000000",
    )
    .replace(
        "ed25519/REPLACE_ME",
        "ed25519/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
}

#[test]
fn example_spec_loads() {
    let path = example_path();
    let text = std::fs::read_to_string(&path).expect("example spec reads");
    let spec = match Spec::parse_str(&with_shaped_credentials(&text)) {
        Ok(spec) => spec,
        Err(error) => panic!(
            "example spec {} fails the closed schema; operator-facing rendering follows:\n{error}",
            path.display()
        ),
    };
    check_invariants(".onlyne.example/spec.toml", &spec);
}

#[test]
fn example_credentials_are_placeholders_and_load_reports_the_line() {
    let path = example_path();
    let error = Spec::load(&path).expect_err("the example uses REPLACE_ME placeholders");
    let rendered = error.to_string();
    assert!(
        rendered.starts_with("spec.toml:"),
        "load error must carry the spec.toml:<line> prefix, got: {rendered}"
    );
    assert!(
        rendered.contains("cert_pin") || rendered.contains("key"),
        "load error must name the credential, got: {rendered}"
    );
}

#[test]
fn fixtures_follow_one_rule() {
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(fixtures_dir()).expect("fixtures directory reads") {
        let path = entry.expect("directory entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let label = path.display().to_string();
        let spec = match Spec::load(&path) {
            Ok(spec) => spec,
            Err(error) => panic!("fixture {label} does not load: {error}"),
        };
        check_invariants(&label, &spec);
        names.push(
            path.file_name()
                .expect("fixture file name")
                .to_string_lossy()
                .into_owned(),
        );
    }
    names.sort();
    assert_eq!(names, vec!["spec-acl.toml"], "fixture set changed");
}
