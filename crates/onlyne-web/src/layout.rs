//! The display file: where a board sits on the graph, which is the front
//! end's own fact and never the spec's.
//!
//! The spec holds semantics only (`docs/v2-PLAN.md` line 388); coordinates a
//! user dragged live in `<server-root>/.onlyne/web-layout.json`, discovered
//! from the server's own `spec_get` answer (which names the spec's absolute
//! path), so the file sits beside the spec it draws and survives restarts.

use onlyne_proto::{AdminOp, SpecView};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::admin::exchange;

/// One node's saved place on the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct NodePos {
    pub x: f64,
    pub y: f64,
}

/// The display file's whole content: one place per board.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", default)]
pub struct Layout {
    pub nodes: BTreeMap<String, NodePos>,
}

/// The leaf the layout is stored under, beside `spec.toml`.
pub const LAYOUT_FILE: &str = "web-layout.json";

/// The layout the front end reads and writes, kept in memory with the file
/// beside it when the server's root is known.
#[derive(Debug, Default)]
pub struct LayoutStore {
    inner: Mutex<Layout>,
    /// None until the server answered one `spec_get`, or when an explicit
    /// `--layout` path was not given and the server is down.
    path: Mutex<Option<PathBuf>>,
}

impl LayoutStore {
    /// A store with an explicit path, for `--layout` and for tests.
    pub fn at(path: PathBuf) -> Self {
        let store = LayoutStore::default();
        *store.path.lock().expect("layout path") = Some(path.clone());
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(layout) = serde_json::from_slice::<Layout>(&bytes) {
                *store.inner.lock().expect("layout state") = layout;
            }
        }
        store
    }

    /// The current layout.
    pub fn get(&self) -> Layout {
        self.inner.lock().expect("layout state").clone()
    }

    /// Adopt the server root a `spec_get` answer names, if no explicit path
    /// was given. Called from the op handler whenever the spec is read, so a
    /// layout written before the root was known still finds its file.
    pub fn adopt_server_root(&self, spec_path: &str) {
        // `spec_get` answers the spec's absolute path; its parent's parent is
        // the server root, and `.onlyne/` holds both files.
        let root = Path::new(spec_path)
            .parent()
            .and_then(Path::parent)
            .map(|root| root.join(".onlyne").join(LAYOUT_FILE));
        if let Some(path) = root {
            let mut slot = self.path.lock().expect("layout path");
            if slot.is_none() {
                *slot = Some(path.clone());
                drop(slot);
                if let Ok(bytes) = std::fs::read(&path) {
                    if let Ok(layout) = serde_json::from_slice::<Layout>(&bytes) {
                        *self.inner.lock().expect("layout state") = layout;
                    }
                }
            }
        }
    }

    /// Save a layout the browser dragged into place. The write is atomic, so a
    /// browser reading it back never sees half a file.
    pub fn put(&self, layout: Layout) -> Result<(), String> {
        let path = self.path.lock().expect("layout path").clone();
        if let Some(path) = path {
            let bytes = serde_json::to_vec_pretty(&layout).map_err(|e| e.to_string())?;
            let temp = path.with_extension("json.tmp");
            std::fs::write(&temp, bytes).map_err(|e| e.to_string())?;
            std::fs::rename(&temp, &path).map_err(|e| e.to_string())?;
        }
        *self.inner.lock().expect("layout state") = layout;
        Ok(())
    }
}

/// Read the spec's absolute path once, to adopt the layout file's place.
pub async fn discover_root(
    socket: &Path,
    timeout_ms: u64,
    store: &LayoutStore,
) -> Result<(), String> {
    let data = exchange(
        socket,
        AdminOp::SpecGet(serde_json::Value::Null),
        timeout_ms,
    )
    .await
    .map_err(|error| match error {
        crate::admin::OpError::Refused { code, message } => format!("{code}: {message}"),
        crate::admin::OpError::Transport(why) => why,
    })?;
    let view: SpecView = serde_json::from_value(data).map_err(|e| e.to_string())?;
    store.adopt_server_root(&view.path);
    Ok(())
}
