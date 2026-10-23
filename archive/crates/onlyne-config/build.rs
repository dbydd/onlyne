use std::{env, fs, io, path::Path};

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=schema");
    let cargo_out = env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo");
    let cargo_out = Path::new(&cargo_out);
    let schema_dir = Path::new("schema");
    for name in ["spec.schema.json", "config-client.schema.json"] {
        let src = schema_dir.join(name);
        let dst = cargo_out.join(name);
        if src.exists() {
            fs::copy(&src, &dst)?;
        } else {
            fs::write(&dst, b"{}")?;
        }
    }
    Ok(())
}
