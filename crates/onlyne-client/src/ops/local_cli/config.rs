//! The workspace `config.toml` plugin array: locate the top-level `plugins`
//! line, rewrite it in place, and fold the legacy `[[plugin]]` blocks the
//! 1.2.1 installer appended.

use anyhow::{Context, Result, anyhow};
use onlyne_layout::RoleWorkspace;
use std::path::Path;

/// The top-level `plugins` array line, its parsed ids, and the text the file
/// keeps after the closing bracket (a trailing comment or the line terminator).
struct PluginsLine {
    index: usize,
    ids: Vec<String>,
    tail: String,
}

/// The first top-level `plugins` assignment and any duplicate assignments whose
/// ids must fold into it.
struct PluginsEdit {
    primary: PluginsLine,
    duplicates: Vec<usize>,
}

/// Locate the top-level `plugins = [...]` assignment. TOML binds every key
/// after a table header to that table, so only lines before the first header
/// are the client's own. Duplicate top-level assignments merge their ids onto
/// the first line; a `plugins` line whose value is not a single-line array of
/// plain quoted strings is refused, never guessed at.
fn find_plugins(text: &str) -> Result<Option<PluginsEdit>> {
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let key = line.trim_end_matches('\r').trim_start();
        if key.is_empty() || key.starts_with('#') {
            continue;
        }
        if key.starts_with('[') {
            break;
        }
        let Some(value) = key_value(line, "plugins") else {
            continue;
        };
        let (ids, tail) = parse_plugins_value(value, index)?;
        found.push(PluginsLine { index, ids, tail });
    }
    let mut found = found.into_iter();
    let Some(mut primary) = found.next() else {
        return Ok(None);
    };
    let mut duplicates = Vec::new();
    for duplicate in found {
        for id in duplicate.ids {
            if !primary.ids.contains(&id) {
                primary.ids.push(id);
            }
        }
        duplicates.push(duplicate.index);
    }
    Ok(Some(PluginsEdit {
        primary,
        duplicates,
    }))
}

/// Parse one physical `plugins` value, naming the line whenever its shape is
/// outside the closed, single-line form this module can safely rewrite.
fn parse_plugins_value(value: &str, index: usize) -> Result<(Vec<String>, String)> {
    parse_inline_array(value).map_err(|error| anyhow!("{error} (line {})", index + 1))
}

/// Split a `name = value` line into the value text when `name` matches.
fn key_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let key = line.trim_end_matches('\r').trim_start();
    let rest = key.strip_prefix(name)?.trim_start();
    let value = rest.strip_prefix('=')?.trim_start();
    Some(value)
}

/// Parse a one-line inline array of quoted strings. Everything after the
/// closing bracket must be empty or a comment; the returned tail keeps it.
fn parse_inline_array(value: &str) -> Result<(Vec<String>, String)> {
    let value = value.trim_end();
    let open = value.strip_prefix('[').ok_or_else(|| {
        anyhow!("onlyne: config.toml keeps `plugins` in a shape this verb cannot edit; write it as plugins = [\"id\"]")
    })?;
    let close = open.rfind(']').ok_or_else(|| {
        anyhow!(
            "onlyne: config.toml splits the `plugins` array across lines; put every id on one line"
        )
    })?;
    let tail = open[close + 1..].to_string();
    if !tail.trim().is_empty() && !tail.trim().starts_with('#') {
        return Err(anyhow!(
            "onlyne: config.toml has trailing text on the `plugins` line: {value}"
        ));
    }
    let ids = open[..close]
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| {
            let bare = item.strip_prefix('"')?.strip_suffix('"')?;
            if bare.contains(['\\', '"']) {
                return None;
            }
            Some(bare.to_string())
        })
        .collect::<Option<Vec<String>>>()
        .ok_or_else(|| {
            anyhow!("onlyne: config.toml `plugins` array holds a value this verb cannot edit")
        })?;
    Ok((ids, tail))
}

/// The inline-array spelling this module always writes.
fn render_array(ids: &[String]) -> String {
    let items: Vec<String> = ids.iter().map(|id| format!("{id:?}")).collect();
    format!("[{}]", items.join(", "))
}

/// The config line the plugin verbs report to the operator.
pub(super) fn render_plugins(ids: &[String]) -> String {
    format!("plugins = {}", render_array(ids))
}

/// Rewrite the top-level `plugins` array line in place, or insert a fresh one
/// after `key_path` (falling back to just before the first table header, then
/// to the end of the file). Every other line keeps its exact bytes.
fn set_plugins(text: &str, ids: &[String]) -> Result<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let body = render_plugins(ids);
    match find_plugins(text)? {
        Some(edit) => {
            let old = &lines[edit.primary.index];
            let indent = &old[..old.len() - old.trim_start().len()];
            lines[edit.primary.index] = format!("{indent}{body}{}", edit.primary.tail);
            for duplicate in edit.duplicates.iter().rev() {
                lines.remove(*duplicate);
            }
        }
        None => {
            // Only lines before the first table header are top-level, so a
            // table that happens to hold a `key_path` key cannot mislead the
            // insertion point.
            let first_header = lines
                .iter()
                .position(|line| line.trim_end_matches('\r').trim_start().starts_with('['))
                .unwrap_or(lines.len());
            let insert_at = lines[..first_header]
                .iter()
                .position(|line| key_value(line, "key_path").is_some())
                .map_or(first_header, |index| index + 1);
            lines.insert(insert_at, body);
        }
    }
    let mut out = lines.join("\n");
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// Recognize a legacy plugin table header even when it uses spaces or carries
/// a trailing comment. The header line itself contains no quoted value, so the
/// first `#` is unambiguously the start of a comment.
fn is_plugin_table_header(line: &str) -> bool {
    let line = line.trim_end_matches('\r').trim_start();
    let line = match line.split_once('#') {
        Some((header, _)) => header.trim_end(),
        None => line,
    };
    let Some(inner) = line
        .strip_prefix("[[")
        .and_then(|line| line.strip_suffix("]]"))
    else {
        return false;
    };
    inner.trim() == "plugin"
}

/// Fold `[[plugin]]` blocks (the table form the 1.2.1 installer wrote, which
/// `ClientConfig` rejects) out of a config text. `only` restricts the fold to
/// blocks naming that id; every other block stays byte-for-byte. A block that
/// carries no parsable `id` line names no plugin, so it stays too and the
/// operator keeps seeing the loader's refusal.
fn fold_plugin_blocks(text: &str, only: Option<&str>) -> (String, Vec<String>, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let ends_with_newline = text.ends_with('\n');
    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    let mut ids: Vec<String> = Vec::new();
    let mut folded = 0usize;
    let mut cursor = 0;
    while cursor < lines.len() {
        if !is_plugin_table_header(lines[cursor]) {
            kept.push(lines[cursor]);
            cursor += 1;
            continue;
        }
        let start = cursor;
        cursor += 1;
        while cursor < lines.len()
            && !lines[cursor]
                .trim_end_matches('\r')
                .trim_start()
                .starts_with('[')
        {
            cursor += 1;
        }
        let block_id = block_id_value(&lines[start + 1..cursor]);
        let owned = match (only, &block_id) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(wanted), Some(id)) => id == wanted,
        };
        if owned {
            folded += 1;
            let id = block_id.unwrap();
            if !ids.contains(&id) {
                ids.push(id);
            }
        } else {
            kept.extend_from_slice(&lines[start..cursor]);
        }
    }
    let mut out = kept.join("\n");
    if ends_with_newline && !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    (out, ids, folded)
}

/// The `id = "..."` entry inside one `[[plugin]]` block.
fn block_id_value(block: &[&str]) -> Option<String> {
    block.iter().find_map(|line| {
        let value = key_value(line, "id")?.trim_end();
        let bare = value.strip_prefix('"')?.strip_suffix('"')?;
        if bare.contains(['\\', '"']) {
            return None;
        }
        Some(bare.to_string())
    })
}

/// Replace a file through a sibling temp name so a half-written config never
/// survives a crash.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let name = path
        .file_name()
        .map(|name| format!("{}.tmp", name.to_string_lossy()))
        .unwrap_or_else(|| "onlyne-config.tmp".to_string());
    let tmp = path.with_file_name(name);
    std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

/// Whether the workspace config already registers this plugin id. A workspace
/// with no config file registers nothing.
pub(super) fn config_lists_plugin(workspace: &Path, plugin_id: &str) -> Result<bool> {
    let config = RoleWorkspace::resolve(workspace).config_path();
    let Ok(text) = std::fs::read_to_string(&config) else {
        return Ok(false);
    };
    Ok(find_plugins(&text)?.is_some_and(|edit| edit.primary.ids.iter().any(|id| id == plugin_id)))
}

/// Register `plugin_id` in the workspace `plugins` array and return the ids
/// the file lists afterwards. An id the merged array already holds changes the
/// registered set; duplicate top-level lines still fold onto the first line.
pub(super) fn append_plugin_entry(workspace: &Path, plugin_id: &str) -> Result<Vec<String>> {
    let config = RoleWorkspace::resolve(workspace).config_path();
    let text = std::fs::read_to_string(&config).unwrap_or_default();
    let edit = find_plugins(&text)?;
    let has_duplicates = edit
        .as_ref()
        .is_some_and(|edit| !edit.duplicates.is_empty());
    let mut ids = edit.map(|edit| edit.primary.ids).unwrap_or_default();
    let already_registered = ids.iter().any(|id| id == plugin_id);
    if !already_registered {
        ids.push(plugin_id.to_string());
    }
    if !already_registered || has_duplicates {
        let updated = set_plugins(&text, &ids)?;
        write_atomic(&config, &updated)?;
    }
    Ok(ids)
}

/// Deregister `plugin_id`: drop it from the `plugins` array and fold away any
/// leftover `[[plugin]]` block naming it, so uninstalling a 1.2.1-era entry
/// still recovers a workspace that never restarted the client.
pub(super) fn remove_plugin_entry(workspace: &Path, plugin_id: &str) -> Result<()> {
    let config = RoleWorkspace::resolve(workspace).config_path();
    let text =
        std::fs::read_to_string(&config).with_context(|| format!("read {}", config.display()))?;
    let (body, _ids, _folded) = fold_plugin_blocks(&text, Some(plugin_id));
    let Some(edit) = find_plugins(&body)? else {
        if body != text {
            write_atomic(&config, &body)?;
        }
        return Ok(());
    };
    if !edit.primary.ids.iter().any(|id| id == plugin_id) {
        // The array holds nothing, but a legacy block naming this plugin may
        // have been folded above; that removal still has to land.
        if body != text {
            write_atomic(&config, &body)?;
        }
        return Ok(());
    }
    let kept: Vec<String> = edit
        .primary
        .ids
        .into_iter()
        .filter(|id| id != plugin_id)
        .collect();
    let updated = set_plugins(&body, &kept)?;
    write_atomic(&config, &updated)
}

/// Whether the top-level part of the config repeats `plugins`. This check only
/// recognizes the key; value safety stays with the parser that will rewrite it.
fn has_duplicate_plugins_lines(text: &str) -> bool {
    let mut seen = false;
    for line in text.lines() {
        let key = line.trim_end_matches('\r').trim_start();
        if key.is_empty() || key.starts_with('#') {
            continue;
        }
        if key.starts_with('[') {
            break;
        }
        if key_value(line, "plugins").is_some() {
            if seen {
                return true;
            }
            seen = true;
        }
    }
    false
}

/// Self-heal a workspace whose `config.toml` still carries the `[[plugin]]`
/// tables the 1.2.1 installer appended: merge their ids into the top-level
/// `plugins` array, drop the blocks, and rewrite the file atomically. Duplicate
/// top-level `plugins` lines merge onto the first line even when no legacy
/// block remains. Returns the number of folded blocks; `0` means no block was
/// folded, so an unrelated parse failure keeps reporting its own serde error.
/// A `plugins` array this module refuses to edit aborts the fold without
/// writing, and the loader's refusal then reaches the operator.
pub fn migrate_plugin_blocks(workspace: &Path) -> Result<usize> {
    let config = RoleWorkspace::resolve(workspace).config_path();
    let Ok(text) = std::fs::read_to_string(&config) else {
        return Ok(0);
    };
    let (body, block_ids, folded) = fold_plugin_blocks(&text, None);
    if folded == 0 && !has_duplicate_plugins_lines(&body) {
        return Ok(0);
    }
    let mut ids = find_plugins(&body)?
        .map(|edit| edit.primary.ids)
        .unwrap_or_default();
    for id in block_ids {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let updated = set_plugins(&body, &ids)?;
    if updated != body {
        write_atomic(&config, &updated)?;
    }
    Ok(folded)
}

/// Startup hook: fold the legacy blocks and print exactly one operator line
/// when the workspace self-healed or the fold was refused.
pub fn heal_workspace_config(workspace: &Path) {
    match migrate_plugin_blocks(workspace) {
        Ok(0) => {}
        Ok(_) => {
            eprintln!("onlyne: migrated [[plugin]] blocks into plugins = [...]");
        }
        Err(error) => eprintln!("{error}"),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod workspace_tests;
