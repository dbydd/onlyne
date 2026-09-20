//! The keys a config file carries that its generated schema does not name.
//!
//! Parsing is lenient on purpose: serde drops a key no field declares, so a file
//! written for another build still starts the process. Dropping silently is the
//! half this module covers. It reads the same JSON Schema the `config-schema`
//! binary writes — `schemars` generates it from these structs, so a field added to
//! a struct is a key this walker knows about, with no list to maintain — and
//! answers which paths in a document the schema leaves unnamed.
//!
//! The loaders call [`unknown_spec_keys`] and [`unknown_client_keys`] to put one
//! warning per ignored key in the log, and the test suite calls them to hold its
//! own example files to the schema.

use serde_json::{Map, Value as Json};
use std::sync::LazyLock;

/// The generated schema for `spec.toml`, parsed once.
///
/// [`crate::spec_schema`] is the `build.rs` copy of the file the `config-schema`
/// binary writes, so the embedded text and the structs move together.
static SPEC_SCHEMA: LazyLock<Json> = LazyLock::new(|| {
    serde_json::from_str(crate::spec_schema()).expect("generated spec schema is valid JSON")
});

/// The generated schema for `<workspace>/.onlyne/config.toml`, parsed once.
static CLIENT_SCHEMA: LazyLock<Json> = LazyLock::new(|| {
    serde_json::from_str(crate::config_client_schema())
        .expect("generated client schema is valid JSON")
});

/// JSON Pointer for a local `$ref` (`#/definitions/Timeouts`).
fn ref_pointer(reference: &str) -> Option<String> {
    let tail = reference.strip_prefix("#/")?;
    let segments: Vec<&str> = tail.split('/').collect();
    Some(format!("/{}", segments.join("/")))
}

/// Follow `$ref` chains until the node itself carries the schema.
fn resolve<'a>(root: &'a Json, node: &'a Json) -> &'a Json {
    let mut current = node;
    for _ in 0..16 {
        let Some(target) = current
            .get("$ref")
            .and_then(Json::as_str)
            .and_then(ref_pointer)
        else {
            return current;
        };
        match root.pointer(&target) {
            Some(next) => current = next,
            None => return current,
        }
    }
    current
}

/// Every key name this node accepts in a table, unioned across its arms.
///
/// `schemars` puts a field's own notes beside a reference as `allOf`, and an
/// `Option<struct>` field generates one `anyOf` arm per alternative. Both shapes
/// have to be read through, or a real key reads as ignored.
fn accepted(root: &Json, node: &Json, found: &mut Map<String, Json>) {
    if let Some(props) = node.get("properties").and_then(Json::as_object) {
        for (key, schema) in props {
            found.insert(key.clone(), schema.clone());
        }
    }
    for arm in ["allOf", "anyOf", "oneOf"] {
        let Some(arms) = node.get(arm).and_then(Json::as_array) else {
            continue;
        };
        for entry in arms {
            accepted(root, resolve(root, entry), found);
        }
    }
}

/// Join one path segment onto a dotted config path.
///
/// Array elements attach (`client[1].max_sessions`), table keys take a dot.
fn join(base: &str, segment: &str) -> String {
    if base.is_empty() {
        return segment.to_string();
    }
    if segment.starts_with('[') {
        return format!("{base}{segment}");
    }
    format!("{base}.{segment}")
}

/// A table's accepted key names plus how it treats the rest.
///
/// `extra` is a map field's value schema, `open` means the schema allows any
/// key with no value rule, and neither case yields an ignored-key report.
struct TableRule {
    props: Map<String, Json>,
    extra: Option<Json>,
    open: bool,
}

fn table_rule(root: &Json, node: &Json) -> TableRule {
    let mut props = Map::new();
    accepted(root, node, &mut props);
    let extra = node.get("additionalProperties");
    TableRule {
        props,
        extra: extra.filter(|schema| schema.as_bool().is_none()).cloned(),
        open: matches!(extra, Some(Json::Bool(true))),
    }
}

fn walk(root: &Json, node: &Json, value: &toml::Value, path: &str, out: &mut Vec<String>) {
    let node = resolve(root, node);
    match value {
        toml::Value::Table(entries) => {
            let rule = table_rule(root, node);
            for (key, child) in entries {
                if let Some(schema) = rule.props.get(key) {
                    walk(root, schema, child, &join(path, key), out);
                    continue;
                }
                if let Some(schema) = &rule.extra {
                    // A map field: its keys are operator data, and the value
                    // schema is the only place they can be checked.
                    walk(root, schema, child, &join(path, key), out);
                    continue;
                }
                if rule.open {
                    continue;
                }
                out.push(join(path, key));
            }
        }
        toml::Value::Array(items) => {
            let Some(schema) = node.get("items") else {
                return;
            };
            for (index, item) in items.iter().enumerate() {
                walk(root, schema, item, &join(path, &format!("[{index}]")), out);
            }
        }
        _ => {}
    }
}

/// Every config path in `instance` that `schema` does not name.
///
/// `schema` is the generated JSON Schema document for the type `instance` was
/// parsed into. Paths come back in document order, dotted, with array indices
/// attached: `client[1].reusable`.
pub fn unknown_paths(schema: &Json, instance: &toml::Value) -> Vec<String> {
    let root = schema;
    let mut out = Vec::new();
    walk(root, resolve(root, root), instance, "", &mut out);
    out
}

/// One warning per key a loader ignored.
///
/// The parse succeeded, so the process starts. The line exists because a key
/// spelled wrong falls back to its default value, and a default that arrives
/// through a typo looks like a config that took effect.
pub(crate) fn warn_ignored(paths: &[String], file: &str) {
    for path in paths {
        tracing::warn!("{file}: ignoring unknown key `{path}`");
    }
}

/// The paths one parsed spec document carries beyond `Spec`'s own fields.
pub(crate) fn spec_unknown(value: &toml::Value) -> Vec<String> {
    unknown_paths(&SPEC_SCHEMA, value)
}

/// The paths one parsed client config document carries beyond `ClientConfig`'s own
/// fields.
pub(crate) fn client_unknown(value: &toml::Value) -> Vec<String> {
    unknown_paths(&CLIENT_SCHEMA, value)
}

/// The keys one `spec.toml` text carries that `Spec` does not declare.
///
/// The `relay_required_count` alias is folded onto `relay_count` first, the same
/// rewrite the loader applies, so an accepted alias never reads as ignored.
/// `Err` carries the TOML message a file that does not parse would produce.
pub fn unknown_spec_keys(text: &str) -> Result<Vec<String>, String> {
    let mut value: toml::Value = text
        .parse()
        .map_err(|error: toml::de::Error| error.message().to_string())?;
    crate::spec::rewrite_client_relay_count_alias(&mut value);
    Ok(spec_unknown(&value))
}

/// The keys one client `config.toml` text carries that `ClientConfig` declares
/// nothing for.
pub fn unknown_client_keys(text: &str) -> Result<Vec<String>, String> {
    let value: toml::Value = text
        .parse()
        .map_err(|error: toml::de::Error| error.message().to_string())?;
    Ok(client_unknown(&value))
}
