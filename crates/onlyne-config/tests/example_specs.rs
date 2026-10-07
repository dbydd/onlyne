//! The shipped example specs are what an operator copies.
//!
//! `packaging/check-example.sh` used to guard this by loading
//! `.onlyne.example/spec.toml` through the parser and calling the run a failure
//! when the file named a key the schema does not carry. The binary it invoked
//! is gone, so the gate stopped running while its reason stayed on: an operator
//! who copied the example into a real root inherited every key in it, and a key
//! this build ignores fails silently — the server starts, the setting keeps its
//! default, and the file looks correct. Six `running_ms` entries sat in a live
//! spec for exactly that reason.
//!
//! So the contract is pinned here instead: the example parses, and it names no
//! key this build ignores. A key added to the example and retired from the
//! schema at the same time cannot merge quietly.

use onlyne_config::Spec;
use onlyne_config::keys::{unknown_client_keys, unknown_spec_keys};

const EXAMPLE_SPEC: &str = include_str!("../../../.onlyne.example/spec.toml");
const EXAMPLE_CLIENT: &str =
    include_str!("../../../.onlyne.example/templates/dev/planner/.onlyne/config.toml");

/// A syntactically legal stand-in for each placeholder the example ships.
///
/// `REPLACE_ME` is a marker, not a value: the server names it a bad `cert_pin`
/// before an operator fills it in, which is why every tree copies the file,
/// substitutes, and only then runs `generate`. Filling it here is what the
/// operator's first step does, so parsing the filled text is the contract the
/// dead script meant to check.
fn filled(text: &str) -> String {
    text.replace(
        "sha256/REPLACE_ME",
        "sha256/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .replace(
        "ed25519/REPLACE_ME",
        "ed25519/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
}

#[test]
fn the_example_spec_parses_and_declares_every_key_it_names() {
    Spec::parse_named(&filled(EXAMPLE_SPEC), "spec.toml")
        .expect("the shipped example parses once its placeholders are filled");

    let ignored = unknown_spec_keys(EXAMPLE_SPEC).expect("the example is TOML");
    assert!(
        ignored.is_empty(),
        "the example names keys this build ignores, so every root that copies it keeps a \
         setting that does nothing: {ignored:?}"
    );
}

#[test]
fn the_example_client_fragment_names_no_key_this_build_ignores() {
    // The planner example is a local override fragment, not a whole config: it
    // declares only what needs a local decision and `generate` merges the derived
    // values over it. So the check is the silent-ignore class alone — a key in a
    // template fragment that this build drops reaches every workspace generated
    // from it.
    let ignored = unknown_client_keys(EXAMPLE_CLIENT).expect("the example is TOML");
    assert!(
        ignored.is_empty(),
        "the example client fragment names keys this build ignores: {ignored:?}"
    );
}
