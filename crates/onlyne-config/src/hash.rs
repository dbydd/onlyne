use sha2::{Digest, Sha256};

/// Lowercase SHA-256 hex over canonical TOML bytes when the input parses.
pub fn spec_hash(bytes: &[u8]) -> String {
    let hash_input = std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.parse::<toml::Value>().ok())
        .map_or_else(|| bytes.to_vec(), |value| canonical_bytes(&value));
    hex_lower(&Sha256::digest(hash_input))
}

/// Lowercase SHA-256 hex over the bytes as they were read.
///
/// Where [`spec_hash`] hashes the canonical form of a parsed document — so a
/// comment, a key's order, or a blank line leaves it alone — this hashes the
/// file's own bytes. It is the concurrency token a `spec_apply` request states
/// as its `base_hash` (`docs/v2-CONTRACT.md` §"Slice 4"): a comment-only edit
/// moves it, and a file rewritten with identical bytes does not.
pub fn source_hash(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

/// Canonical bytes for a parsed spec.
///
/// Tables are sorted by key, arrays keep document order, and every scalar uses
/// a stable textual representation. This supports cache invalidation that is
/// insensitive to TOML key ordering inside equivalent documents.
pub fn canonical_bytes(value: &toml::Value) -> Vec<u8> {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out.into_bytes()
}

fn write_canonical(value: &toml::Value, out: &mut String) {
    match value {
        toml::Value::String(s) => {
            out.push('"');
            out.push_str(&escape(s));
            out.push('"');
        }
        toml::Value::Integer(i) => out.push_str(&i.to_string()),
        toml::Value::Float(float) => out.push_str(&float.to_string()),
        toml::Value::Boolean(flag) => out.push_str(if *flag { "true" } else { "false" }),
        toml::Value::Datetime(datetime) => out.push_str(&datetime.to_string()),
        toml::Value::Array(items) => {
            out.push('[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        toml::Value::Table(table) => {
            out.push('{');
            let mut keys: Vec<&String> = table.keys().collect();
            keys.sort();
            for (idx, key) in keys.into_iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                out.push_str(key);
                out.push(':');
                write_canonical(&table[key], out);
            }
            out.push('}');
        }
    }
}

fn escape(raw: &str) -> String {
    raw.chars().flat_map(char::escape_default).collect()
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
