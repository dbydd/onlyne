use std::{env, fs, io, path::Path};

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=schema");
    let out_dir = env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo");
    let out_dir = Path::new(&out_dir);
    let schema_dir = Path::new("schema");
    for name in ["spec.schema.json", "config-client.schema.json"] {
        let src = schema_dir.join(name);
        let dst = out_dir.join(name);
        if src.exists() {
            fs::copy(&src, &dst)?;
        } else {
            fs::write(&dst, b"{}")?;
        }
    }
    Ok(())
}
