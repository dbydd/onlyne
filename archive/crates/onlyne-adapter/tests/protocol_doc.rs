//! `PROTOCOL.md` is the document an external TypeScript plugin mirrors, so its
//! examples are a machine-readable surface: every frame it shows must decode
//! through the types a plugin uses, and every envelope it shows must pass the
//! host's own validator.

use onlyne_adapter::WireMessage;
use onlyne_proto::{AdapterMsg, Envelope};
use serde_json::Value;

/// Every JSON object in the frame table, in row order.
fn documented_examples() -> Vec<String> {
    include_str!("../PROTOCOL.md")
        .lines()
        .filter(|line| line.starts_with('|'))
        .filter_map(|line| {
            let start = line.find("`{")? + 1;
            let rest = &line[start..];
            let end = rest.find("}`")? + 1;
            Some(rest[..end].to_string())
        })
        .collect()
}

/// Every envelope nested in one documented frame, in document order.
fn envelopes(value: &Value) -> Vec<Envelope> {
    let mut found = Vec::new();
    let mut walk = vec![value];
    while let Some(node) = walk.pop() {
        match node {
            Value::Object(map) => {
                if map.contains_key("protocol") && map.contains_key("ts") {
                    let envelope = serde_json::from_value::<Envelope>(node.clone())
                        .unwrap_or_else(|err| panic!("{node} is not an envelope: {err}"));
                    found.push(envelope);
                }
                walk.extend(map.values());
            }
            Value::Array(items) => walk.extend(items.iter()),
            _ => {}
        }
    }
    found
}

#[test]
fn every_protocol_example_round_trips_through_the_published_types() {
    let examples = documented_examples();
    assert!(
        examples.len() >= 20,
        "the frame table lost examples: {examples:#?}"
    );
    let mut validated = 0;
    for example in &examples {
        let value: Value = serde_json::from_str(example)
            .unwrap_or_else(|err| panic!("{example} is not JSON: {err}"));
        let wire: WireMessage = serde_json::from_value(value.clone())
            .unwrap_or_else(|err| panic!("{example} does not decode as a wire frame: {err}"));
        match &wire.msg {
            AdapterMsg::Res(body) => assert!(
                body.ok || body.error.is_some(),
                "{example} is a response carrying neither ok nor error"
            ),
            _ => assert!(value.get("op").is_some(), "{example} names no operation"),
        }
        for envelope in envelopes(&value) {
            envelope.validate().unwrap_or_else(|err| {
                panic!("{example} carries an envelope the host rejects: {err}")
            });
            validated += 1;
        }
    }
    assert_eq!(
        validated, 4,
        "the document carries an envelope in send, assign, deliver, and render_send"
    );
}
