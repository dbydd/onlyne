//! Adopting a conversation nobody registered.
//!
//! A chat id is only readable after a bot is already in the conversation, and
//! the bot was only started to serve conversations — so an operator who must
//! name every chat id up front cannot name the first one. This module closes
//! that loop: an inbound event for a conversation no workspace under
//! `conversations/` carries is *adopted*, which means minting the same
//! workspace `onlyne-im generate` writes — its coordinates, its chat key, its
//! media directory — and registering it for this run.
//!
//! What the adoption is **not** is a second source of identity. The chat key is
//! minted here, once, and written to the same `.onlyne/chat.key` the on-disk
//! registry has always used; a restart finds the workspace and reads the key
//! back. The registry is still the only minter — it has merely moved from the
//! operator's shell into the gateway, at the moment the first message arrives
//! (`docs/v2.3-CHAT-PROTOCOL.md` §6, the "a new conversation" row).
//!
//! Whether a conversation *may* be adopted is the gateway's own policy
//! ([`crate::config::ListenPolicy`]) and is checked by the caller, not here.

use crate::config::{CONVERSATIONS_DIR, Conversation, Gateway};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Adopt one conversation the registry does not carry yet.
///
/// The workspace is derived from the two ids, so the second event from the same
/// conversation finds the first one's directory and returns its key rather than
/// minting a rival. A directory that exists but names a *different*
/// conversation is refused: two platform conversations must not share one chat
/// key, which is the same cross-check the loader's `check` makes.
pub fn adopt(gateway: &Gateway, backend: &str, conversation: &str) -> Result<Conversation> {
    let slug = derive_slug(backend, conversation);
    validate_slug(&slug)?;
    let workspace = gateway.root.join(CONVERSATIONS_DIR).join(&slug);
    let config_path = workspace.join(".onlyne/config.toml");
    let chat_key = if config_path.is_file() {
        let existing = crate::config::load_conversation(&workspace)?;
        if existing.gateway != backend || existing.conversation != conversation {
            bail!(
                "{slug} already holds platform conversation {} on backend {}; give this one its \
                 own slug with `onlyne-im generate`",
                existing.conversation,
                existing.gateway
            );
        }
        existing.chat_key
    } else {
        write_workspace(&workspace, backend, conversation)?
    };
    let entry = Conversation {
        slug,
        workspace,
        config_path,
        gateway: backend.to_string(),
        conversation: conversation.to_string(),
        chat_key,
    };
    gateway.adopt(entry.clone())?;
    Ok(entry)
}

/// Derive a slug from the two ids, in the shape the design's examples use
/// (`tg1-1007788`, `feishu-oc_abc`).
pub fn derive_slug(gateway: &str, conversation: &str) -> String {
    let gateway = sanitize(gateway);
    let conversation = sanitize(conversation);
    if conversation.is_empty() {
        gateway
    } else {
        format!("{gateway}-{conversation}")
    }
}

/// Keep a platform-supplied id to characters a directory name can carry.
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

/// The names a slug may take: the same shape a directory can hold.
pub fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() {
        bail!("the derived slug is empty; pass --role");
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        bail!("slug {slug:?} carries characters a directory name cannot hold; pass --role");
    }
    if slug.starts_with('.') {
        bail!("slug {slug:?} begins with a dot; pass --role");
    }
    Ok(())
}

/// Write one conversation workspace: its coordinates, its minted chat key, and
/// the directories its runtime files live in.
///
/// The key is returned rather than printed: `onlyne-im generate` prints it for
/// an operator and an adoption logs it, and neither reads it from disk to find
/// out what it just wrote.
pub fn write_workspace(workspace: &Path, gateway: &str, conversation: &str) -> Result<String> {
    let onlyne = workspace.join(".onlyne");
    std::fs::create_dir_all(&onlyne).with_context(|| format!("creating {}", onlyne.display()))?;
    std::fs::create_dir_all(workspace.join("media"))
        .with_context(|| format!("creating {}", workspace.join("media").display()))?;

    let chat_key = crate::chatkey::mint();
    let key_path = onlyne.join("chat.key");
    std::fs::write(&key_path, &chat_key)
        .with_context(|| format!("writing {}", key_path.display()))?;
    onlyne_wire::socket::apply_private_mode(&key_path)
        .map_err(|error| anyhow::anyhow!("{}: {error}", key_path.display()))?;

    let config_path = onlyne.join("config.toml");
    let document = format!(
        "# This conversation's platform coordinates. The directory name is the\n\
        # slug, so this file never names one; the conversation's identity is\n\
        # the chat key beside it.\n\
         \n\
         [chat]\n\
         gateway = {gateway:?}\n\
         conversation = {conversation:?}\n"
    );
    std::fs::write(&config_path, document)
        .with_context(|| format!("writing {}", config_path.display()))?;
    Ok(chat_key)
}

/// The workspace one adopted conversation would live in, without touching the
/// filesystem. `onlyne-im check` prints it.
pub fn workspace_for(root: &Path, gateway: &str, conversation: &str) -> PathBuf {
    root.join(CONVERSATIONS_DIR)
        .join(derive_slug(gateway, conversation))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slug_joins_the_two_ids() {
        assert_eq!(derive_slug("tg1", "-1007788"), "tg1-1007788");
        assert_eq!(derive_slug("feishu", "oc_abc123"), "feishu-oc_abc123");
        assert_eq!(derive_slug("qqbot", "group:88"), "qqbot-group-88");
    }

    #[test]
    fn a_slug_refuses_what_a_directory_cannot_hold() {
        assert!(validate_slug("tg1-100").is_ok());
        assert!(validate_slug("").is_err());
        assert!(validate_slug("a/b").is_err());
        assert!(validate_slug(".hidden").is_err());
    }

    #[test]
    fn writing_a_workspace_mints_a_key_and_the_coordinates() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("conversations/tg1-100");
        let key = write_workspace(&workspace, "tg1", "-100").unwrap();
        assert!(crate::chatkey::validate(&key));
        let text = std::fs::read_to_string(workspace.join(".onlyne/config.toml")).unwrap();
        assert!(text.contains("gateway = \"tg1\""), "{text}");
        assert!(text.contains("conversation = \"-100\""), "{text}");
        assert!(workspace.join("media").is_dir());
    }

    #[test]
    fn adopting_twice_returns_the_first_key() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = Gateway {
            role: "im".into(),
            root: dir.path().to_path_buf(),
            server: "127.0.0.1:1".into(),
            cert_pin: "sha256/x".into(),
            backends: Vec::new(),
            pushes: Vec::new(),
            conversations: Default::default(),
            listen: Default::default(),
        };
        let first = adopt(&gateway, "tg1", "-100").unwrap();
        let second = adopt(&gateway, "tg1", "-100").unwrap();
        assert_eq!(first.chat_key, second.chat_key);
        assert_eq!(gateway.conversations().len(), 1);
    }
}
