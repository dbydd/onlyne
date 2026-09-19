//! Machine-readable wire vectors (decision D18).
//!
//! Every `tests/wire_vectors/*.json` file holds `frame` (the exact UTF-8 JSON
//! bytes the daemon writes, declaration order preserved), `encoding`, and
//! `note`. This test decodes each `frame` into its published type, re-encodes
//! it, and requires the bytes back, so a type change that moves the wire breaks
//! here first.

use onlyne_proto::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const ENCODING: &str = "u32_be_length_prefix_plus_utf8_json";

fn vector_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/wire_vectors")
}

struct Vector {
    name: String,
    frame: String,
    encoding: String,
    note: String,
}

fn load_vectors() -> Vec<Vector> {
    let dir = vector_dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("vector name")
                .to_string();
            let text = std::fs::read_to_string(&path).expect("read vector");
            let value: Value =
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: not json: {e}"));
            let object = value.as_object().expect("vector is an object");
            assert_eq!(object.len(), 3, "{name}: vectors hold exactly three keys");
            Vector {
                frame: object["frame"]
                    .as_str()
                    .expect("frame is a string")
                    .to_string(),
                encoding: object["encoding"]
                    .as_str()
                    .expect("encoding is a string")
                    .to_string(),
                note: object["note"]
                    .as_str()
                    .expect("note is a string")
                    .to_string(),
                name,
            }
        })
        .collect()
}

/// Decode into `T`, re-encode, and require the original bytes back.
fn round_trip<T: DeserializeOwned + Serialize>(name: &str, raw: &str) -> Value {
    let typed: T = serde_json::from_str(raw)
        .unwrap_or_else(|e| panic!("{name}: does not decode into its published type: {e}"));
    let again = serde_json::to_string(&typed).expect("re-encode");
    assert_eq!(
        again, raw,
        "{name}: re-encoded bytes differ from the vector, so the wire moved"
    );
    serde_json::from_str(&again).expect("re-encoded vector parses")
}

fn note_bool(note: &str, key: &str) -> bool {
    let needle = format!("{key}=");
    let at = note
        .find(&needle)
        .unwrap_or_else(|| panic!("note carries no {key}: {note}"));
    note[at + needle.len()..].starts_with("true")
}

/// The family a vector file belongs to, taken from its name prefix.
fn vector_family(name: &str) -> &str {
    for family in [
        "req_client",
        "req_gateway",
        "req_admin",
        "res",
        "ev",
        "frame",
        "error",
        "adapter",
    ] {
        if name
            .strip_prefix(family)
            .is_some_and(|rest| rest.starts_with('_'))
        {
            return family;
        }
    }
    panic!("{name}: vector does not belong to a known family");
}
#[test]
fn every_vector_matches_its_published_type() {
    let vectors = load_vectors();
    let mut family_counts: BTreeMap<String, usize> = BTreeMap::new();

    for vector in &vectors {
        let Vector {
            name,
            frame,
            encoding,
            note,
        } = vector;
        assert_eq!(encoding, ENCODING, "{name}: unexpected encoding");
        assert!(
            !note.trim().is_empty(),
            "{name}: a vector states the rule it pins"
        );

        *family_counts
            .entry(vector_family(name).to_string())
            .or_insert(0usize) += 1;

        if let Some(op) = name.strip_prefix("req_client_") {
            let value = round_trip::<Frame<ClientOp>>(name, frame);
            assert_eq!(value["f"], "req", "{name}");
            assert_eq!(value["op"], op, "{name}: op name follows the file name");
        } else if let Some(op) = name.strip_prefix("req_gateway_") {
            let value = round_trip::<GatewayFrame>(name, frame);
            assert_eq!(value["f"], "req", "{name}");
            assert_eq!(value["op"], op, "{name}: op name follows the file name");
        } else if let Some(op) = name.strip_prefix("req_admin_") {
            let value = round_trip::<AdminFrame>(name, frame);
            assert_eq!(value["f"], "req", "{name}");
            assert_eq!(value["op"], op, "{name}: op name follows the file name");
        } else if let Some(kind) = name.strip_prefix("frame_") {
            let value = round_trip::<Frame>(name, frame);
            assert_eq!(value["f"], kind, "{name}: frame discriminant");
        } else if let Some(kind) = name.strip_prefix("ev_") {
            let value = round_trip::<Frame>(name, frame);
            assert_eq!(value["f"], "ev", "{name}");
            assert_eq!(
                value["type"], kind,
                "{name}: event type follows the file name"
            );
        } else if name.starts_with("res_") {
            let value = round_trip::<Frame>(name, frame);
            assert_eq!(value["f"], "res", "{name}");
            assert!(value["ok"].is_boolean(), "{name}: a response states ok");
            if value["ok"] == Value::Bool(true) {
                assert!(value.get("data").is_some(), "{name}: success carries data");
                assert!(value.get("error").is_none(), "{name}: success omits error");
            } else {
                assert!(
                    value.get("error").is_some(),
                    "{name}: failure names an error"
                );
            }
        } else if let Some(code) = name.strip_prefix("error_") {
            let value = round_trip::<Frame>(name, frame);
            assert_eq!(value["f"], "res", "{name}");
            assert_eq!(value["ok"], Value::Bool(false), "{name}");
            assert_eq!(
                value["error"]["code"], code,
                "{name}: code spelling follows the file name"
            );
            assert!(
                value.get("data").is_none(),
                "{name}: a plain rejection carries no data"
            );
        } else if name.starts_with("adapter_") {
            let value = round_trip::<AdapterMsg>(name, frame);
            assert!(
                value["op"].is_string(),
                "{name}: adapter messages name their op"
            );
        } else {
            panic!("{name}: vector does not belong to a known family");
        }
    }

    let expected = [
        ("req_client", 13usize),
        ("req_gateway", 5),
        ("req_admin", 19),
        ("res", 12),
        ("ev", 6),
        ("frame", 4),
        ("error", 14),
        ("adapter", 11),
    ];
    for (family, count) in expected {
        assert_eq!(
            family_counts.get(family).copied().unwrap_or(0),
            count,
            "{family}: vector count"
        );
    }
    assert_eq!(vectors.len(), 84, "total vector count");
}

#[test]
fn frame_bytes_carry_the_documented_length_prefix() {
    let vectors = load_vectors();
    let mut pinned = 0usize;
    for vector in &vectors {
        let Some(at) = vector.note.find("length prefix 0x") else {
            continue;
        };
        let hex = &vector.note[at + "length prefix 0x".len()..];
        let hex: String = hex.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        let declared = u32::from_str_radix(&hex, 16).expect("prefix is hex");
        assert_eq!(
            declared,
            vector.frame.len() as u32,
            "{}: the announced big-endian u32 prefix must equal the frame byte count",
            vector.name
        );
        assert_eq!(
            vector.frame.len(),
            vector.frame.chars().map(char::len_utf8).sum::<usize>(),
            "{}: the wire length counts UTF-8 bytes",
            vector.name
        );
        pinned += 1;
    }
    assert_eq!(
        pinned, 1,
        "one representative vector pins the length prefix"
    );
}

#[test]
fn the_vectors_are_the_source_of_truth_for_retryable_codes() {
    let vectors = load_vectors();
    let mut retryable = BTreeSet::new();
    let mut permanent = BTreeSet::new();
    for vector in &vectors {
        let Some(code) = vector.name.strip_prefix("error_") else {
            continue;
        };
        if note_bool(&vector.note, "retryable") {
            retryable.insert(code.to_string());
        } else {
            permanent.insert(code.to_string());
        }
    }
    assert_eq!(
        retryable.len(),
        4,
        "exactly four codes retry: {retryable:?}"
    );
    assert_eq!(
        retryable,
        ["duplicate", "internal", "recipient_offline", "unauthorized"]
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>(),
        "the vector notes name the retryable set"
    );
    assert_eq!(
        retryable.len() + permanent.len(),
        14,
        "all codes are covered"
    );
}

/// A minimal validator for the subset `schemars` emits. Any keyword outside
/// that subset is a hard failure, so this can never silently pass.
struct SchemaValidator<'a> {
    root: &'a Value,
}

impl<'a> SchemaValidator<'a> {
    fn new(root: &'a Value) -> Self {
        SchemaValidator { root }
    }

    fn resolve(&self, reference: &str) -> &'a Value {
        let pointer = reference
            .strip_prefix('#')
            .unwrap_or_else(|| panic!("only local refs are supported: {reference}"));
        self.root
            .pointer(pointer)
            .unwrap_or_else(|| panic!("unresolvable ref {reference}"))
    }

    fn check(&self, schema: &Value, instance: &Value, path: &str) -> Result<(), String> {
        let object = match schema {
            Value::Object(object) => object,
            Value::Bool(true) => return Ok(()),
            Value::Bool(false) => return Err(format!("{path}: schema forbids every value")),
            other => return Err(format!("{path}: malformed schema node {other}")),
        };
        for (keyword, spec) in object {
            match keyword.as_str() {
                "$schema" | "title" | "description" | "default" | "definitions" => {}
                "$ref" => {
                    let reference = spec
                        .as_str()
                        .ok_or_else(|| format!("{path}: $ref is not a string"))?;
                    self.check(self.resolve(reference), instance, path)?;
                }
                "type" => self.check_type(spec, instance, path)?,
                "enum" => {
                    let options = spec
                        .as_array()
                        .ok_or_else(|| format!("{path}: enum is not an array"))?;
                    if !options.contains(instance) {
                        return Err(format!("{path}: {instance} is outside {options:?}"));
                    }
                }
                "anyOf" => {
                    let branches = spec
                        .as_array()
                        .ok_or_else(|| format!("{path}: anyOf is not an array"))?;
                    if !branches
                        .iter()
                        .any(|branch| self.check(branch, instance, path).is_ok())
                    {
                        return Err(format!("{path}: no anyOf branch matches {instance}"));
                    }
                }
                "oneOf" => {
                    let branches = spec
                        .as_array()
                        .ok_or_else(|| format!("{path}: oneOf is not an array"))?;
                    let matched = branches
                        .iter()
                        .filter(|branch| self.check(branch, instance, path).is_ok())
                        .count();
                    if matched != 1 {
                        return Err(format!(
                            "{path}: {matched} oneOf branches match {instance}, expected exactly one"
                        ));
                    }
                }
                "allOf" => {
                    let branches = spec
                        .as_array()
                        .ok_or_else(|| format!("{path}: allOf is not an array"))?;
                    for branch in branches {
                        self.check(branch, instance, path)?;
                    }
                }
                "required" => {
                    let keys = spec
                        .as_array()
                        .ok_or_else(|| format!("{path}: required is not an array"))?;
                    for key in keys {
                        let key = key
                            .as_str()
                            .ok_or_else(|| format!("{path}: required key is not a string"))?;
                        if instance.get(key).is_none() {
                            return Err(format!("{path}: missing required key {key}"));
                        }
                    }
                }
                "properties" => {
                    let properties = spec
                        .as_object()
                        .ok_or_else(|| format!("{path}: properties is not an object"))?;
                    for (key, subschema) in properties {
                        if let Some(found) = instance.get(key) {
                            self.check(subschema, found, &format!("{path}.{key}"))?;
                        }
                    }
                }
                "additionalProperties" => {
                    if spec == &Value::Bool(false) {
                        let properties = object.get("properties").and_then(|p| p.as_object());
                        for key in instance
                            .as_object()
                            .ok_or_else(|| format!("{path}: not an object"))?
                            .keys()
                        {
                            let known = properties.is_some_and(|p| p.contains_key(key));
                            if !known {
                                return Err(format!("{path}: unexpected key {key}"));
                            }
                        }
                    }
                }
                "items" => {
                    let items = instance
                        .as_array()
                        .ok_or_else(|| format!("{path}: items apply to an array"))?;
                    for (index, item) in items.iter().enumerate() {
                        self.check(spec, item, &format!("{path}[{index}]"))?;
                    }
                }
                "minimum" => {
                    let floor = spec
                        .as_f64()
                        .ok_or_else(|| format!("{path}: minimum is not a number"))?;
                    let found = instance
                        .as_f64()
                        .ok_or_else(|| format!("{path}: minimum applies to a number"))?;
                    if found < floor {
                        return Err(format!("{path}: {found} is below the minimum {floor}"));
                    }
                }
                "format" => {
                    if spec == "date-time" {
                        let text = instance
                            .as_str()
                            .ok_or_else(|| format!("{path}: date-time applies to a string"))?;
                        if chrono::DateTime::parse_from_rfc3339(text).is_err() {
                            return Err(format!("{path}: {text} is not a date-time"));
                        }
                    }
                }
                other => {
                    return Err(format!(
                        "{path}: this validator does not implement the schema keyword {other}"
                    ));
                }
            }
        }
        Ok(())
    }

    fn check_type(&self, spec: &Value, instance: &Value, path: &str) -> Result<(), String> {
        let names: Vec<&str> = match spec {
            Value::String(name) => vec![name.as_str()],
            Value::Array(names) => names.iter().filter_map(|n| n.as_str()).collect(),
            other => return Err(format!("{path}: malformed type {other}")),
        };
        let actual = match instance {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(number) if number.is_f64() => "number",
            Value::Number(_) => "integer",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        let matched = names
            .iter()
            .any(|name| *name == actual || (*name == "number" && actual == "integer"));
        if matched {
            Ok(())
        } else {
            Err(format!("{path}: {actual} is not any of {names:?}"))
        }
    }
}

/// Validate every nested envelope in `value`, returning how many were checked.
fn validate_envelopes(
    validator: &SchemaValidator<'_>,
    value: &Value,
    path: &str,
    found: &mut usize,
) {
    match value {
        Value::Object(object) => {
            let looks_like_envelope =
                object.contains_key("protocol") && object.contains_key("body");
            if looks_like_envelope {
                validator
                    .check(validator.root, value, path)
                    .unwrap_or_else(|message| panic!("envelope schema mismatch at {message}"));
                *found += 1;
            }
            for (key, child) in object {
                validate_envelopes(validator, child, &format!("{path}.{key}"), found);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                validate_envelopes(validator, item, &format!("{path}[{index}]"), found);
            }
        }
        _ => {}
    }
}

#[test]
fn every_vector_validates_against_the_embedded_schemas() {
    let envelope_schema: Value =
        serde_json::from_str(envelope_schema()).expect("envelope schema parses");
    let adapter_schema: Value =
        serde_json::from_str(adapter_schema()).expect("adapter schema parses");
    let envelope_validator = SchemaValidator::new(&envelope_schema);
    let adapter_validator = SchemaValidator::new(&adapter_schema);

    let mut carriers = BTreeSet::new();
    let mut adapter_messages = 0usize;
    for vector in load_vectors() {
        let value: Value = serde_json::from_str(&vector.frame).expect("vector frame parses");
        let mut found = 0usize;
        validate_envelopes(&envelope_validator, &value, "$", &mut found);
        if found > 0 {
            carriers.insert(vector.name.clone());
        }
        if vector.name.starts_with("adapter_") {
            adapter_validator
                .check(&adapter_schema, &value, "$")
                .unwrap_or_else(|message| {
                    panic!("{}: adapter schema mismatch at {message}", vector.name)
                });
            adapter_messages += 1;
        }
    }
    assert_eq!(
        carriers,
        [
            "adapter_host_assign",
            "adapter_host_render_send",
            "adapter_plugin_send",
            "adapter_plugin_send_cluster",
            "adapter_plugin_send_completion",
            "adapter_plugin_send_downstream",
            "adapter_plugin_send_note",
            "req_admin_send",
            "req_client_send",
            "req_gateway_deliver",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect::<BTreeSet<_>>(),
        "every vector carrying an Envelope is schema-checked"
    );
    assert_eq!(
        adapter_messages, 11,
        "every adapter vector is schema-checked"
    );
}

#[test]
fn protocol_text_vectors_match_the_crate_constants() {
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/text_vectors.json"),
    )
    .expect("text vectors");
    let text: Value = serde_json::from_str(&text).expect("text vectors parse");
    let string = |key: &str| {
        text[key]
            .as_str()
            .expect("text vector is a string")
            .to_string()
    };

    assert_eq!(string("op_id_conflict"), OP_ID_CONFLICT_MESSAGE);
    assert_eq!(string("no_socket"), text::NO_SOCKET_MESSAGE);
    assert_eq!(string("legacy_workspace"), text::LEGACY_WORKSPACE_MESSAGE);
    assert_eq!(
        string("unsupported_schema"),
        text::UNSUPPORTED_SCHEMA_MESSAGE
    );
    assert_eq!(
        string("binary_not_found_example"),
        text::binary_not_found("onlyne-server")
    );
    assert_eq!(
        string("binary_not_found_template"),
        format!("{}{}", text::BINARY_NOT_FOUND_PREFIX, "<name>")
    );
    assert_eq!(
        OP_ID_CONFLICT_MESSAGE,
        "op_id conflict: request differs from durable receipt"
    );
    assert_eq!(
        string("causality_required_control"),
        text::causality_required("control")
    );
}
