use onlyne_config::{ClientConfig, Spec};
use schemars::schema_for;
use std::{fs, io, path::Path};

fn main() -> io::Result<()> {
    let schema_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema");
    fs::create_dir_all(&schema_dir)?;

    let spec_schema = serde_json::to_string_pretty(&schema_for!(Spec))?;
    fs::write(schema_dir.join("spec.schema.json"), spec_schema)?;

    let client_schema = serde_json::to_string_pretty(&schema_for!(ClientConfig))?;
    fs::write(schema_dir.join("config-client.schema.json"), client_schema)?;
    Ok(())
}
