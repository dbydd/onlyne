//! The chat key: the conversation identity a delivery carries in its text.
//!
//! `docs/v2.3-CHAT-PROTOCOL.md` §4. The core (server, client, delivery
//! template) never parses one; this module is the whole of the gateway's own
//! vocabulary for it:
//!
//! - [`mint`] mints a key: a uuid4 without its dashes, 32 characters.
//! - [`validate`] refuses a key that could not have come from this module —
//!   length, alphabet.
//! - [`extract`] pulls the key out of a body: the **first line** must be the
//!   key and the key alone, `<chat:...>` wrapped, which is the extraction rule
//!   the protocol's malformed-handling table is built around.
//!
//! The brackets exist because a client is a model, not a program: JWZ's survey
//! of real mailboxes found 4,554 of 22,950 `In-Reply-To` headers carried no
//! bracketed id at all, and the one extraction rule that survived forty years
//! of that abuse is "take the first `<...>`". Our rule is stricter — the whole
//! first line or nothing — because a reply that misfiles into another person's
//! conversation is worse than a reply that fails.

/// The longest body a key may be, between `chat:` and the closing bracket.
const MAX_KEY_LEN: usize = 128;

/// The longest platform id a `<msg:…>` or `<sticker:…>` marker may carry.
///
/// Not the chat key's bound: a key is minted here (32 hex characters) and a
/// bound on it is a shape check, but a platform id is whatever the platform
/// says. QQ's message ids are around 170 characters and its quote handles are
/// longer still, so a 64-character bound silently demotes a real `<msg:…>`
/// line to ordinary body text and the reader sees the id in the message.
/// Measured 2026-10-15 on a live group.
const MAX_ID_LEN: usize = 512;

/// One freshly minted chat key: 32 hex characters, no dashes.
pub fn mint() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Whether `key` is a well-formed chat key value: 1-64 characters from the
/// alphabet `[A-Za-z0-9_:-]`.
///
/// This is the shape check only. Whether the key is *known* — minted by this
/// gateway root and present in its registry — is the registry's answer, not
/// this one.
pub fn validate(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_LEN
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ':')
}

/// Pull the chat key off a body: the first line, wrapped as
/// `<chat:key>`, and nothing else on that line.
///
/// Three answers, and the caller treats them differently
/// (`docs/v2.3-CHAT-PROTOCOL.md` §6):
///
/// - `Ok((key, rest))` — the first line was the key; `rest` is the body after
///   the newline, verbatim.
/// - `Ok(("", body))` — the body starts with no key at all; the whole body
///   comes back untouched. This is the *no key* case, which the protocol lets
///   stand for deliveries that are not chat replies.
/// - `Err(Misplaced)` — a key-shaped thing exists in the body but not as the
///   whole first line, or the first line carries a key the shape check
///   refuses. A reply that names its conversation somewhere other than the
///   first line is a reply that could be misfiled, so it is refused rather
///   than guessed at.
pub fn extract(body: &str) -> Result<(String, String), Misplaced> {
    let markers = extract_markers(body)?;
    Ok((markers.key, markers.rest))
}

/// The marker lines a reply body opens with.
///
/// One line each, in this order, each optional after the key:
///
/// ```text
/// <chat:7f3a9c2e1b04d5a6f8c1e3b7a9d2f4c6>
/// <msg:ROBOT1.0_AdAX>
/// <sticker:CAACAgQAAxkBAAE>
///
/// the words the model wrote
/// ```
///
/// A leading line that is not one of these ends the header: everything from
/// that line on is the body, byte for byte. That is what keeps an ordinary
/// message — no markers, or only the key — exactly as it was, and what lets a
/// body quote another marker without it being read as one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Markers {
    /// The chat key, empty when the body names none.
    pub key: String,
    /// The platform message this reply answers (`<msg:…>`).
    pub reply_to: Option<String>,
    /// A sticker to send instead of words (`<sticker:…>`), which the gateway
    /// records as the message's metadata rather than as text.
    pub sticker: Option<String>,
    /// Everything after the header, verbatim.
    pub rest: String,
}

/// Pull the marker lines off a body.
///
/// Three answers, and the caller treats them differently
/// (`docs/v2.3-CHAT-PROTOCOL.md` §6):
///
/// - `Ok(markers)` — the body opened with the key, and any other marker line
///   that followed it. [`Markers::rest`] is the body after the header.
/// - `Ok(markers)` with an empty [`Markers::key`] — the body names no key at
///   all; the whole body comes back as `rest`, untouched. This is the *no key*
///   case, which the protocol lets stand for deliveries that are not chat
///   replies.
/// - `Err(Misplaced)` — a key-shaped thing exists in the body but not as the
///   whole first line, or the first line carries a key the shape check
///   refuses. A reply that names its conversation somewhere other than the
///   first line is a reply that could be misfiled, so it is refused rather
///   than guessed at.
pub fn extract_markers(body: &str) -> Result<Markers, Misplaced> {
    // The first line, whether or not a newline follows it: a body that is
    // exactly one key is a key with nothing under it.
    let (first, after_first) = match body.split_once('\n') {
        Some((first, tail)) => (first, tail),
        None => (body, ""),
    };
    let Some(key) = unwrap(first) else {
        return if contains_key_shaped(body) {
            Err(Misplaced)
        } else {
            Ok(Markers {
                rest: body.to_string(),
                ..Markers::default()
            })
        };
    };
    // The key is the first line; the marker lines that follow it are read in
    // order, and the first line that is not one of them starts the body.
    let mut rest = after_first;
    let mut markers = Markers {
        key,
        ..Markers::default()
    };
    for (slot, prefix, max) in [
        (&mut markers.reply_to, "<msg:", MAX_ID_LEN),
        (&mut markers.sticker, "<sticker:", MAX_ID_LEN),
    ] {
        if slot.is_some() {
            continue;
        }
        let Some(line) = rest.split_once('\n').map(|(line, _)| line) else {
            // The header ran to the end of the body with no trailing newline.
            if let Some(value) = unwrap_prefixed(rest, prefix, max) {
                *slot = Some(value);
                rest = "";
            }
            continue;
        };
        if let Some(value) = unwrap_prefixed(line, prefix, max) {
            *slot = Some(value);
            rest = rest.split_once('\n').map(|(_, tail)| tail).unwrap_or("");
        }
    }
    markers.rest = rest.to_string();
    Ok(markers)
}

/// The chat key and, when the body names one, the platform event it answers.
///
/// The two-value shape callers already read; [`extract_markers`] is the one
/// that carries everything.
pub fn extract_with_reply(body: &str) -> Result<(String, Option<String>, String), Misplaced> {
    let markers = extract_markers(body)?;
    Ok((markers.key, markers.reply_to, markers.rest))
}

/// The value inside one `<prefix:…>` wrapper, if the line is exactly that.
fn unwrap_prefixed(line: &str, prefix: &str, max: usize) -> Option<String> {
    let inner = line.strip_prefix(prefix)?.strip_suffix('>')?;
    let inner = inner.trim();
    (!inner.is_empty() && inner.len() <= max).then(|| inner.to_string())
}

/// Why [`extract`] refused: the body carries a key-shaped token, but not as
/// its first line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Misplaced;

/// The key inside its `<chat:...>` wrapper, if the line is exactly that.
fn unwrap(line: &str) -> Option<String> {
    let inner = line.strip_prefix("<chat:")?.strip_suffix('>')?;
    if inner.is_empty() || inner.len() > MAX_KEY_LEN {
        return None;
    }
    validate(inner).then_some(inner.to_string())
}

/// Whether a key-shaped token appears anywhere in the body, which is what
/// turns a missing first line into a refusal instead of a shrug.
fn contains_key_shaped(body: &str) -> bool {
    body.contains("<chat:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_is_thirty_two_hex() {
        let key = mint();
        assert_eq!(key.len(), 32);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn mint_is_unique_across_a_burst() {
        let keys: Vec<String> = (0..1000).map(|_| mint()).collect();
        let unique = keys.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), 1000);
    }

    #[test]
    fn validate_accepts_minted_and_alphabet_keys() {
        assert!(validate(&mint()));
        assert!(validate("a"));
        assert!(validate("Ab_09-z"));
        assert!(validate(&"x".repeat(MAX_KEY_LEN)));
    }

    #[test]
    fn validate_refuses_the_empty_overlong_and_wrong_alphabet() {
        assert!(!validate(""));
        assert!(!validate(&"x".repeat(MAX_KEY_LEN + 1)));
        assert!(!validate("has space"));
        assert!(!validate("has.dot"));
        assert!(!validate("has/slash"));
        assert!(!validate("大"));
    }

    #[test]
    fn extract_reads_the_first_line_and_keeps_the_rest_verbatim() {
        let key = mint();
        let body = format!("<chat:{key}>\n\n在的，说\n");
        let (got, rest) = extract(&body).expect("a well-placed key");
        assert_eq!(got, key);
        assert_eq!(rest, "\n在的，说\n");
    }

    #[test]
    fn extract_accepts_a_bare_key_whole_body() {
        let key = mint();
        let (got, rest) = extract(&format!("<chat:{key}>")).expect("one-line key");
        assert_eq!(got, key);
        assert!(rest.is_empty());
    }

    #[test]
    fn a_second_msg_line_names_the_event_this_reply_answers() {
        let key = mint();
        let body = format!("<chat:{key}>\n<msg:ROBOT1.0_abc>\n\n在的\n");
        let (got, reply, rest) = extract_with_reply(&body).expect("a well-placed key");
        assert_eq!(got, key);
        assert_eq!(reply.as_deref(), Some("ROBOT1.0_abc"));
        // The payload keeps its own blank line and text, byte for byte.
        assert_eq!(rest, "\n在的\n");
    }

    #[test]
    fn a_body_without_a_msg_line_keeps_every_byte_of_its_rest() {
        let key = mint();
        let body = format!("<chat:{key}>\n\n在的\n<msg:not-first>\n");
        let (got, reply, rest) = extract_with_reply(&body).expect("a well-placed key");
        assert_eq!(got, key);
        assert_eq!(reply, None, "only the line under the key counts");
        assert_eq!(rest, "\n在的\n<msg:not-first>\n");
        // `extract` stays the two-value shape its callers already read.
        assert_eq!(extract(&body).unwrap(), (got, rest));
    }

    #[test]
    fn a_sticker_marker_is_read_as_metadata_not_as_words() {
        let key = mint();
        let body = format!("<chat:{key}>\n<sticker:CAACAgQAAxkBAAE>\n\n");
        let markers = extract_markers(&body).expect("markers");
        assert_eq!(markers.key, key);
        assert_eq!(markers.sticker.as_deref(), Some("CAACAgQAAxkBAAE"));
        assert!(markers.reply_to.is_none());
        assert_eq!(markers.rest, "\n");
    }

    #[test]
    fn the_markers_may_appear_together_in_their_documented_order() {
        let key = mint();
        let body = format!("<chat:{key}>\n<msg:ROBOT1.0_AdAX>\n<sticker:abc>\n\nhi");
        let markers = extract_markers(&body).expect("markers");
        assert_eq!(markers.reply_to.as_deref(), Some("ROBOT1.0_AdAX"));
        assert_eq!(markers.sticker.as_deref(), Some("abc"));
        assert_eq!(markers.rest, "\nhi");
    }

    #[test]
    fn a_line_that_is_not_a_marker_starts_the_body() {
        let key = mint();
        let body = format!("<chat:{key}>\n\n<msg:not-a-marker-here>\n");
        let markers = extract_markers(&body).expect("markers");
        // The blank line ends the header, so a marker-shaped line inside the
        // body stays in the body — quoting one must not move it.
        assert!(markers.reply_to.is_none());
        assert_eq!(markers.rest, "\n<msg:not-a-marker-here>\n");
    }

    #[test]
    fn a_sticker_marker_with_nothing_in_it_is_not_a_marker() {
        let key = mint();
        let body = format!("<chat:{key}>\n<sticker:>\n\nhi");
        let markers = extract_markers(&body).expect("markers");
        assert!(markers.sticker.is_none());
        assert_eq!(markers.rest, "<sticker:>\n\nhi");
    }

    #[test]
    fn a_platform_id_far_longer_than_a_chat_key_is_still_a_marker() {
        // The live QQ message id, at its real length: a bound meant for a
        // minted key demoted this line to body text and the group saw it.
        let key = mint();
        let id = format!("ROBOT1.0_{}", "A".repeat(160));
        let body = format!("<chat:{key}>\n<msg:{id}>\n\n正文");
        let markers = extract_markers(&body).expect("markers");
        assert_eq!(markers.reply_to.as_deref(), Some(id.as_str()));
        assert_eq!(markers.rest, "\n正文", "the marker line must not leak");
    }

    #[test]
    fn a_marker_longer_than_any_real_id_stays_in_the_body() {
        let key = mint();
        let absurd = "A".repeat(MAX_ID_LEN + 1);
        let body = format!("<chat:{key}>\n<msg:{absurd}>\n\nhi");
        let markers = extract_markers(&body).expect("markers");
        assert!(markers.reply_to.is_none());
        assert!(markers.rest.starts_with("<msg:"), "{}", markers.rest);
    }

    #[test]
    fn extract_answers_no_key_for_an_ordinary_body() {
        let (got, rest) = extract("在的，说\n").expect("no key is fine");
        assert!(got.is_empty());
        assert_eq!(rest, "在的，说\n");
    }

    #[test]
    fn extract_refuses_a_key_that_is_not_the_first_line() {
        let key = mint();
        let body = format!("在的，说\n\n<chat:{key}>\n");
        assert_eq!(extract(&body), Err(Misplaced));
    }

    #[test]
    fn extract_refuses_a_first_line_with_prose_around_the_key() {
        let key = mint();
        let body = format!("reply <chat:{key}> now\n");
        assert_eq!(extract(&body), Err(Misplaced));
    }

    #[test]
    fn extract_refuses_a_malformed_first_line_key() {
        // Right shape of wrapper, key overlong.
        let overlong = "x".repeat(MAX_KEY_LEN + 1);
        assert_eq!(extract(&format!("<chat:{overlong}>\n")), Err(Misplaced));
        // Wrapper opened, never closed.
        assert_eq!(extract("<chat:abc\n"), Err(Misplaced));
    }
}
