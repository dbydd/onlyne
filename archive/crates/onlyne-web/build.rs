//! The build gate: the binary serves the bundle `rust-embed` embeds, so a
//! build without the bundle fails here rather than serving an empty page.
//!
//! The bundle is made by the Svelte 5 + Vite app in `web/`:
//!
//! ```text
//! cd crates/onlyne-web/web && npm install && npm run build
//! ```
//!
//! which writes `assets/dist/`. That step needs Node; this crate and the core
//! workspace do not — which is why `onlyne-web` is excluded from the workspace
//! and only `cargo build -p onlyne-web` reaches this check.

use std::path::Path;

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("dist");
    let index = dist.join("index.html");
    println!("cargo:rerun-if-changed={}", index.display());
    if !index.is_file() {
        panic!(
            "onlyne-web: the built web bundle is missing — {}\n\
             the binary embeds `assets/dist/`, which the Svelte app in \
             `crates/onlyne-web/web/` produces:\n\
             \x20 cd crates/onlyne-web/web && npm install && npm run build\n\
             that step needs Node; `cargo build` in the repo root never does",
            index.display(),
        );
    }
}
