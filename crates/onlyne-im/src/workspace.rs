//! Laying a gateway down: `init`, `generate`, `login`.
//!
//! These three verbs are the whole of the gateway's setup, and each writes into
//! one of the two halves a gateway has (`docs/v2.2-IM-GATEWAY-PLAN.md` §7.1):
//!
//! - `init` writes the gateway root's own document and the `conversations/`
//!   directory beside it.
//! - `generate` mints **one conversation**: its workspace, its key, its
//!   coordinates — and then the cluster half, because a conversation nothing in
//!   the cluster can address is not a conversation. That second half is a
//!   `spec_apply` over the server's admin socket, so `toml_edit` keeps the
//!   operator's comments and the server reloads on the way out.
//! - `login` writes a platform credential where a backend process reads it.
//!
//! No templates are involved anywhere. A conversation is a small, fixed set of
//! files whose content follows from four flags, which is why this is a verb
//! here and not a template tree under a server root.

use anyhow::{Context, Result, bail};
use onlyne_config::Spec;
use onlyne_config::layout::ServerRoot;
use onlyne_im::config::GatewayFile;
use onlyne_net::KeyPair;
use onlyne_proto::{
    AdminOp, Frame, ResBody, SetSenders, SetTargets, SpecApply, SpecEdit, SpecView, UpsertRole,
    new_id,
};
use onlyne_wire::frame::{FrameReader, write_frame};
use onlyne_wire::socket::{
    RegistrationKind, apply_private_mode, connect_local, read_registration, socket_path,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one admin exchange may take.
const ADMIN_TIMEOUT: Duration = Duration::from_secs(20);

/// The role an IM conversation talks to when `--peer` names none: the cluster's
/// operator agent, which is what a message from a human is usually addressed to
/// (`examples/supervisor/run.py`, `onlyne_config::spec::SUPERVISOR_ROLE`).
pub const DEFAULT_PEER: &str = onlyne_config::SUPERVISOR_ROLE;

/// `onlyne-im init`.
#[derive(Debug, Clone)]
pub struct InitArgs {
    pub root: PathBuf,
    /// The server endpoint, or `None` to take it from `--server-root`'s spec.
    pub server: Option<String>,
    /// The SPKI pin, or `None` to take it from `--server-root`'s spec.
    pub cert_pin: Option<String>,
    pub server_root: Option<PathBuf>,
    pub force: bool,
}

/// `onlyne-im generate`.
#[derive(Debug, Clone)]
pub struct GenerateArgs {
    pub root: PathBuf,
    pub gateway: String,
    pub conversation: String,
    /// The role name; `None` derives one from the gateway and conversation ids.
    pub role: Option<String>,
    /// The roles this conversation may address and be addressed by.
    pub peers: Vec<String>,
    /// The server root whose `spec.toml` gains the role.
    pub server_root: Option<PathBuf>,
    /// An explicit admin socket, overriding discovery.
    pub socket: Option<PathBuf>,
}

/// `onlyne-im login`.
#[derive(Debug, Clone)]
pub struct LoginArgs {
    pub root: PathBuf,
    pub gateway: String,
    /// The token, or the file holding it. Exactly one must be present.
    pub token: Option<String>,
    pub token_file: Option<PathBuf>,
}

/// Write a gateway root: its document, and an empty `conversations/`.
pub fn init(args: &InitArgs) -> Result<()> {
    let (server, cert_pin) = resolve_server(args)?;
    let onlyne = args.root.join(".onlyne");
    std::fs::create_dir_all(&onlyne).with_context(|| format!("creating {}", onlyne.display()))?;
    let conversations = args.root.join(onlyne_im::config::CONVERSATIONS_DIR);
    std::fs::create_dir_all(&conversations)
        .with_context(|| format!("creating {}", conversations.display()))?;

    let path = onlyne.join("config.toml");
    if path.exists() && !args.force {
        bail!(
            "{} already exists; pass --force to replace it",
            path.display()
        );
    }
    let document = format!(
        "# The IM gateway's own configuration.\n\
         #\n\
         # A conversation is a directory under `conversations/` whose name is its\n\
         # role; this file holds what is the gateway's rather than one\n\
         # conversation's: which cluster to speak to, which platform backends to\n\
         # spawn, and which scripts an event runs.\n\
         \n\
         [server]\n\
         endpoint = {server:?}\n\
         cert_pin = {pin:?}\n\
         \n\
         # One process per bot connection. `gateway` is the id a conversation's\n\
         # `[chat].gateway` names; `run` is an argv, not a shell line.\n\
         [[im.backend]]\n\
         gateway = \"telegram\"\n\
         run = [\"onlyne-im-backend-telegram\", \"--gateway\", \"telegram\"]\n\
         \n\
         # What the gateway runs when an event or a beat arrives. `mode` is\n\
         # `trigger` (one run per platform event) or `heartbeat` (one run per\n\
         # `every`). Where a script sends is the script's own call.\n\
         # [[im.push]]\n\
         # mode = \"trigger\"\n\
         # run = [\"/abs/path/im-in.sh\"]\n\
         # timeout = \"10s\"\n",
        pin = cert_pin,
    );
    std::fs::write(&path, document).with_context(|| format!("writing {}", path.display()))?;
    println!("{}", path.display());
    println!("{}", conversations.display());
    Ok(())
}

/// Mint one conversation workspace, then register its role in the cluster.
///
/// The cluster half is the reason this is one verb and not two: the workspace
/// and the `[[client]]` row describe one role, and a run that wrote only the
/// first would leave a conversation nothing can address. So the server is
/// reached first, and a server that cannot be reached refuses before anything is
/// minted.
pub async fn generate(args: &GenerateArgs) -> Result<()> {
    let file = GatewayFile::load(&args.root)
        .with_context(|| format!("reading the gateway root {}", args.root.display()))?;
    if file.backend(&args.gateway).is_none() {
        let known: Vec<&str> = file.backends.iter().map(|b| b.gateway.as_str()).collect();
        bail!(
            "no `[[im.backend]]` names gateway {:?}; this root declares {}",
            args.gateway,
            if known.is_empty() {
                "none".to_string()
            } else {
                known.join(", ")
            }
        );
    }
    let role = match &args.role {
        Some(role) if !role.trim().is_empty() => role.clone(),
        Some(_) => bail!("`--role` is empty"),
        None => derive_role(&args.gateway, &args.conversation),
    };
    validate_role(&role)?;

    let workspace = args
        .root
        .join(onlyne_im::config::CONVERSATIONS_DIR)
        .join(&role);
    if workspace.exists() {
        bail!(
            "{} already exists; this role is already generated",
            workspace.display()
        );
    }

    // The cluster half first: a server we cannot reach is a refusal before any
    // file exists, and the role name is checked against the live spec so a
    // second `generate` for one role cannot silently overwrite a key.
    let endpoint = resolve_admin(args)?;
    let spec = spec_get(&endpoint).await?;
    if spec["spec"]["client"]
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["role"] == role.as_str()))
    {
        bail!(
            "role {role} is already in this cluster's spec.toml; choose another name with --role"
        );
    }

    let key = KeyPair::generate();
    let public = key.public_str();
    let peers = if args.peers.is_empty() {
        vec![DEFAULT_PEER.to_string()]
    } else {
        args.peers.clone()
    };
    // Every refusal that can be decided offline is decided here, before the
    // first byte is written: a `generate` that failed on a bad `--peer` after
    // minting a key and a directory would leave an operator cleaning up after a
    // command that reported nothing had happened.
    let edits = spec_edits(&role, &public, &peers, &spec["spec"])?;

    write_workspace(&workspace, &role, &args.gateway, &args.conversation, &key)?;

    match spec_apply(&endpoint, &spec_source_hash(&spec)?, edits).await {
        Ok(()) => {}
        Err(error) => {
            // The workspace is already on disk; a half-registered role is worse
            // than a named leftover, so say exactly what stands and what does
            // not rather than deleting a key an operator may already have read.
            bail!(
                "{error}\n\
                 the conversation workspace at {} was written and its role is NOT registered; \
                 fix the server and run `onlyne-im generate` again after removing that directory",
                workspace.display()
            );
        }
    }

    println!("{}", workspace.display());
    println!("role {role} registered with peers: {}", peers.join(", "));
    Ok(())
}

/// Write a platform credential where the backend process reads it.
pub fn login(args: &LoginArgs) -> Result<()> {
    let file = GatewayFile::load(&args.root)?;
    if file.backend(&args.gateway).is_none() {
        bail!(
            "no `[[im.backend]]` names gateway {:?}; credentials belong to a backend",
            args.gateway
        );
    }
    let token = match (&args.token, &args.token_file) {
        (Some(_), Some(_)) => bail!("pass `--token` or `--token-file`, not both"),
        (Some(token), None) => token.clone(),
        (None, Some(path)) => {
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
        }
        (None, None) => bail!("pass `--token` or `--token-file`"),
    };
    let token = token.trim();
    if token.is_empty() {
        bail!("the token is empty");
    }
    let dir = args.root.join(".onlyne/credentials");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{}.token", sanitize(&args.gateway)));
    std::fs::write(&path, token.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    apply_private_mode(&path).map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))?;
    // The path is printed, never the token: a token that reaches a terminal is
    // a token that reaches a scrollback.
    println!("{}", path.display());
    Ok(())
}

/// Derive a role name from the two ids, in the shape the design's examples use
/// (`tg-group-7788`).
fn derive_role(gateway: &str, conversation: &str) -> String {
    let gateway = sanitize(gateway);
    let conversation = sanitize(conversation);
    if conversation.is_empty() {
        gateway
    } else {
        format!("{gateway}-{conversation}")
    }
}

/// Keep a platform-supplied id to characters a role name and a directory name
/// can both carry.
fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// The names a role may take: the same shape a directory can hold and a peer
/// can address.
fn validate_role(role: &str) -> Result<()> {
    if role.is_empty() {
        bail!("the derived role name is empty; pass --role");
    }
    if !role
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        bail!("role {role:?} carries characters a role name cannot hold; pass --role");
    }
    if role.starts_with('.') {
        bail!("role {role:?} begins with a dot; pass --role");
    }
    Ok(())
}

/// Write one conversation workspace: its coordinates, its key, and the
/// directories its runtime files live in.
fn write_workspace(
    workspace: &Path,
    role: &str,
    gateway: &str,
    conversation: &str,
    key: &KeyPair,
) -> Result<()> {
    let onlyne = workspace.join(".onlyne");
    let keys = onlyne.join("keys");
    std::fs::create_dir_all(&keys).with_context(|| format!("creating {}", keys.display()))?;
    std::fs::create_dir_all(workspace.join("media"))
        .with_context(|| format!("creating {}", workspace.join("media").display()))?;

    let key_path = keys.join("role.key");
    key.save(&key_path)
        .map_err(|error| anyhow::anyhow!("{}: {error}", key_path.display()))?;
    apply_private_mode(&key_path)
        .map_err(|error| anyhow::anyhow!("{}: {error}", key_path.display()))?;

    let config_path = onlyne.join("config.toml");
    let document = format!(
        "# This conversation's platform coordinates. The directory name is the\n\
         # role, so this file never names one.\n\
         \n\
         [chat]\n\
         gateway = {gateway:?}\n\
         conversation = {conversation:?}\n"
    );
    std::fs::write(&config_path, document)
        .with_context(|| format!("writing {}", config_path.display()))?;
    let _ = role;
    Ok(())
}

/// The edits that make a role addressable: declare it, then set both sides of
/// each edge the ACL reads.
///
/// Reach is two-sided (`onlyne_config::Spec::acl_edges`): a peer reaches this
/// conversation only when the peer's `allowed_targets` names the role **and**
/// this role's `allowed_senders` names the peer. A half-written edge is the
/// failure `docs/v2-CONTRACT.md` §"Slice 6" is about, so both halves are written
/// here, and a peer whose own list is a `"*"` or already names the role is left
/// alone.
fn spec_edits(
    role: &str,
    public_key: &str,
    peers: &[String],
    spec: &Value,
) -> Result<Vec<SpecEdit>> {
    let mut edits = vec![SpecEdit::UpsertRole(UpsertRole {
        role: role.to_string(),
        key: Some(public_key.to_string()),
        prose: None,
        admin: None,
        max_sessions: Some(1),
    })];
    edits.push(SpecEdit::SetTargets(SetTargets {
        role: role.to_string(),
        targets: peers.to_vec(),
    }));
    edits.push(SpecEdit::SetSenders(SetSenders {
        role: role.to_string(),
        senders: peers.to_vec(),
    }));
    for peer in peers {
        let Some(row) = client_row(spec, peer) else {
            // A peer that is not in the cluster cannot be owed an edge, and the
            // loader would refuse the `allowed_targets` entry by name anyway.
            bail!(
                "peer {peer} is not a role in this cluster's spec.toml; \
                 name a registered role with --peer"
            );
        };
        let targets = string_list(&row["allowed_targets"]);
        // The supervisor's reach is one-sided: an empty or `"*"` list already
        // covers every role, so only a named list needs the append.
        let supervisor = peer == DEFAULT_PEER;
        let covered = targets.is_empty() && supervisor
            || targets.iter().any(|name| name == "*")
            || targets.iter().any(|name| name == role);
        if covered {
            continue;
        }
        let mut next = targets;
        next.push(role.to_string());
        edits.push(SpecEdit::SetTargets(SetTargets {
            role: peer.clone(),
            targets: next,
        }));
    }
    Ok(edits)
}

/// One `[[client]]` row of a parsed spec, matched by role.
fn client_row(spec: &Value, role: &str) -> Option<Value> {
    spec["client"]
        .as_array()?
        .iter()
        .find(|row| row["role"] == role)
        .cloned()
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The endpoint and pin `init` writes, from the flags or the server root's spec.
fn resolve_server(args: &InitArgs) -> Result<(String, String)> {
    let named = |flag: &Option<String>| flag.clone().filter(|value| !value.trim().is_empty());
    match (named(&args.server), named(&args.cert_pin)) {
        (Some(server), Some(pin)) => Ok((server, pin)),
        (server, pin) => {
            let root = args.server_root.as_ref().context(
                "`--server` and `--cert-pin` are both required unless `--server-root` is given",
            )?;
            let spec = Spec::load(ServerRoot::resolve(root).spec_path())
                .with_context(|| format!("reading the spec under {}", root.display()))?;
            Ok((
                server.unwrap_or(spec.server.listen),
                pin.unwrap_or(spec.server.cert_pin),
            ))
        }
    }
}

/// The admin socket to apply edits through.
///
/// Resolution is the CLI's own order — `--socket`, then `ONLYNE_SOCKET`, then
/// the server root — and the registration file answers last whether the socket
/// there is really a server's, because applying a role to a client's socket
/// would fail with a vocabulary error rather than a sentence an operator can act
/// on.
fn resolve_admin(args: &GenerateArgs) -> Result<PathBuf> {
    if let Some(socket) = &args.socket {
        return Ok(socket.clone());
    }
    if let Ok(path) = std::env::var("ONLYNE_SOCKET")
        && !path.is_empty()
    {
        return Ok(PathBuf::from(path));
    }
    let root = args.server_root.as_ref().context(
        "no admin socket: pass `--server-root <dir>` (the server root whose spec.toml gains this \
         role) or `--socket <path>`, or set ONLYNE_SOCKET",
    )?;
    let root = ServerRoot::resolve(root).root().to_path_buf();
    match read_registration(&root) {
        Ok(Some(registration)) if registration.kind == RegistrationKind::Server => {}
        Ok(Some(_)) => bail!(
            "{} serves a role client, not a server; the role has to be registered on a server \
             root (pass --server-root)",
            root.display()
        ),
        _ => bail!(
            "no server is registered for {}; start it (`onlyne-server run --root <dir>`) and try \
             again",
            root.display()
        ),
    }
    socket_path(&root).map_err(|error| anyhow::anyhow!("{error}"))
}

/// One admin exchange: send one op, read its answer.
async fn exchange(socket: &Path, op: AdminOp) -> Result<ResBody> {
    let mut stream = connect_local(socket)
        .await
        .map_err(|error| anyhow::anyhow!("connecting to {}: {error}", socket.display()))?;
    let frame: Frame<AdminOp> = Frame::req(new_id(), op);
    write_frame(&mut stream, &frame).await?;
    let mut reader = FrameReader::new();
    let answer = tokio::time::timeout(ADMIN_TIMEOUT, reader.next::<_, Frame<AdminOp>>(&mut stream))
        .await
        .map_err(|_| anyhow::anyhow!("{} did not answer in {ADMIN_TIMEOUT:?}", socket.display()))?
        .map_err(|error| anyhow::anyhow!("reading from {}: {error}", socket.display()))?
        .context("the socket closed before an answer")?;
    match answer {
        Frame::Res { body, .. } => Ok(body),
        other => bail!("expected a response frame, got {other:?}"),
    }
}

/// Read the live spec through `spec_get`.
async fn spec_get(socket: &Path) -> Result<Value> {
    let body = exchange(socket, AdminOp::SpecGet(Value::Null)).await?;
    if !body.ok {
        bail!("{}", refusal(&body));
    }
    body.data.context("the spec_get answer carried no data")
}

fn spec_source_hash(spec: &Value) -> Result<String> {
    let view: SpecView =
        serde_json::from_value(spec.clone()).context("the spec_get answer is not a spec view")?;
    Ok(view.source_hash)
}

/// Apply the edits and reload.
async fn spec_apply(socket: &Path, base_hash: &str, edits: Vec<SpecEdit>) -> Result<()> {
    let body = exchange(
        socket,
        AdminOp::SpecApply(SpecApply {
            base_hash: base_hash.to_string(),
            edits,
        }),
    )
    .await?;
    if !body.ok {
        bail!("{}", refusal(&body));
    }
    Ok(())
}

/// One refusal body as the sentence an operator reads.
fn refusal(body: &ResBody) -> String {
    match &body.error {
        Some(error) => format!("{}: {}", error.code, error.message),
        None => "refused".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_role_name_is_derived_from_the_two_ids() {
        assert_eq!(derive_role("tg1", "-1007788"), "tg1-1007788");
        assert_eq!(derive_role("telegram", "42"), "telegram-42");
    }

    #[test]
    fn a_role_name_refuses_what_a_directory_cannot_hold() {
        assert!(validate_role("tg-group-1").is_ok());
        assert!(validate_role("").is_err());
        assert!(validate_role("a/b").is_err());
        assert!(validate_role(".hidden").is_err());
    }

    #[test]
    fn the_edits_write_both_sides_of_every_edge() {
        let spec = json!({
            "client": [
                { "role": "_supervisor", "allowed_targets": ["a", "b"] },
            ]
        });
        let peers = vec!["_supervisor".to_string()];
        let edits = spec_edits("tg-group-1", "ed25519/x", &peers, &spec).unwrap();
        // Declare the role, then its own two lists, then the peer's list.
        assert!(matches!(edits[0], SpecEdit::UpsertRole(_)));
        assert!(matches!(edits[1], SpecEdit::SetTargets(_)));
        assert!(matches!(edits[2], SpecEdit::SetSenders(_)));
        let SpecEdit::SetTargets(peer_edit) = &edits[3] else {
            panic!("the peer's list is appended: {edits:?}");
        };
        assert_eq!(peer_edit.role, "_supervisor");
        assert_eq!(peer_edit.targets, vec!["a", "b", "tg-group-1"]);
    }

    #[test]
    fn a_peer_with_an_empty_supervisor_list_needs_no_edit() {
        // The supervisor's reach is one-sided and an empty list covers every
        // role, so appending would be a no-op edit that still rewrites the file.
        let spec = json!({ "client": [ { "role": "_supervisor" } ] });
        let edits = spec_edits("tg-1", "ed25519/x", &["_supervisor".to_string()], &spec).unwrap();
        assert_eq!(edits.len(), 3);
    }

    #[test]
    fn a_peer_that_is_not_a_role_is_refused_before_anything_is_written() {
        let spec = json!({ "client": [ { "role": "other" } ] });
        let error = spec_edits("tg-1", "ed25519/x", &["ghost".to_string()], &spec).unwrap_err();
        assert!(error.to_string().contains("not a role"), "{error}");
    }
}
