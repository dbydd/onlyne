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
const MAX_KEY_LEN: usize = 64;

/// One freshly minted chat key: 32 hex characters, no dashes.
pub fn mint() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Whether `key` is a well-formed chat key value: 1-64 characters from the
/// alphabet `[A-Za-z0-9_-]`.
///
/// This is the shape check only. Whether the key is *known* — minted by this
/// gateway root and present in its registry — is the registry's answer, not
/// this one.
pub fn validate(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_LEN
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
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
    let (key, _reply, rest) = extract_with_reply(body)?;
    Ok((key, rest))
}

/// Extract the chat key and, when the body names one on its second line, the
/// platform event the reply answers; `rest` is the body after both lines.
///
/// A second line `<msg:id>` is optional and only meaningful to a platform
/// whose passive reply is only passive with the id it answers (QQ). The
/// default when it is absent is the conversation's own last inbound event,
/// which [`crate::chatlog`] holds — the reply then answers the message the
/// operator just read, which is the shape "reply to what came in" means.
pub fn extract_with_reply(body: &str) -> Result<(String, Option<String>, String), Misplaced> {
    // Line one: the key, or no key at all.
    let (key, rest) = match body.split_once('\n') {
        // A one-line body: it is either exactly one key or no key at all.
        None => match unwrap(body) {
            Some(key) => (key, String::new()),
            None if contains_key_shaped(body) => return Err(Misplaced),
            None => return Ok((String::new(), None, body.to_string())),
        },
        Some((first, rest)) => match unwrap(first) {
            Some(key) => (key, rest.to_string()),
            None if contains_key_shaped(body) => return Err(Misplaced),
            None => return Ok((String::new(), None, body.to_string())),
        },
    };
    // Line two, when it is one: the platform event this reply answers. An
    // ordinary `<msg:…>`-free body keeps every byte of `rest` as the payload.
    match rest.split_once('\n') {
        Some((line, tail)) => match unwrap_msg(line) {
            Some(id) => Ok((key, Some(id), tail.to_string())),
            None => Ok((key, None, rest)),
        },
        None => match unwrap_msg(&rest) {
            Some(id) => Ok((key, Some(id), String::new())),
            None => Ok((key, None, rest)),
        },
    }
}

/// The message id inside its `<msg:...>` wrapper, if the line is exactly that.
fn unwrap_msg(line: &str) -> Option<String> {
    let inner = line.strip_prefix("<msg:")?.strip_suffix('>')?;
    (!inner.is_empty() && inner.len() <= MAX_KEY_LEN).then(|| inner.to_string())
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
