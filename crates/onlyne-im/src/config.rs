//! The gateway root: its own `config.toml`, plus the conversation workspaces it
//! serves, read once at start.
//!
//! Two shapes of configuration, and they answer different questions:
//!
//! - `<root>/.onlyne/config.toml` is the **gateway's own**: the server endpoint
//!   and pin its links dial, the platform backends it spawns (`[[im.backend]]`,
//!   one process per bot connection), and the scripts an event or a beat runs
//!   (`[[im.push]]`, and nothing about where a script sends — that is the
//!   script's own call).
//! - `<root>/conversations/<role>/.onlyne/config.toml` is **one conversation's**:
//!   its platform coordinates. The directory name *is* the role, so the file
//!   never names one; the key lives at the fixed path beside it. Scanning
//!   `conversations/` is the whole list of what this gateway serves — there is
//!   no `[[link]]` table to keep in step with the directories.
//!
//! A conversation directory is complete on its own: coordinates, key, chat log,
//! and attachments all live under it, so moving one moves that IM conversation.
//!
//! What the loader refuses is the set of shapes that make a conversation
//! ambiguous: a duplicate `[[im.backend]]` id, a conversation whose `gateway` no
//! backend answers, two conversations claiming one platform conversation, or a
//! conversation with no key. Each refusal names the file and the remedy.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The directory under the gateway root that holds one workspace per
/// conversation; each child's name is a role.
pub const CONVERSATIONS_DIR: &str = "conversations";

/// How an `[[im.push]]` script is triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushMode {
    /// One run per platform event.
    Trigger,
    /// One run per fixed interval.
    Heartbeat,
}

/// One `[[im.push]]` entry: a command the gateway runs, in one of two modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushScript {
    pub mode: PushMode,
    /// argv, not a shell line — `run[0]` is the program.
    pub run: Vec<String>,
    /// Bound on one run.
    pub timeout: std::time::Duration,
    /// Only a `heartbeat` carries it; `0` is refused for a heartbeat.
    pub every: Option<std::time::Duration>,
}

/// One `[[im.backend]]` entry: a platform backend process, and the gateway id
/// whose conversations it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendEntry {
    /// The id this backend answers to; a conversation's `[chat].gateway` names it.
    pub gateway: String,
    /// The backend's argv, e.g. `["onlyne-im-backend-telegram"]`.
    pub run: Vec<String>,
}

/// One conversation the gateway serves: a workspace directory whose name is its
/// role, its platform coordinates, and the paths its runtime files live at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    /// The role, taken from the directory name — never from the file.
    pub role: String,
    /// `<root>/conversations/<role>`.
    pub workspace: PathBuf,
    /// Where this conversation's coordinates came from.
    pub config_path: PathBuf,
    /// `<workspace>/.onlyne/keys/role.key`, a fixed path.
    pub key_path: PathBuf,
    /// Which `[[im.backend]]` carries this conversation's traffic.
    pub gateway: String,
    /// The conversation's id on the platform.
    pub conversation: String,
}

impl Conversation {
    /// The directory inbound downloads and outbound attachments share.
    pub fn media_dir(&self) -> PathBuf {
        self.workspace.join("media")
    }

    /// The append-only chat log: one JSON object per line, both directions.
    pub fn chat_log(&self) -> PathBuf {
        self.workspace.join("chat.jsonl")
    }
}

/// Everything one gateway run reads from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gateway {
    /// The gateway root, as given to `--root`.
    pub root: PathBuf,
    /// The server endpoint every conversation link dials.
    pub server: String,
    /// The server certificate's SPKI pin.
    pub cert_pin: String,
    /// The platform backends to spawn, in declaration order.
    pub backends: Vec<BackendEntry>,
    /// The push scripts, in declaration order.
    pub pushes: Vec<PushScript>,
    /// The conversations found by scanning `conversations/`.
    pub conversations: Vec<Conversation>,
}

/// The gateway root's own document, before the conversation scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayFile {
    pub server: String,
    pub cert_pin: String,
    pub backends: Vec<BackendEntry>,
    pub pushes: Vec<PushScript>,
}

impl GatewayFile {
    /// Read and validate `<root>/.onlyne/config.toml`.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(".onlyne/config.toml");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("the gateway config {} is unreadable", path.display()))?;
        Self::parse(&text, &path.display().to_string())
    }

    /// Parse a gateway document, refusing the shapes the design forbids.
    pub fn parse(text: &str, file: &str) -> Result<Self> {
        let raw: RawConfig =
            toml::from_str(text).with_context(|| format!("{file} does not parse"))?;
        let mut parsed = GatewayFile {
            server: raw.server.endpoint,
            cert_pin: raw.server.cert_pin,
            backends: Vec::new(),
            pushes: Vec::new(),
        };
        if parsed.server.trim().is_empty() {
            bail!("{file}: `[server].endpoint` is required");
        }
        if parsed.cert_pin.trim().is_empty() {
            bail!("{file}: `[server].cert_pin` is required");
        }
        let mut seen: BTreeMap<String, ()> = BTreeMap::new();
        for backend in raw.im.backend {
            if backend.gateway.trim().is_empty() {
                bail!("{file}: an `[[im.backend]]` names no `gateway`");
            }
            if backend.run.is_empty() {
                bail!(
                    "{file}: `[[im.backend]]` gateway {} names no `run` argv",
                    backend.gateway
                );
            }
            if seen.insert(backend.gateway.clone(), ()).is_some() {
                // Two processes answering one id would each report the same
                // conversations, and the gateway could not say which rendered
                // an outbound message or owns the platform cursor.
                bail!(
                    "{file}: two `[[im.backend]]` rows name gateway {}",
                    backend.gateway
                );
            }
            parsed.backends.push(BackendEntry {
                gateway: backend.gateway,
                run: backend.run,
            });
        }
        for push in raw.im.push {
            parsed.pushes.push(push.into_script(file)?);
        }
        Ok(parsed)
    }

    /// The backend one gateway id names.
    pub fn backend(&self, gateway: &str) -> Option<&BackendEntry> {
        self.backends
            .iter()
            .find(|backend| backend.gateway == gateway)
    }
}

impl Gateway {
    /// Read a gateway root: its own document, then every conversation under
    /// `conversations/`, then the cross-checks only both halves can answer.
    pub fn load(root: &Path) -> Result<Self> {
        let file = GatewayFile::load(root)?;
        let mut gateway = Gateway {
            root: root.to_path_buf(),
            server: file.server,
            cert_pin: file.cert_pin,
            backends: file.backends,
            pushes: file.pushes,
            conversations: Vec::new(),
        };
        gateway.conversations = scan_conversations(root)?;
        gateway.check()?;
        Ok(gateway)
    }

    /// The cross-checks a conversation needs its gateway for: a gateway no
    /// backend answers, and one platform conversation claimed twice.
    fn check(&self) -> Result<()> {
        let mut claimed: BTreeMap<(&str, &str), &str> = BTreeMap::new();
        for conversation in &self.conversations {
            if self.backend(&conversation.gateway).is_none() {
                bail!(
                    "{}: `[chat].gateway = \"{}\"` names no `[[im.backend]]`; add that backend \
                     to the gateway config or name one it declares",
                    conversation.config_path.display(),
                    conversation.gateway
                );
            }
            let key = (
                conversation.gateway.as_str(),
                conversation.conversation.as_str(),
            );
            if let Some(other) = claimed.insert(key, conversation.role.as_str()) {
                // Outbound would be ambiguous and inbound would fire two roles'
                // scripts for one platform message.
                bail!(
                    "{}: conversation {} is also claimed by role {}; one platform conversation \
                     belongs to one role",
                    conversation.config_path.display(),
                    conversation.conversation,
                    other
                );
            }
        }
        Ok(())
    }

    /// The conversation one platform message belongs to.
    pub fn conversation_for(&self, gateway: &str, conversation: &str) -> Option<&Conversation> {
        self.conversations
            .iter()
            .find(|entry| entry.gateway == gateway && entry.conversation == conversation)
    }

    /// The conversation one role names.
    pub fn conversation(&self, role: &str) -> Option<&Conversation> {
        self.conversations.iter().find(|entry| entry.role == role)
    }

    /// The roles this gateway serves, in scan order.
    pub fn roles(&self) -> impl Iterator<Item = &str> {
        self.conversations.iter().map(|entry| entry.role.as_str())
    }

    /// The backend one gateway id names.
    pub fn backend(&self, gateway: &str) -> Option<&BackendEntry> {
        self.backends
            .iter()
            .find(|backend| backend.gateway == gateway)
    }
}

/// Scan `<root>/conversations/*/`, one conversation per directory.
///
/// A missing `conversations/` is a gateway that serves nothing yet — a fresh
/// `init` — and reads as empty. A stray file is skipped with a debug line
/// rather than refused: a `.DS_Store` or a README beside the workspaces is not
/// a mistake an operator needs told about.
fn scan_conversations(root: &Path) -> Result<Vec<Conversation>> {
    let dir = root.join(CONVERSATIONS_DIR);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            entries.push(path);
        } else {
            tracing::debug!(path = %path.display(), "a conversations/ entry is not a workspace");
        }
    }
    // Sort so role order is the directory order, not the filesystem's.
    entries.sort();
    let mut conversations = Vec::with_capacity(entries.len());
    for workspace in &entries {
        conversations.push(load_conversation(workspace)?);
    }
    Ok(conversations)
}

/// Read one conversation workspace.
fn load_conversation(workspace: &Path) -> Result<Conversation> {
    let role = workspace
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("{} has no readable name", workspace.display()))?
        .to_string();
    let config_path = workspace.join(".onlyne/config.toml");
    let file = config_path.display().to_string();
    let text = std::fs::read_to_string(&config_path).with_context(|| {
        format!(
            "the conversation config {file} is unreadable; a directory under `conversations/` \
             is one conversation workspace, and `onlyne-im generate` writes this file"
        )
    })?;
    let raw: RawConversation =
        toml::from_str(&text).with_context(|| format!("{file} does not parse"))?;
    let chat = raw
        .chat
        .context(format!("{file}: a `[chat]` table is required"))?;
    if chat.conversation.trim().is_empty() {
        bail!("{file}: `[chat].conversation` is required");
    }
    if chat.gateway.trim().is_empty() {
        bail!(
            "{file}: `[chat].gateway` is required; it names the `[[im.backend]]` that carries \
             this conversation"
        );
    }
    if let Some(declared) = raw.role.filter(|name| !name.trim().is_empty())
        && declared != role
    {
        // The directory name is the role: it is what a peer addresses and what
        // this gateway dials with, so a file that names another role would make
        // the same workspace answer to two names depending on the reader.
        bail!(
            "{file}: `role = \"{declared}\"` disagrees with the directory name \"{role}\"; the \
             directory name is the role — rename the directory or drop the key"
        );
    }
    let key_path = workspace.join(".onlyne/keys/role.key");
    if !key_path.is_file() {
        bail!(
            "{}: role {role} has no key; run `onlyne-im generate` in this gateway root to mint \
             one",
            key_path.display()
        );
    }
    Ok(Conversation {
        role,
        workspace: workspace.to_path_buf(),
        config_path,
        key_path,
        gateway: chat.gateway,
        conversation: chat.conversation,
    })
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    server: RawServer,
    #[serde(default)]
    im: RawIm,
}

#[derive(Debug, Default, Deserialize)]
struct RawServer {
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    cert_pin: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawIm {
    #[serde(default)]
    backend: Vec<RawBackend>,
    #[serde(default)]
    push: Vec<RawPush>,
}

#[derive(Debug, Deserialize)]
struct RawBackend {
    gateway: String,
    run: Vec<String>,
}

/// One conversation's own document: its coordinates, and the directory name is
/// its role.
#[derive(Debug, Deserialize)]
struct RawConversation {
    /// Read only to refuse a disagreement with the directory name.
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    chat: Option<RawChat>,
}

#[derive(Debug, Deserialize)]
struct RawChat {
    gateway: String,
    conversation: String,
}

#[derive(Debug, Deserialize)]
struct RawPush {
    mode: String,
    run: Vec<String>,
    #[serde(default)]
    timeout: Option<String>,
    #[serde(default)]
    every: Option<String>,
}

impl RawPush {
    fn into_script(self, file: &str) -> Result<PushScript> {
        let mode = match self.mode.as_str() {
            "trigger" => PushMode::Trigger,
            "heartbeat" => PushMode::Heartbeat,
            other => bail!("{file}: unknown push mode '{other}'; use `trigger` or `heartbeat`"),
        };
        if self.run.is_empty() {
            bail!("{file}: a `[[im.push]]` names no `run` command");
        }
        let timeout = parse_duration(self.timeout.as_deref(), std::time::Duration::from_secs(10))
            .with_context(|| format!("{file}: `timeout`"))?;
        let every = self
            .every
            .as_deref()
            .map(|text| {
                parse_duration(Some(text), std::time::Duration::ZERO)
                    .with_context(|| format!("{file}: `every`"))
            })
            .transpose()?;
        match mode {
            PushMode::Heartbeat => {
                if every.is_none_or(|d| d.is_zero()) {
                    bail!("{file}: a `heartbeat` push needs a nonzero `every`");
                }
                if self.every.is_none() {
                    bail!("{file}: `timeout` on a heartbeat is fine, but `every` is required");
                }
            }
            PushMode::Trigger => {
                if every.is_some() {
                    bail!("{file}: `every` belongs to a `heartbeat`, not a `trigger`");
                }
            }
        }
        Ok(PushScript {
            mode,
            run: self.run,
            timeout,
            every,
        })
    }
}

/// Parse a duration as `30s`, `5m`, `1h`, or a bare integer of seconds.
pub(crate) fn parse_duration(
    text: Option<&str>,
    default: std::time::Duration,
) -> Result<std::time::Duration> {
    let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(default);
    };
    let (digits, unit) = text.split_at(
        text.find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len()),
    );
    let value: u64 = digits
        .parse()
        .with_context(|| format!("'{text}' is not a duration"))?;
    let seconds = match unit.trim() {
        "" | "s" => value,
        "m" => value * 60,
        "h" => value * 3600,
        "d" => value * 86400,
        other => bail!("unknown duration unit '{other}'"),
    };
    Ok(std::time::Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const ROOT: &str = r#"
[server]
endpoint = "127.0.0.1:7000"
cert_pin = "sha256/abc"

[[im.backend]]
gateway = "tg1"
run = ["onlyne-im-backend-telegram"]
"#;

    /// Lay out a gateway root: the gateway document plus one workspace per
    /// `(role, gateway, conversation)`, each with a real key.
    fn root(dir: &Path, document: &str, conversations: &[(&str, &str, &str)]) {
        std::fs::create_dir_all(dir.join(".onlyne")).unwrap();
        std::fs::write(dir.join(".onlyne/config.toml"), document).unwrap();
        std::fs::create_dir_all(dir.join(CONVERSATIONS_DIR)).unwrap();
        for (role, gateway, conversation) in conversations {
            let workspace = dir.join("conversations").join(role);
            write_conversation(&workspace, gateway, conversation, None, true);
        }
    }

    fn write_conversation(
        workspace: &Path,
        gateway: &str,
        conversation: &str,
        role: Option<&str>,
        with_key: bool,
    ) {
        std::fs::create_dir_all(workspace.join(".onlyne/keys")).unwrap();
        std::fs::write(
            workspace.join(".onlyne/config.toml"),
            format!(
                "{}[chat]\ngateway = \"{gateway}\"\nconversation = \"{conversation}\"\n",
                role.map(|name| format!("role = \"{name}\"\n"))
                    .unwrap_or_default()
            ),
        )
        .unwrap();
        if with_key {
            std::fs::write(workspace.join(".onlyne/keys/role.key"), [7u8; 32]).unwrap();
        }
    }

    #[test]
    fn a_gateway_root_reads_backends_pushes_and_conversations() {
        let dir = tempfile::tempdir().unwrap();
        let document = format!(
            "{ROOT}\n[[im.push]]\nmode = \"trigger\"\nrun = [\"/abs/in.sh\"]\ntimeout = \"5s\"\n\
             \n[[im.push]]\nmode = \"heartbeat\"\nevery = \"5m\"\nrun = [\"/abs/beat.sh\"]\n"
        );
        root(
            dir.path(),
            &document,
            &[("tg-group-1", "tg1", "-100"), ("tg-dm-2", "tg1", "2")],
        );
        let gateway = Gateway::load(dir.path()).unwrap();
        assert_eq!(
            gateway.roles().collect::<Vec<_>>(),
            vec!["tg-dm-2", "tg-group-1"]
        );
        assert_eq!(gateway.pushes.len(), 2);
        assert_eq!(gateway.pushes[1].mode, PushMode::Heartbeat);
        assert_eq!(gateway.pushes[1].every, Some(Duration::from_secs(300)));
        let found = gateway
            .conversation_for("tg1", "-100")
            .expect("the conversation");
        assert_eq!(found.role, "tg-group-1");
        // The runtime files hang off the conversation's own workspace.
        assert!(found.media_dir().starts_with(&found.workspace));
        assert!(found.chat_log().starts_with(&found.workspace));
    }

    #[test]
    fn a_root_without_conversations_serves_nothing_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        root(dir.path(), ROOT, &[]);
        std::fs::remove_dir_all(dir.path().join("conversations")).unwrap();
        let gateway = Gateway::load(dir.path()).unwrap();
        assert!(gateway.conversations.is_empty());
    }

    #[test]
    fn two_backends_one_id_are_refused() {
        let document = format!("{ROOT}\n[[im.backend]]\ngateway = \"tg1\"\nrun = [\"x\"]\n");
        let error = GatewayFile::parse(&document, "config.toml").unwrap_err();
        assert!(error.to_string().contains("name gateway tg1"), "{error}");
    }

    #[test]
    fn a_conversation_naming_an_absent_backend_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        root(dir.path(), ROOT, &[("tg-1", "tg9", "-1")]);
        let error = Gateway::load(dir.path()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("tg9"), "{message}");
        assert!(message.contains("no `[[im.backend]]`"), "{message}");
    }

    #[test]
    fn two_conversations_one_platform_chat_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        root(
            dir.path(),
            ROOT,
            &[("tg-a", "tg1", "-100"), ("tg-b", "tg1", "-100")],
        );
        let error = Gateway::load(dir.path()).unwrap_err();
        assert!(error.to_string().contains("belongs to one role"), "{error}");
    }

    #[test]
    fn a_workspace_without_a_key_names_the_command_that_fixes_it() {
        let dir = tempfile::tempdir().unwrap();
        root(dir.path(), ROOT, &[]);
        write_conversation(
            &dir.path().join("conversations/tg-1"),
            "tg1",
            "-1",
            None,
            false,
        );
        let error = Gateway::load(dir.path()).unwrap_err();
        assert!(error.to_string().contains("onlyne-im generate"), "{error}");
    }

    #[test]
    fn a_role_key_that_disagrees_with_the_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        root(dir.path(), ROOT, &[]);
        write_conversation(
            &dir.path().join("conversations/tg-1"),
            "tg1",
            "-1",
            Some("tg-other"),
            true,
        );
        let error = Gateway::load(dir.path()).unwrap_err();
        assert!(
            error.to_string().contains("directory name is the role"),
            "{error}"
        );
    }

    #[test]
    fn a_workspace_without_coordinates_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        root(dir.path(), ROOT, &[]);
        let workspace = dir.path().join("conversations/tg-1");
        std::fs::create_dir_all(workspace.join(".onlyne/keys")).unwrap();
        std::fs::write(workspace.join(".onlyne/keys/role.key"), [7u8; 32]).unwrap();
        std::fs::write(workspace.join(".onlyne/config.toml"), "").unwrap();
        let error = Gateway::load(dir.path()).unwrap_err();
        assert!(
            error.to_string().contains("`[chat]` table is required"),
            "{error}"
        );
    }

    #[test]
    fn a_heartbeat_needs_a_nonzero_every() {
        let document =
            format!("{ROOT}\n[[im.push]]\nmode = \"heartbeat\"\nrun = [\"/abs/b.sh\"]\n");
        let error = GatewayFile::parse(&document, "config.toml").unwrap_err();
        assert!(error.to_string().contains("nonzero `every`"), "{error}");
    }

    #[test]
    fn a_trigger_may_not_carry_every() {
        let document = format!(
            "{ROOT}\n[[im.push]]\nmode = \"trigger\"\nrun = [\"/abs/in.sh\"]\nevery = \"5m\"\n"
        );
        let error = GatewayFile::parse(&document, "config.toml").unwrap_err();
        assert!(
            error.to_string().contains("belongs to a `heartbeat`"),
            "{error}"
        );
    }

    #[test]
    fn durations_read_the_units_a_hook_accepts() {
        assert_eq!(
            parse_duration(Some("30s"), Duration::ZERO)
                .unwrap()
                .as_secs(),
            30
        );
        assert_eq!(
            parse_duration(Some("5m"), Duration::ZERO)
                .unwrap()
                .as_secs(),
            300
        );
        assert_eq!(
            parse_duration(Some("1h"), Duration::ZERO)
                .unwrap()
                .as_secs(),
            3600
        );
        assert_eq!(
            parse_duration(Some("90"), Duration::ZERO)
                .unwrap()
                .as_secs(),
            90
        );
        assert!(parse_duration(Some("5x"), Duration::ZERO).is_err());
    }
}
