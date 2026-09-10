use std::path::Path;

/// Copy exported schemas into OUT_DIR for `include_str!` embedding.
/// Write a titled stub for each root when the schema directory is absent.
fn main() {
    println!("cargo:rerun-if-changed=schema");
    let out = std::env::var("OUT_DIR").expect("OUT_DIR is set for build scripts");
    let out_dir = Path::new(&out);
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set");
    let schema_dir = Path::new(&manifest).join("schema");
    if let Ok(entries) = std::fs::read_dir(&schema_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_schema = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".schema.json"));
            if is_schema {
                if let Some(name) = path.file_name() {
                    let _ = std::fs::copy(&path, out_dir.join(name));
                }
            }
        }
    }
    for (name, title) in [
        ("envelope.schema.json", "Envelope"),
        ("adapter.schema.json", "AdapterMsg"),
    ] {
        let dest = out_dir.join(name);
        if !dest.exists() {
            let stub = format!(
                "{{\"$schema\":\"http://json-schema.org/draft-07/schema#\",\"title\":\"{title}\",\"type\":\"object\"}}"
            );
            let _ = std::fs::write(&dest, stub);
        }
    }
}
