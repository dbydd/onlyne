//! Render the machine-readable schemas the front end's TypeScript types come
//! from, into `web/schema/`.
//!
//! The generator is the one the repo already has — `schemars::schema_for!`
//! over `onlyne-proto`'s own types, the same seam
//! `crates/onlyne-proto/src/bin/gen-schema.rs` is (`docs/v2-CONTRACT.md`
//! §"Slice 10"): the request and response types the web speaks are generated
//! from the proto's schema, never hand-written. `npm run gen` in `web/` runs
//! this and then converts the JSON to TypeScript.
//!
//! Building this bin needs the bundle (`build.rs` gates the whole crate), so
//! the schemas in `web/schema/` are committed; regenerate them with
//! `npm run gen` when `onlyne-proto` moves.

use onlyne_proto::view::{Snapshot, View};
use onlyne_web::ops::WebOp;
use onlyne_web::render::{Board, BoardCard};
use schemars::schema_for;
use std::path::Path;

fn main() {
    let schema_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("web")
        .join("schema");
    std::fs::create_dir_all(&schema_dir).expect("create the schema directory");
    write(&schema_dir, "view", &schema_for!(View));
    write(&schema_dir, "snapshot", &schema_for!(Snapshot));
    write(&schema_dir, "board", &schema_for!(Board));
    write(&schema_dir, "board-card", &schema_for!(BoardCard));
    write(&schema_dir, "web-op", &schema_for!(WebOp));
}

fn write(dir: &Path, name: &str, schema: &schemars::schema::RootSchema) {
    let path = dir.join(format!("{name}.schema.json"));
    let json = serde_json::to_string_pretty(schema).expect("schema as json");
    std::fs::write(&path, json).expect("write the schema");
    println!("wrote {}", path.display());
}
