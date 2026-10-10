//! The spec surface: `spec_get`'s read and `spec_apply`'s one writer
//! (contract §"Slice 4").
//!
//! `spec.toml` has one writer in this process besides an operator's own editor.
//! `spec_apply` applies its typed edits to the document **as text** with
//! `toml_edit`, so the comments and the layout an operator wrote survive an
//! edit; validates the result with the parser that reads the file; renames a
//! temporary file over the original; and reloads through
//! [`crate::router::reload_spec`] — the body `AdminOp::Reload` and the SIGHUP
//! listener already share, so an applied edit and a hand edit cannot take two
//! different paths to the running cluster.
//!
//! Every refusal returns before the write, which is what makes "answers
//! `conflict` / `invalid` and writes nothing" one property rather than two.

use crate::router;
use crate::state::State;
use onlyne_config::layout::ServerRoot;
use onlyne_config::{Drive as FileDrive, RuntimeSection, Spec, source_hash};
use onlyne_proto::{
    Drive as WireDrive, ErrorCode, RemoveRole, ResBody, RoleRuntime, SetOwesTargets, SetProse,
    SetRuntime, SetSenders, SetSession, SetTargets, SpecApply, SpecEdit, SpecView, UpsertRole,
};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, TableLike, Value, value};

/// The `spec_get` answer: the parsed spec, the file it came from, and the hash
/// of that file's bytes.
pub fn get(state: &Arc<State>) -> ResBody {
    let path = spec_path(state);
    let file = display_name(&path);
    let (bytes, text) = match read_source(&path, file) {
        Ok(pair) => pair,
        Err(body) => return body,
    };
    let spec = match Spec::parse_named(&text, file) {
        Ok(spec) => spec,
        Err(error) => return invalid(file, error.to_string()),
    };
    ResBody::ok(json!(SpecView {
        path: path.display().to_string(),
        source_hash: source_hash(&bytes),
        spec: serde_json::to_value(&spec).unwrap_or_default(),
    }))
}

/// Apply one `spec_apply` request: check the hash, edit the document, validate,
/// write atomically, reload.
pub fn apply(state: &Arc<State>, args: &SpecApply) -> ResBody {
    let path = spec_path(state);
    let file = display_name(&path);
    if args.base_hash.is_empty() {
        return ResBody::err(
            ErrorCode::Invalid,
            "`base_hash` is empty: an edit is checked against the hash `spec_get` answered",
            Some("base_hash".to_string()),
        );
    }
    if args.edits.is_empty() {
        return ResBody::err(
            ErrorCode::Invalid,
            "`edits` is empty: a request that changes nothing does not rewrite spec.toml",
            Some("edits".to_string()),
        );
    }
    // One writer at a time across the read, the check, and the rename. Without
    // this the hash would only be a spot check: two requests that read the same
    // bytes would both write, and the second would silently undo the first.
    let _writing = state.spec_write.lock();
    let (bytes, text) = match read_source(&path, file) {
        Ok(pair) => pair,
        Err(body) => return body,
    };
    if source_hash(&bytes) != args.base_hash {
        return ResBody::err(
            ErrorCode::Conflict,
            format!("`base_hash` does not match {file}: the file moved after it was read"),
            Some("base_hash".to_string()),
        );
    }
    let mut document: DocumentMut = match text.parse() {
        Ok(document) => document,
        // A document `toml_edit` cannot hold is one the loader refuses too, so
        // the refusal travels in the loader's own voice.
        Err(error) => {
            return match Spec::parse_named(&text, file) {
                Err(loader) => invalid(file, loader.to_string()),
                Ok(_) => invalid(file, format!("{file}: {error}")),
            };
        }
    };
    if let Err(refusal) = apply_edits(&mut document, &args.edits) {
        return refusal.body();
    }
    let edited = document.to_string();
    if let Err(error) = Spec::parse_named(&edited, file) {
        return invalid(file, error.to_string());
    }
    if let Err(error) = write_atomically(&path, edited.as_bytes()) {
        return ResBody::err(
            ErrorCode::Internal,
            format!("{}: {error}", path.display()),
            None,
        );
    }
    match router::reload_spec(state) {
        Ok(outcome) => ResBody::ok(json!({
            "source_hash": source_hash(edited.as_bytes()),
            "spec_hash": outcome.spec_hash,
            "render": outcome.render,
            "roles": outcome.roles,
            "gateways": outcome.gateways,
            "routes": outcome.routes,
        })),
        Err(message) => invalid(file, message),
    }
}

/// The absolute path of the file this root's spec lives at.
fn spec_path(state: &State) -> PathBuf {
    ServerRoot::resolve(&state.root).spec_path()
}

/// The name the loader's messages use for a path.
fn display_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("spec.toml")
}

/// A refusal in the loader's voice: `invalid` naming the file.
fn invalid(file: &str, message: impl Into<String>) -> ResBody {
    ResBody::err(ErrorCode::Invalid, message, Some(file.to_string()))
}

/// Read the file's bytes and its text.
fn read_source(path: &Path, file: &str) -> Result<(Vec<u8>, String), ResBody> {
    let bytes = std::fs::read(path).map_err(|error| {
        ResBody::err(
            ErrorCode::Internal,
            format!("{}: {error}", path.display()),
            None,
        )
    })?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| invalid(file, format!("{file}: the file is not UTF-8")))?
        .to_string();
    Ok((bytes, text))
}

/// Replace the file with `bytes` in one step.
///
/// The temporary file is written beside the target — same directory, same
/// filesystem — and renamed over it, so a reader sees the old bytes or the new
/// ones and never a half-written file. The target's mode is copied when it has
/// one: a private `spec.toml` stays private.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let name = display_name(path);
    let stamp = format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or(0)
    );
    let temporary = directory.join(stamp);
    let mode = std::fs::metadata(path).ok().map(|meta| meta.permissions());
    let mut file = std::fs::File::create(&temporary)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| match &mode {
            Some(permissions) => std::fs::set_permissions(&temporary, permissions.clone()),
            None => Ok(()),
        })
        .and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        // A failed write must not leave the temporary beside the spec: the next
        // apply would read the directory, not the file, and an operator would
        // find a stray document in the tree.
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// A refused edit: an error code, the loader's voice for the message, and the
/// field the message names.
struct Refusal {
    code: ErrorCode,
    message: String,
    field: String,
}

impl Refusal {
    fn invalid(field: &str, message: String) -> Self {
        Refusal {
            code: ErrorCode::Invalid,
            message,
            field: field.to_string(),
        }
    }

    /// An edit naming a role no entry declares.
    fn unknown_role(role: &str) -> Self {
        Refusal::invalid(
            "role",
            format!(
                "role {role} is not declared by an entry in the file: `upsert_role` declares a role, \
                 the other edits rewrite one that exists"
            ),
        )
    }

    /// A key that does not hold the shape the edit writes.
    fn shape(key: &str, shape: &str) -> Self {
        Refusal::invalid(
            key,
            format!("{key} is not {shape} in the file, so this edit cannot replace it"),
        )
    }

    fn body(self) -> ResBody {
        ResBody::err(self.code, self.message, Some(self.field))
    }
}

/// Apply every edit, in order, to the document in memory.
fn apply_edits(document: &mut DocumentMut, edits: &[SpecEdit]) -> Result<(), Refusal> {
    for edit in edits {
        match edit {
            SpecEdit::UpsertRole(edit) => upsert_role(document, edit)?,
            SpecEdit::RemoveRole(edit) => remove_role(document, edit)?,
            SpecEdit::SetTargets(edit) => set_targets(document, edit)?,
            SpecEdit::SetOwesTargets(edit) => set_owes_targets(document, edit)?,
            SpecEdit::SetSenders(edit) => set_senders(document, edit)?,
            SpecEdit::SetProse(edit) => set_prose(document, edit)?,
            SpecEdit::SetSession(edit) => set_session(document, edit)?,
            SpecEdit::SetRuntime(edit) => set_runtime(document, edit)?,
        }
    }
    Ok(())
}

/// The `[[client]]` entries of the document, the array created when it has none.
fn clients(document: &mut DocumentMut) -> Result<&mut ArrayOfTables, Refusal> {
    let item = document
        .entry("client")
        .or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
    item.as_array_of_tables_mut()
        .ok_or_else(|| Refusal::shape("client", "an array of tables"))
}

/// The entry a role is declared by, refused when no entry declares it.
///
/// Every edit but `upsert_role` reaches its entry through here, which is what
/// keeps a typo from declaring a role: a name the file does not carry is
/// `unknown_role`, never a new entry.
fn require_entry<'a>(document: &'a mut DocumentMut, role: &str) -> Result<&'a mut Table, Refusal> {
    clients(document)?
        .iter_mut()
        .find(|entry| declares(entry, role))
        .ok_or_else(|| Refusal::unknown_role(role))
}

/// Whether one `[[client]]` entry declares `role`.
fn declares(entry: &Table, role: &str) -> bool {
    entry.get("role").and_then(Item::as_str) == Some(role)
}

/// `upsert_role`: rewrite the keys this edit names, or declare the entry.
fn upsert_role(document: &mut DocumentMut, edit: &UpsertRole) -> Result<(), Refusal> {
    if edit.role.is_empty() {
        return Err(Refusal::invalid("role", "`role` is empty".to_string()));
    }
    let tables = clients(document)?;
    if let Some(entry) = tables.iter_mut().find(|entry| declares(entry, &edit.role)) {
        // Only the keys the edit names move. An absent field is not a request
        // to clear one, so a key an operator wrote stays where it is.
        if let Some(key) = &edit.key {
            entry.insert("key", value(key.clone()));
        }
        if let Some(prose) = &edit.prose {
            put_string(entry, "prose", prose);
        }
        if let Some(admin) = edit.admin {
            entry.insert("admin", value(admin));
        }
        if let Some(max_sessions) = edit.max_sessions {
            entry.insert("max_sessions", value(i64::from(max_sessions)));
        }
        return Ok(());
    }
    let Some(key) = &edit.key else {
        return Err(Refusal::invalid(
            "key",
            format!(
                "role {} is not declared yet and this edit carries no `key`: an entry without one \
                 can never authenticate, so `key` is what declares a role",
                edit.role
            ),
        ));
    };
    let mut entry = Table::new();
    entry.insert("role", value(edit.role.clone()));
    entry.insert("key", value(key.clone()));
    if let Some(prose) = &edit.prose {
        entry.insert("prose", value(prose.clone()));
    }
    if let Some(admin) = edit.admin {
        entry.insert("admin", value(admin));
    }
    if let Some(max_sessions) = edit.max_sessions {
        entry.insert("max_sessions", value(i64::from(max_sessions)));
    }
    tables.push(entry);
    Ok(())
}

/// `remove_role`: drop the entry a role is declared by.
fn remove_role(document: &mut DocumentMut, edit: &RemoveRole) -> Result<(), Refusal> {
    let tables = clients(document)?;
    let index = tables.iter().position(|entry| declares(entry, &edit.role));
    match index {
        Some(index) => {
            tables.remove(index);
            Ok(())
        }
        None => Err(Refusal::unknown_role(&edit.role)),
    }
}

/// `set_targets`: replace the roles this role reaches.
fn set_targets(document: &mut DocumentMut, edit: &SetTargets) -> Result<(), Refusal> {
    let entry = require_entry(document, &edit.role)?;
    set_string_array(entry, "allowed_targets", &edit.targets)
}

/// `set_owes_targets`: replace the roles this role's sessions owe a delivery to.
fn set_owes_targets(document: &mut DocumentMut, edit: &SetOwesTargets) -> Result<(), Refusal> {
    let entry = require_entry(document, &edit.role)?;
    set_string_array(entry, "owes_targets", &edit.targets)
}

/// `set_senders`: replace the roles that reach this one.
fn set_senders(document: &mut DocumentMut, edit: &SetSenders) -> Result<(), Refusal> {
    let entry = require_entry(document, &edit.role)?;
    set_string_array(entry, "allowed_senders", &edit.senders)
}

/// `set_prose`: replace the role prose the next session of this role reads.
fn set_prose(document: &mut DocumentMut, edit: &SetProse) -> Result<(), Refusal> {
    let entry = require_entry(document, &edit.role)?;
    put_string(entry, "prose", &edit.prose);
    Ok(())
}

/// `set_session`: write the keys this edit names under `[client.timeout]` and
/// `[client.intent]`.
fn set_session(document: &mut DocumentMut, edit: &SetSession) -> Result<(), Refusal> {
    let entry = require_entry(document, &edit.role)?;
    if let Some(ready_ms) = edit.ready_ms {
        nested(entry, "timeout")?.insert("ready_ms", value(integer(ready_ms, "ready_ms")?));
    }
    if let Some(idle_ms) = edit.idle_ms {
        nested(entry, "timeout")?.insert("idle_ms", value(integer(idle_ms, "idle_ms")?));
    }
    if let Some(attempts) = edit.attempts {
        nested(entry, "intent")?.insert("attempts", value(i64::from(attempts)));
    }
    if let Some(backoff_ms) = &edit.backoff_ms {
        let policy = nested(entry, "intent")?;
        let values = ms_array(backoff_ms)?;
        set_values(policy, "backoff_ms", &values)?;
    }
    Ok(())
}

/// `set_runtime`: replace this role's `[client.runtime]` table.
fn set_runtime(document: &mut DocumentMut, edit: &SetRuntime) -> Result<(), Refusal> {
    let entry = require_entry(document, &edit.role)?;
    let runtime = file_runtime(&edit.runtime);
    let table = nested(entry, "runtime")?;
    put_string(table, "drive", runtime.drive.as_str());
    set_values(table, "command", &runtime.command)
}

/// The wire's runtime table as the file's.
///
/// The crossing `router::role_runtime` makes in the other direction: each crate
/// owns the spelling its side uses, and this is the one place the server reads
/// one in the other's terms.
fn file_runtime(runtime: &RoleRuntime) -> RuntimeSection {
    RuntimeSection {
        drive: match runtime.drive {
            WireDrive::Plugin => FileDrive::Plugin,
            WireDrive::Acp => FileDrive::Acp,
            WireDrive::Exec => FileDrive::Exec,
        },
        command: runtime.command.clone(),
    }
}

/// Convert milliseconds to the integer TOML holds, refusing what it cannot.
fn integer(ms: u64, field: &str) -> Result<i64, Refusal> {
    i64::try_from(ms).map_err(|_| {
        Refusal::invalid(
            field,
            format!("{field} is {ms} milliseconds, more than the file can hold"),
        )
    })
}

/// A `backoff_ms` list as TOML integers.
fn ms_array(values: &[u64]) -> Result<Vec<String>, Refusal> {
    values
        .iter()
        .map(|ms| integer(*ms, "backoff_ms").map(|value| value.to_string()))
        .collect()
}

/// The table a key holds, refused when the key holds something else.
///
/// `[client.timeout]`, `[client.intent]`, and `[client.runtime]` are the tables
/// the loader reads under those names; an edit writes one, creating it when the
/// entry has none. `TableLike` is what keeps a table written in the header form
/// and the same table written inline (`timeout = { ready_ms = 1 }`) the one
/// shape an edit reaches.
fn nested<'a>(entry: &'a mut Table, key: &str) -> Result<&'a mut dyn TableLike, Refusal> {
    let item = entry.entry(key).or_insert(Item::Table(Table::new()));
    item.as_table_like_mut()
        .ok_or_else(|| Refusal::shape(key, "a table"))
}

/// Write a string value, keeping the decoration the value it replaces carried:
/// the indentation and any comment trailing the line stay where the operator
/// wrote them.
fn put_string(table: &mut dyn TableLike, key: &str, text: &str) {
    let decor = table
        .get(key)
        .and_then(Item::as_value)
        .map(|value| value.decor().clone());
    let mut value = Value::from(text.to_string());
    if let Some(decor) = decor {
        *value.decor_mut() = decor;
    }
    table.insert(key, Item::Value(value));
}

/// Replace one array of strings, keeping the shape the operator wrote.
///
/// The array itself is never replaced: its decoration — the space before `[`, a
/// comment trailing the line, the newline before `]`, the trailing comma — is
/// the operator's and survives. Only the elements move, and a multi-line array
/// stays multi-line by reusing the indentation its first element carried.
fn set_string_array(
    entry: &mut dyn TableLike,
    key: &str,
    values: &[String],
) -> Result<(), Refusal> {
    let elements: Vec<Value> = values.iter().cloned().map(Value::from).collect();
    rewrite_array(entry, key, &elements)
}

/// Replace one array of integers, the same way [`set_string_array`] does.
fn set_values(entry: &mut dyn TableLike, key: &str, values: &[String]) -> Result<(), Refusal> {
    let elements: Vec<Value> = values
        .iter()
        .map(|text| match text.parse::<i64>() {
            Ok(number) => Value::from(number),
            Err(_) => Value::from(text.clone()),
        })
        .collect();
    rewrite_array(entry, key, &elements)
}

/// Replace the elements of one array, keeping the shape the operator wrote.
fn rewrite_array(entry: &mut dyn TableLike, key: &str, elements: &[Value]) -> Result<(), Refusal> {
    let array = string_array(entry, key)?;
    let indent = array
        .iter()
        .next()
        .and_then(|value| value.decor().prefix().cloned())
        .filter(|prefix| prefix.as_str().is_some_and(|text| text.contains('\n')));
    array.clear();
    for element in elements {
        let mut value = element.clone();
        if let Some(prefix) = &indent {
            value.decor_mut().set_prefix(prefix.clone());
        }
        array.push_formatted(value);
    }
    Ok(())
}

/// The array one key holds, refused when the key holds something else.
fn string_array<'a>(entry: &'a mut dyn TableLike, key: &str) -> Result<&'a mut Array, Refusal> {
    let item = entry
        .entry(key)
        .or_insert(Item::Value(Value::Array(Array::new())));
    item.as_array_mut()
        .ok_or_else(|| Refusal::shape(key, "an array"))
}
