//! Where a backend's credential comes from.
//!
//! One place, so every platform backend answers "do I have a credential, and
//! what is it" the same way. The rule is v1's (`frozen/gateway-v1`'s
//! `plugins/*/src/auth.rs`), carried over because it is the part operators
//! already know:
//!
//! - The gateway hands a backend the path to its credential file in
//!   `ONLYNE_IM_CREDENTIAL`, and no secret ever travels in an argv.
//! - A credential file holds either the secret itself or a `$NAME` reference,
//!   resolved from the environment. So an operator can keep a file naming a
//!   variable without keeping the variable's value.
//! - A missing credential is a state the backend reports, not an error it dies
//!   of: the gateway starts, says which backend is unauthenticated, and
//!   `onlyne-im login` fixes it.
//!
//! Every environment read goes through one injected lookup, so the tests pin
//! the precedence without mutating a process-wide variable.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The environment variable carrying the credential file's path.
pub const CREDENTIAL_ENV: &str = "ONLYNE_IM_CREDENTIAL";
/// The environment variable carrying this backend's gateway id.
pub const GATEWAY_ENV: &str = "ONLYNE_IM_GATEWAY";

/// One environment read: `None` for unset or empty.
pub type Lookup<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// One backend's credential, or why it has none.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Credential {
    /// The secret. Empty means the backend cannot authenticate — which is a
    /// state it reports, not an error it dies of.
    pub value: String,
    /// The sentence naming what is missing, when nothing was found.
    pub problem: Option<String>,
}

impl Credential {
    /// Whether a secret was found.
    pub fn is_present(&self) -> bool {
        !self.value.trim().is_empty()
    }
}

impl From<Result<String>> for Credential {
    fn from(loaded: Result<String>) -> Self {
        match loaded {
            Ok(value) if !value.trim().is_empty() => Credential {
                value,
                problem: None,
            },
            Ok(_) => Credential {
                value: String::new(),
                problem: Some("the credential is empty".to_string()),
            },
            Err(error) => Credential {
                value: String::new(),
                problem: Some(format!("{error:#}")),
            },
        }
    }
}

/// Load one backend's credential from the process environment, in the order an
/// operator expects:
///
/// 1. `explicit`, the run's own `--token` — a deliberate override.
/// 2. the file `ONLYNE_IM_CREDENTIAL` names, which is what `onlyne-im login`
///    wrote and what the gateway hands every backend it spawns.
/// 3. `platform_env`, the platform's own conventional variable, so an operator
///    with an exported token needs no file at all.
pub fn load(explicit: Option<&str>, platform_env: &str) -> Credential {
    let lookup = |name: &str| std::env::var(name).ok();
    load_with(explicit, platform_env, &lookup)
}

/// [`load`] against an injected environment, so a test pins the precedence
/// without setting a variable the whole test process shares.
pub fn load_with(explicit: Option<&str>, platform_env: &str, lookup: &Lookup<'_>) -> Credential {
    load_with_last_resort(explicit, platform_env, None, lookup)
}

/// [`load`] with one more source consulted **after** the platform variable.
///
/// Feishu and QQ both document a pair (`FEISHU_APP_ID` + `FEISHU_APP_SECRET`,
/// `QQBOT_APP_ID` + `QQBOT_APP_SECRET`) as well as the single combined string
/// this protocol carries. Reading the pair is what makes the two-variable form
/// real: without it, an operator who exports the pair the platform's own console
/// shows gets an unauthenticated backend and no sentence explaining why,
/// because the pair is never looked at.
///
/// The order is [`load`]'s, with the pair as its last resort: `--token`, then
/// the credential file, then the combined variable, then the pair joined with
/// `:`. A pair missing either half is not a half-credential — it falls through
/// to the same "nothing configured" state, so the sentence names every source
/// an operator could use.
pub fn load_two(
    explicit: Option<&str>,
    combined_env: &str,
    first_env: &str,
    second_env: &str,
) -> Credential {
    let lookup = |name: &str| std::env::var(name).ok();
    load_two_with(explicit, combined_env, first_env, second_env, &lookup)
}

/// [`load_two`] against an injected environment, the mirror of [`load_with`].
pub fn load_two_with(
    explicit: Option<&str>,
    combined_env: &str,
    first_env: &str,
    second_env: &str,
    lookup: &Lookup<'_>,
) -> Credential {
    let pair = || {
        let first = lookup(first_env)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let second = lookup(second_env)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        match (first, second) {
            (Some(first), Some(second)) => Some(format!("{first}:{second}")),
            _ => None,
        }
    };
    load_with_last_resort(explicit, combined_env, Some(&pair), lookup)
}

/// The shared body of [`load_with`] and [`load_two_with`].
///
/// `last_resort` is consulted only after the platform variable has come back
/// empty, so a pair can never override `--token`, the credential file, or the
/// combined variable — it is the answer to "the operator exported the pair the
/// platform's console shows, and nothing else".
fn load_with_last_resort(
    explicit: Option<&str>,
    platform_env: &str,
    last_resort: Option<&dyn Fn() -> Option<String>>,
    lookup: &Lookup<'_>,
) -> Credential {
    if let Some(raw) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        let loaded = resolve_with(raw, lookup);
        return Credential::from(loaded);
    }

    let path = lookup(CREDENTIAL_ENV)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let (source, loaded) = match &path {
        Some(path) => (
            format!("the credential file {path}"),
            read_file(Path::new(path)).and_then(|value| resolve_with(&value, lookup)),
        ),
        None => match lookup(platform_env)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            Some(value) => (platform_env.to_string(), resolve_with(&value, lookup)),
            None => match last_resort.and_then(|pair| pair()) {
                Some(joined) => (
                    format!("{platform_env} (the two-variable pair)"),
                    resolve_with(&joined, lookup),
                ),
                None => (
                    platform_env.to_string(),
                    Err(anyhow::anyhow!(
                        "the environment variable {platform_env} is not set (or is empty)"
                    )),
                ),
            },
        },
    };
    match loaded {
        Ok(value) if !value.trim().is_empty() => Credential {
            value,
            problem: None,
        },
        Ok(_) => Credential {
            value: String::new(),
            problem: Some(format!("{source} is empty")),
        },
        Err(error) => Credential {
            value: String::new(),
            problem: Some(format!("{error:#}")),
        },
    }
}

/// Read one credential file's contents, trimmed.
pub fn read_file(path: &Path) -> Result<String> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "reading the credential file {}; run `onlyne-im login` to write it",
            path.display()
        )
    })?;
    let value = text.trim().to_string();
    if value.is_empty() {
        bail!("the credential file {} is empty", path.display());
    }
    Ok(value)
}

/// Read and resolve one credential file against the process environment.
pub fn from_file(path: &Path) -> Result<String> {
    let lookup = |name: &str| std::env::var(name).ok();
    resolve_with(&read_file(path)?, &lookup).with_context(|| format!("in {}", path.display()))
}

/// Resolve one credential value against the process environment: `$NAME` reads
/// a variable, anything else is the secret itself.
pub fn resolve(value: &str) -> Result<String> {
    let lookup = |name: &str| std::env::var(name).ok();
    resolve_with(value, &lookup)
}

/// [`resolve`] against an injected environment.
///
/// A literal that merely *starts* with a dollar sign cannot be told apart from
/// a reference, which is the one ambiguity v1 accepted: a secret beginning
/// with `$` is written as `$$` and read back as the single dollar.
pub fn resolve_with(value: &str, lookup: &Lookup<'_>) -> Result<String> {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix("$$") {
        return Ok(format!("${rest}"));
    }
    let Some(name) = value.strip_prefix('$') else {
        return Ok(value.to_string());
    };
    let name = name.trim();
    if name.is_empty() {
        bail!("a credential of `$` names no variable");
    }
    match lookup(name).map(|value| value.trim().to_string()) {
        Some(value) if !value.is_empty() => Ok(value),
        _ => bail!("the environment variable {name} is not set (or is empty)"),
    }
}

/// The path a gateway root keeps one backend's credential at.
///
/// `onlyne-im login` writes this and the gateway hands the same path to the
/// backend in [`CREDENTIAL_ENV`], so the two halves cannot disagree about where
/// the secret lives.
pub fn credential_path(root: &Path, gateway: &str) -> PathBuf {
    root.join(".onlyne/credentials")
        .join(format!("{}.token", gateway))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// An environment built from pairs, so a test names exactly what is set.
    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    fn lookup(map: &BTreeMap<String, String>) -> impl Fn(&str) -> Option<String> + '_ {
        move |name: &str| map.get(name).cloned()
    }

    #[test]
    fn a_literal_value_is_the_secret() {
        let map = env(&[]);
        assert_eq!(resolve_with("123:abc", &lookup(&map)).unwrap(), "123:abc");
    }

    #[test]
    fn a_dollar_name_reads_the_environment() {
        let map = env(&[("TG_TOKEN", "from-env")]);
        assert_eq!(
            resolve_with("$TG_TOKEN", &lookup(&map)).unwrap(),
            "from-env"
        );
    }

    #[test]
    fn a_missing_variable_names_itself() {
        let map = env(&[]);
        let error = resolve_with("$TG_MISSING", &lookup(&map)).unwrap_err();
        assert!(error.to_string().contains("TG_MISSING"), "{error}");
    }

    #[test]
    fn a_double_dollar_is_a_literal_dollar() {
        let map = env(&[]);
        assert_eq!(resolve_with("$$ecret", &lookup(&map)).unwrap(), "$ecret");
    }

    #[test]
    fn an_explicit_token_beats_the_file_and_the_platform_variable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tg1.token");
        std::fs::write(&path, "from-file").unwrap();
        let map = env(&[
            (CREDENTIAL_ENV, path.to_str().unwrap()),
            ("TG_TOKEN", "from-env"),
        ]);
        let loaded = load_with(Some("from-flag"), "TG_TOKEN", &lookup(&map));
        assert_eq!(loaded.value, "from-flag");
        assert!(loaded.problem.is_none());
    }

    #[test]
    fn the_file_beats_the_platform_variable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tg1.token");
        std::fs::write(&path, "from-file").unwrap();
        let map = env(&[
            (CREDENTIAL_ENV, path.to_str().unwrap()),
            ("TG_TOKEN", "from-env"),
        ]);
        let loaded = load_with(None, "TG_TOKEN", &lookup(&map));
        assert_eq!(loaded.value, "from-file");
        assert!(loaded.problem.is_none());
    }

    #[test]
    fn the_platform_variable_is_the_last_resort() {
        let map = env(&[("TG_TOKEN", "from-env")]);
        let loaded = load_with(None, "TG_TOKEN", &lookup(&map));
        assert_eq!(loaded.value, "from-env");
        assert!(loaded.problem.is_none());
    }

    #[test]
    fn a_file_holding_a_reference_reads_the_variable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tg1.token");
        std::fs::write(&path, "$TG_TOKEN\n").unwrap();
        let map = env(&[
            (CREDENTIAL_ENV, path.to_str().unwrap()),
            ("TG_TOKEN", "from-env"),
        ]);
        let loaded = load_with(None, "TG_TOKEN", &lookup(&map));
        assert_eq!(loaded.value, "from-env");
    }

    #[test]
    fn nothing_configured_is_a_state_not_an_error() {
        let map = env(&[]);
        let loaded = load_with(None, "TG_TOKEN", &lookup(&map));
        assert!(!loaded.is_present());
        let problem = loaded.problem.expect("a reason");
        assert!(problem.contains("TG_TOKEN"), "{problem}");
    }

    #[test]
    fn an_unreadable_file_reports_its_own_error() {
        let map = env(&[(CREDENTIAL_ENV, "/nonexistent/onlyne-credential")]);
        let loaded = load_with(None, "TG_TOKEN", &lookup(&map));
        assert!(!loaded.is_present());
        let problem = loaded.problem.expect("a reason");
        assert!(problem.contains("onlyne-im login"), "{problem}");
    }

    #[test]
    fn a_file_holding_an_unset_variable_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tg1.token");
        std::fs::write(&path, "$TG_ABSENT").unwrap();
        let map = env(&[(CREDENTIAL_ENV, path.to_str().unwrap())]);
        let loaded = load_with(None, "TG_TOKEN", &lookup(&map));
        let problem = loaded.problem.expect("a reason");
        assert!(problem.contains("TG_ABSENT"), "{problem}");
    }

    #[test]
    fn a_two_variable_pair_is_the_last_resort() {
        let map = env(&[("FS_ID", "cli_x"), ("FS_SECRET", "s3cret")]);
        let loaded = load_two_with(None, "FS_CREDENTIAL", "FS_ID", "FS_SECRET", &lookup(&map));
        assert_eq!(loaded.value, "cli_x:s3cret");
        assert!(loaded.problem.is_none());
    }

    #[test]
    fn the_combined_variable_beats_the_pair() {
        let map = env(&[
            ("FS_CREDENTIAL", "from-combined"),
            ("FS_ID", "cli_x"),
            ("FS_SECRET", "s3cret"),
        ]);
        let loaded = load_two_with(None, "FS_CREDENTIAL", "FS_ID", "FS_SECRET", &lookup(&map));
        assert_eq!(loaded.value, "from-combined");
    }

    #[test]
    fn the_credential_file_beats_the_pair() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fs1.token");
        std::fs::write(&path, "from-file").unwrap();
        let map = env(&[
            (CREDENTIAL_ENV, path.to_str().unwrap()),
            ("FS_ID", "cli_x"),
            ("FS_SECRET", "s3cret"),
        ]);
        let loaded = load_two_with(None, "FS_CREDENTIAL", "FS_ID", "FS_SECRET", &lookup(&map));
        assert_eq!(loaded.value, "from-file");
    }

    #[test]
    fn an_explicit_token_beats_the_pair() {
        let map = env(&[("FS_ID", "cli_x"), ("FS_SECRET", "s3cret")]);
        let loaded = load_two_with(
            Some("from-flag"),
            "FS_CREDENTIAL",
            "FS_ID",
            "FS_SECRET",
            &lookup(&map),
        );
        assert_eq!(loaded.value, "from-flag");
    }

    #[test]
    fn a_half_set_pair_is_not_a_credential() {
        // One half alone answers nothing: a pair missing its secret is the same
        // "nothing configured" state, and the sentence still names the variable
        // an operator would export.
        let map = env(&[("FS_ID", "cli_x")]);
        let loaded = load_two_with(None, "FS_CREDENTIAL", "FS_ID", "FS_SECRET", &lookup(&map));
        assert!(!loaded.is_present());
        let problem = loaded.problem.expect("a reason");
        assert!(problem.contains("FS_CREDENTIAL"), "{problem}");
    }

    #[test]
    fn an_empty_pair_beats_nothing_and_names_the_pair() {
        let map = env(&[("FS_ID", "cli_x"), ("FS_SECRET", "")]);
        let loaded = load_two_with(None, "FS_CREDENTIAL", "FS_ID", "FS_SECRET", &lookup(&map));
        assert!(!loaded.is_present());
        let problem = loaded.problem.expect("a reason");
        assert!(problem.contains("FS_CREDENTIAL"), "{problem}");
    }

    #[test]
    fn an_empty_file_is_refused_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tg1.token");
        std::fs::write(&path, "   \n").unwrap();
        let error = read_file(&path).unwrap_err();
        assert!(error.to_string().contains("is empty"), "{error}");
    }

    #[test]
    fn a_missing_file_names_the_login_verb() {
        let dir = tempfile::tempdir().unwrap();
        let error = read_file(&dir.path().join("absent.token")).unwrap_err();
        assert!(error.to_string().contains("onlyne-im login"), "{error}");
    }

    #[test]
    fn the_path_is_the_one_login_writes() {
        let path = credential_path(Path::new("/root"), "tg1");
        assert_eq!(path, PathBuf::from("/root/.onlyne/credentials/tg1.token"));
    }
}
