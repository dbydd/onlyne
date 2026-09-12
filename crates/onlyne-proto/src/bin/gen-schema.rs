use onlyne_proto::{AdapterMsg, Envelope};
use schemars::schema_for;
use std::path::Path;

/// Render the machine-readable protocol schemas into `schema/`.
fn main() {
    let schema_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema");
    std::fs::create_dir_all(&schema_dir).expect("create schema directory");
    let envelope = schema_for!(Envelope);
    let envelope_json = serde_json::to_string_pretty(&envelope).expect("envelope schema as json");
    let envelope_path = schema_dir.join("envelope.schema.json");
    std::fs::write(&envelope_path, &envelope_json).expect("write envelope schema");
    println!("wrote {}", envelope_path.display());
    let adapter = schema_for!(AdapterMsg);
    let adapter_json = serde_json::to_string_pretty(&adapter).expect("adapter schema as json");
    let adapter_path = schema_dir.join("adapter.schema.json");
    std::fs::write(&adapter_path, &adapter_json).expect("write adapter schema");
    println!("wrote {}", adapter_path.display());
}
