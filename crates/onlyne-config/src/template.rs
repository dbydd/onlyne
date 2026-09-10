//! Template discovery and generation primitives for v1 workspaces.
//!
//! A template is selected by role directory basename. Its parent directory under
//! the template root is the topology name. Template files remain opaque bytes;
//! UTF-8 files receive the closed placeholder substitution pass.

use std::{
    borrow::Cow,
    fmt, fs, io,
    path::{Path, PathBuf},
};

/// A role template selected below a template root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    /// Absolute or caller-provided path to the role directory.
    pub role_dir: PathBuf,
    /// Parent path under the template root, using `/` separators.
    pub topology: String,
    /// Template directory path relative to the template root.
    pub relative: String,
}

/// Placeholder values used while generating a role workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placeholders {
    pub role: String,
    pub cluster: String,
    pub server_name: String,
    pub listen: String,
    pub cert_pin: String,
    pub admin: String,
    pub max_sessions: String,
    pub agent_package: Option<String>,
}

/// Failure emitted by template discovery, loading, substitution, or scanning.
#[derive(Debug)]
pub enum TemplateError {
    /// Filesystem operation failed for a concrete path.
    Io { path: PathBuf, source: io::Error },
    /// More than one role directory matched.
    Ambiguous { role: String, paths: Vec<String> },
    /// No role directory matched below the supplied root.
    NoTemplate {
        role: String,
        template_root: PathBuf,
    },
    /// The caller selected a role/template intersection with no result.
    NoRoleMatches,
    /// A file contains a placeholder outside the closed set.
    UnknownPlaceholder { key: String, path: String },
    /// `{{agent_package}}` was requested with an empty package setting.
    AgentPackageUnset,
    /// A generated file contains an absolute source path.
    EmbeddedPath { path: String },
}

impl TemplateError {
    /// Every template operator failure exits with status 4.
    pub fn exit_code(&self) -> i32 {
        4
    }

    fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Ambiguous { role, paths } => write!(
                f,
                "onlyne: template for role {role} is ambiguous: {}",
                paths.join(", ")
            ),
            Self::NoTemplate {
                role,
                template_root,
            } => write!(
                f,
                "onlyne: no template directory named {role} under {}",
                template_root.display()
            ),
            Self::NoRoleMatches => {
                f.write_str("onlyne: no role matches the requested templates/roles")
            }
            Self::UnknownPlaceholder { key, path } => {
                write!(f, "onlyne: unknown placeholder {{{{{key}}}}} in {path}")
            }
            Self::AgentPackageUnset => {
                f.write_str("onlyne: agent_package not set in spec.toml [server]")
            }
            Self::EmbeddedPath { path } => {
                write!(f, "onlyne: generated workspace embeds absolute path {path}")
            }
        }
    }
}

impl std::error::Error for TemplateError {}

impl From<io::Error> for TemplateError {
    fn from(source: io::Error) -> Self {
        Self::Io {
            path: PathBuf::from("<unknown>"),
            source,
        }
    }
}

/// Recursively discover role directories below `template_root`.
///
/// Directories whose basename equals `role` are candidates. The candidate's
/// parent path relative to `template_root` is its topology. Dot-directories are
/// pruned while searching.
pub fn discover(template_root: &Path, role: &str) -> Result<Vec<Template>, TemplateError> {
    let mut matches = Vec::new();
    collect_role_dirs(template_root, role, &mut matches)
        .map_err(|source| TemplateError::io(template_root, source))?;
    matches.sort_by(|a, b| a.relative.cmp(&b.relative));
    if matches.is_empty() {
        return Err(TemplateError::NoTemplate {
            role: role.to_string(),
            template_root: template_root.to_path_buf(),
        });
    }
    if matches.len() > 1 {
        let paths = matches.iter().map(|item| item.relative.clone()).collect();
        return Err(TemplateError::Ambiguous {
            role: role.to_string(),
            paths,
        });
    }
    Ok(matches)
}

/// Load all ordinary files below one role template.
///
/// Dot-directories are omitted. `.onlyne/config.toml` is read by
/// [`local_override`] and never appears in this returned file list.
pub fn load_tree(template: &Template) -> Result<Vec<(String, Vec<u8>)>, TemplateError> {
    let mut files = Vec::new();
    collect_files(&template.role_dir, &template.role_dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

/// Load the optional `.onlyne/config.toml` local override fragment.
///
/// The fragment is parsed as a TOML value and is carried separately from the
/// opaque template file list. Invalid TOML returns an [`io::Error`] wrapped in
/// [`TemplateError::Io`] so callers retain the path.
pub fn local_override(template: &Template) -> Option<toml::Value> {
    let path = template.role_dir.join(".onlyne/config.toml");
    let bytes = fs::read(path).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    text.parse().ok()
}

/// Deep merge a local fragment with derived values.
///
/// The derived document wins on every overlapping key. Missing derived keys are
/// filled from `override_`, and nested tables are merged recursively.
pub fn merge_fragment(derived: &toml::Value, override_: &toml::Value) -> toml::Value {
    match (derived, override_) {
        (toml::Value::Table(derived_table), toml::Value::Table(override_table)) => {
            let mut merged = override_table.clone();
            for (key, derived_value) in derived_table {
                let value = if let Some(override_value) = merged.get(key) {
                    merge_fragment(derived_value, override_value)
                } else {
                    derived_value.clone()
                };
                merged.insert(key.clone(), value);
            }
            toml::Value::Table(merged)
        }
        (derived_value, _) => derived_value.clone(),
    }
}

/// Substitute the eight closed-set placeholders in a UTF-8 file.
///
/// Binary files and UTF-8 files without `{{` are returned borrowed. The
/// pathless convenience API records `"<template>"` in unknown-placeholder
/// errors; [`substitute_at`] lets a generator provide the actual file path.
pub fn substitute<'a>(bytes: &'a [u8], p: &Placeholders) -> Result<Cow<'a, [u8]>, TemplateError> {
    substitute_at(bytes, p, "<template>")
}

/// Substitute placeholders while reporting the concrete template file path.
pub fn substitute_at<'a>(
    bytes: &'a [u8],
    p: &Placeholders,
    path: impl Into<String>,
) -> Result<Cow<'a, [u8]>, TemplateError> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Ok(Cow::Borrowed(bytes));
    };
    if !text.contains("{{") {
        return Ok(Cow::Borrowed(bytes));
    }
    let path = path.into();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut changed = false;
    while let Some(relative_start) = text[cursor..].find("{{") {
        let start = cursor + relative_start;
        out.push_str(&text[cursor..start]);
        let after = start + 2;
        let Some(relative_end) = text[after..].find("}}") else {
            out.push_str(&text[start..]);
            break;
        };
        let end = after + relative_end;
        let key = &text[after..end];
        let replacement = match key {
            "role" => p.role.clone(),
            "cluster" => p.cluster.clone(),
            "server_name" => p.server_name.clone(),
            "listen" => p.listen.clone(),
            "cert_pin" => p.cert_pin.clone(),
            "admin" => p.admin.clone(),
            "max_sessions" => p.max_sessions.clone(),
            "agent_package" => p
                .agent_package
                .clone()
                .ok_or(TemplateError::AgentPackageUnset)?,
            unknown => {
                return Err(TemplateError::UnknownPlaceholder {
                    key: unknown.to_string(),
                    path,
                });
            }
        };
        out.push_str(&replacement);
        changed = true;
        cursor = end + 2;
    }
    if !changed {
        return Ok(Cow::Borrowed(bytes));
    }
    out.push_str(&text[cursor..]);
    Ok(Cow::Owned(out.into_bytes()))
}

/// Reject generated files containing any absolute path prefix.
///
/// Files are scanned in the order supplied, and prefixes in each file are
/// scanned in the order supplied. The first match is returned.
pub fn scan_for_prefixes(
    files: &[(String, Vec<u8>)],
    prefixes: &[String],
) -> Result<(), TemplateError> {
    for (path, bytes) in files {
        if prefixes.iter().any(|prefix| {
            !prefix.is_empty()
                && bytes
                    .windows(prefix.len())
                    .any(|window| window == prefix.as_bytes())
        }) {
            return Err(TemplateError::EmbeddedPath { path: path.clone() });
        }
    }
    Ok(())
}

fn collect_role_dirs(root: &Path, role: &str, matches: &mut Vec<Template>) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(root)?.collect::<Result<Vec<_>, io::Error>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        if name.to_string_lossy() == role {
            let relative_path = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let parent = path.parent().unwrap_or(root);
            let topology = parent
                .strip_prefix(root)
                .unwrap_or(parent)
                .to_string_lossy()
                .replace('\\', "/");
            matches.push(Template {
                role_dir: path.clone(),
                topology,
                relative: relative_path,
            });
        }
        collect_role_dirs(&path, role, matches)?;
    }
    Ok(())
}

fn collect_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), TemplateError> {
    let mut entries: Vec<_> = fs::read_dir(current)
        .map_err(|source| TemplateError::io(current, source))?
        .collect::<Result<Vec<_>, io::Error>>()
        .map_err(|source| TemplateError::io(current, source))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name_string = name.to_string_lossy();
        let file_type = entry
            .file_type()
            .map_err(|source| TemplateError::io(&entry.path(), source))?;
        if file_type.is_dir() {
            if name_string.starts_with('.') {
                continue;
            }
            collect_files(root, &entry.path(), out)?;
            continue;
        }
        if !file_type.is_file() || (current.ends_with(".onlyne") && name_string == "config.toml") {
            continue;
        }
        let path = entry.path();
        let bytes = fs::read(&path).map_err(|source| TemplateError::io(&path, source))?;
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        out.push((relative, bytes));
    }
    Ok(())
}
