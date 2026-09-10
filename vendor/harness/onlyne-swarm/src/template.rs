use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Model {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub effort: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetryPolicy {
    #[serde(default)]
    pub max_attempts: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceTemplate {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub model: Model,
    #[serde(default)]
    pub back_edges: Vec<String>,
    #[serde(default)]
    pub retry: Option<RetryPolicy>,
}

/// Effective (merged) description of one workspace, keyed by tree path ("" = root).
#[derive(Debug, Clone)]
pub struct Effective {
    pub path: String,
    pub name: String,
    pub role: String,
    pub model: Model,
    /// Normalized tree-absolute back-edge targets.
    pub back_edges: Vec<String>,
}

fn parse_jsonc(path: &Path) -> anyhow::Result<WorkspaceTemplate> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    json5::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

/// Parse a JSONC template document held in memory (root config overlays).
/// Same parser as the file path, so comment style stays identical.
pub fn parse_lenient(text: &str) -> anyhow::Result<serde_json::Value> {
    let v: serde_json::Value = json5::from_str(text)?;
    Ok(v)
}

fn load_opt(path: &Path) -> anyhow::Result<Option<WorkspaceTemplate>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(parse_jsonc(path)?))
}

/// Normalize a back-edge target against the declaring workspace's tree path.
///
/// Two accepted forms:
/// - relative (may contain `..`): resolved against `from_path`;
/// - tree-absolute (no leading `..`, e.g. sync snapshots): returned as-is.
/// A hand-written overlay that repeats the snapshot's absolute form therefore
/// round-trips instead of doubling the prefix (`b` + `a` != `b/a`).
pub fn normalize_edge(from_path: &str, raw: &str) -> String {
    if raw.is_empty() || raw == "." {
        return String::new();
    }
    let is_absolute = !raw.split('/').any(|s| s == "..");
    if is_absolute {
        return raw
            .split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .collect::<Vec<_>>()
            .join("/");
    }
    let mut parts: Vec<&str> = if from_path.is_empty() {
        vec![]
    } else {
        from_path.split('/').collect()
    };
    for seg in raw.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn merge_into(acc: &mut WorkspaceTemplate, over: &WorkspaceTemplate, dir_name: &str) {
    if !over.name.is_empty() {
        acc.name = over.name.clone();
    } else if acc.name.is_empty() {
        acc.name = dir_name.to_string();
    }
    if !over.role.is_empty() {
        acc.role = over.role.clone();
    }
    if !over.model.provider.is_empty() {
        acc.model.provider = over.model.provider.clone();
    }
    if !over.model.model.is_empty() {
        acc.model.model = over.model.model.clone();
    }
    if !over.model.effort.is_empty() {
        acc.model.effort = over.model.effort.clone();
    }
    // back_edges are normalized by the caller before merging as a set union.
}

/// Walk `.agents/.schedule` and produce the effective description per workspace.
/// Returns (ordered paths, map). Root template comes from `.onlyne/swarm.workspace.jsonc`.
/// `$schema` is an editor hint only and never participates in merge output.
pub fn load_tree(root: &Path) -> anyhow::Result<Vec<Effective>> {
    let sched = crate::root::schedule_dir(root);
    let mut out: Vec<Effective> = vec![];
    let mut acc = WorkspaceTemplate::default();
    if let Some(t) = load_opt(&crate::root::swarm_ws_config(root))? {
        merge_into(&mut acc, &t, ".");
    }
    if acc.name.is_empty() {
        acc.name = ".".into();
    }
    out.push(Effective {
        path: String::new(),
        name: acc.name.clone(),
        role: acc.role.clone(),
        model: acc.model.clone(),
        back_edges: vec![],
    });

    if sched.is_dir() {
        let mut dirs: Vec<PathBuf> = vec![];
        collect_dirs(&sched, &mut dirs)?;
        dirs.sort();
        for d in dirs {
            let rel = d
                .strip_prefix(&sched)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let segs: Vec<&str> = rel.split('/').collect();
            // Merge layer by layer: each ancestor's template, then this layer's.
            let mut layer_acc = acc.clone();
            let mut prefix = PathBuf::from(&sched);
            let mut edge_set: BTreeSet<String> = BTreeSet::new();
            // Root-level back_edges normalized from "" path.
            for e in root_back_edges(root)? {
                edge_set.insert(e);
            }
            let mut cur_path = String::new();
            for (i, seg) in segs.iter().enumerate() {
                prefix = prefix.join(seg);
                cur_path = if i == 0 {
                    seg.to_string()
                } else {
                    format!("{cur_path}/{seg}")
                };
                // Only the leaf layer's own template contributes back_edges
                // (normalized against the full workspace path). Ancestor layers
                // contribute scalar fields via merge only.
                if let Some(t) = load_opt(&prefix.join("template.workspace.jsonc"))? {
                    if i == segs.len() - 1 {
                        for e in &t.back_edges {
                            edge_set.insert(normalize_edge(&cur_path, e));
                        }
                    }
                    merge_into(&mut layer_acc, &t, seg);
                } else if layer_acc.name.is_empty() {
                    layer_acc.name = (*seg).to_string();
                }
            }
            // Instance hand-written overlay. Snapshot files written by `sync`
            // store back_edges already normalized (tree-absolute); hand-written
            // files may use relative form. normalize_edge() is idempotent on
            // already-absolute paths (no leading ..), so both forms work.
            let inst_overlay =
                crate::root::resolve_instance(root, &rel).join(".onlyne/swarm.workspace.jsonc");
            if let Some(t) = load_opt(&inst_overlay)? {
                for e in &t.back_edges {
                    edge_set.insert(normalize_edge(&rel, e));
                }
                let leaf = segs.last().copied().unwrap_or("");
                merge_into(&mut layer_acc, &t, leaf);
            }
            // Ancestor layers contribute scalar fields only (merged into layer_acc
            // above). back_edges are per-workspace: no inheritance from ancestors.
            out.push(Effective {
                path: rel,
                name: layer_acc.name.clone(),
                role: layer_acc.role.clone(),
                model: layer_acc.model.clone(),
                back_edges: edge_set.into_iter().collect(),
            });
        }
    }
    validate_edges(&out)?;
    Ok(out)
}

fn root_back_edges(root: &Path) -> anyhow::Result<Vec<String>> {
    let t = load_opt(&crate::root::swarm_ws_config(root))?;
    Ok(
        t.map(|t| t.back_edges.iter().map(|e| normalize_edge("", e)).collect())
            .unwrap_or_default(),
    )
}

fn collect_dirs(dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for e in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let e = e?;
        if e.file_type()?.is_dir() {
            // Dot-dirs (`.pi/`, `.git/`, `.DS_Store/` etc.) are tooling
            // metadata, never workspace layers. Without this a per-role
            // `.pi/settings.json` materializes a phantom `<role>/.pi`
            // instance that legacy `onlyne_in` diagnostics could misread
            // against the `onlyne_in/<role>` symlink file (observed 2026-09-08).
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            out.push(e.path());
            collect_dirs(&e.path(), out)?;
        }
    }
    Ok(())
}

fn validate_edges(tree: &[Effective]) -> anyhow::Result<()> {
    let known: BTreeSet<&str> = tree.iter().map(|e| e.path.as_str()).collect();
    for e in tree {
        for t in &e.back_edges {
            if !known.contains(t.as_str()) {
                anyhow::bail!(
                    "back_edge target missing: workspace '{}' -> '{}'",
                    if e.path.is_empty() { "." } else { &e.path },
                    t
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalize_relative_edges() {
        assert_eq!(normalize_edge("a/b", "../reviewer"), "a/reviewer");
        assert_eq!(normalize_edge("a", "../../x/y"), "x/y");
        assert_eq!(normalize_edge("", "a/b"), "a/b");
        assert_eq!(normalize_edge("a/b", "."), "");
        // Tree-absolute form (sync snapshots) round-trips.
        assert_eq!(normalize_edge("b", "a"), "a");
        assert_eq!(normalize_edge("b", "b/a"), "b/a");
    }

    #[test]
    fn back_edges_are_per_workspace_no_inherit() {
        // regression: b declares ../a; nested b/c must NOT inherit it as b/a.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".agents/.schedule/b/c")).unwrap();
        std::fs::write(
            root.join(".agents/.schedule/b/template.workspace.jsonc"),
            r#"{"back_edges": ["../a"]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".agents/.schedule/a")).unwrap();
        std::fs::write(
            root.join(".agents/.schedule/b/c/template.workspace.jsonc"),
            r#"{}"#,
        )
        .unwrap();
        let tree = load_tree(root).unwrap();
        let b = tree.iter().find(|e| e.path == "b").unwrap();
        assert_eq!(b.back_edges, vec!["a"]);
        let c = tree.iter().find(|e| e.path == "b/c").unwrap();
        assert!(c.back_edges.is_empty(), "got {:?}", c.back_edges);
    }

    #[test]
    fn deep_merge_priority() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".agents/.schedule/a")).unwrap();
        std::fs::write(
            root.join(".agents/.schedule/a/template.workspace.jsonc"),
            r#"{"role": "upper", "model": {"provider": "p", "model": "m", "effort": "low"}, "back_edges": ["../r"]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".agents/.schedule/r")).unwrap();
        std::fs::write(
            root.join(".agents/.schedule/r/template.workspace.jsonc"),
            r#"{"role": "r"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".ws/a/.onlyne")).unwrap();
        std::fs::write(
            root.join(".ws/a/.onlyne/swarm.workspace.jsonc"),
            r#"{"role": "hand"}"#,
        )
        .unwrap();
        let tree = load_tree(root).unwrap();
        let a = tree.iter().find(|e| e.path == "a").unwrap();
        assert_eq!(a.role, "hand");
        assert_eq!(a.model.model, "m");
        assert_eq!(a.back_edges, vec!["r"]);
    }

    #[test]
    fn marquee_declares_e_to_a_return_edge() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/marquee");
        let tree = load_tree(&root).unwrap();
        let e = tree.iter().find(|entry| entry.path == "e").unwrap();
        assert_eq!(e.back_edges, vec!["a"]);
    }

    #[test]
    fn dot_dirs_are_not_workspace_layers() {
        // regression (2026-09-08): a per-role `.pi/settings.json` made
        // collect_dirs treat `.pi` as a `<role>/.pi` workspace; run_sync
        // then materialized phantom instances and legacy `onlyne_in`
        // diagnostics could misread the `onlyne_in/<role>` symlink file.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".pi")).unwrap();
        std::fs::write(
            root.join(".pi/settings.json"),
            r#"{"packages":["./harness/pi-onlyne"]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("harness/pi-onlyne")).unwrap();
        std::fs::write(
            root.join("harness/pi-onlyne/package.json"),
            r#"{"name":"pi-onlyne"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".agents/.schedule/a/.pi")).unwrap();
        std::fs::write(
            root.join(".agents/.schedule/a/template.workspace.jsonc"),
            r#"{"name": "a", "role": "r"}"#,
        )
        .unwrap();
        std::fs::write(root.join(".agents/.schedule/a/.pi/settings.json"), "{}").unwrap();
        let tree = load_tree(root).unwrap();
        assert!(
            tree.iter().all(|e| e.path != "a/.pi"),
            "got {:?}",
            tree.iter().map(|e| &e.path).collect::<Vec<_>>()
        );
        let rep = crate::sync::run_sync(root).unwrap();
        assert!(!rep.created.iter().any(|c| c == "a/.pi"));
        assert!(!root.join(".ws/a/.pi/.onlyne").exists());
    }

    #[test]
    fn missing_edge_target_fails() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".agents/.schedule/a")).unwrap();
        std::fs::write(
            root.join(".agents/.schedule/a/template.workspace.jsonc"),
            r#"{"back_edges": ["ghost"]}"#,
        )
        .unwrap();
        assert!(load_tree(root).is_err());
    }
}
