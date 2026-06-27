use std::{env, fs, path::Path};

fn main() {
    println!("cargo:rerun-if-changed=onlyne-config.schema.json");
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let schema = Path::new(&manifest_dir).join("onlyne-config.schema.json");
    let out = Path::new(&out_dir).join("onlyne-config.schema.json");
    fs::copy(schema, out).expect("copy schema");
}
