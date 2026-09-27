use super::*;

pub fn process_env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

fn env_nonempty(env: &BTreeMap<String, String>, key: &str) -> bool {
    env.get(key).is_some_and(|value| !value.is_empty())
}

pub(super) fn herdr_host_present(env: &BTreeMap<String, String>) -> bool {
    env.get("HERDR_ENV").is_some_and(|value| value == "1")
        && (env_nonempty(env, "HERDR_SOCKET_PATH")
            || env_nonempty(env, "HERDR_SESSION")
            || env_nonempty(env, "HERDR_WORKSPACE_ID"))
}

fn orca_host_present(env: &BTreeMap<String, String>) -> bool {
    env_nonempty(env, "ORCA_PANE_KEY")
        || env_nonempty(env, "ORCA_TERMINAL_HANDLE")
        || env_nonempty(env, "ORCA_WORKTREE_ID")
}

fn zellij_host_present(env: &BTreeMap<String, String>) -> bool {
    env.contains_key("ZELLIJ")
}

/// Map process environment to a backend choice.
pub fn detect_host(env: &BTreeMap<String, String>) -> HostDetection {
    let explicit = env
        .get("ONLYNE_BACKEND")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(name) = &explicit {
        if !name.eq_ignore_ascii_case("auto") {
            return HostDetection {
                backend: BackendName::parse(name),
                source: SelectionSource::Explicit,
                explicit: Some(name.clone()),
            };
        }
    }
    let backend = if herdr_host_present(env) {
        Some(BackendName::Herdr)
    } else if orca_host_present(env) {
        Some(BackendName::Orca)
    } else if zellij_host_present(env) {
        Some(BackendName::Zellij)
    } else {
        None
    };
    HostDetection {
        source: if backend.is_some() {
            SelectionSource::Env
        } else {
            SelectionSource::None
        },
        backend,
        explicit,
    }
}

pub fn doctor_report(env: &BTreeMap<String, String>) -> Value {
    let detected = detect_host(env);
    let host = detected.backend.map(|name| name.as_str());
    let backend_selection = match detected.source {
        SelectionSource::Explicit => "explicit",
        SelectionSource::Env => "env",
        SelectionSource::None => "none",
    };
    let binary = match detected.backend {
        Some(BackendName::Herdr) => Some(
            env.get("HERDR_BIN_PATH")
                .filter(|value| !value.is_empty())
                .cloned()
                .unwrap_or_else(|| "herdr".into()),
        ),
        Some(BackendName::Orca) => Some(
            env.get("ORCA_CLI_COMMAND")
                .filter(|value| !value.is_empty())
                .cloned()
                .unwrap_or_else(|| "orca".into()),
        ),
        Some(BackendName::Zellij) => Some(
            env.get("ZELLIJ_COMMAND")
                .filter(|value| !value.is_empty())
                .cloned()
                .unwrap_or_else(|| "zellij".into()),
        ),
        // `exec`, `acp` and `fake` run or drive a command the role config names,
        // so there is no host binary to report.
        Some(BackendName::Exec) | Some(BackendName::Acp) | Some(BackendName::Fake) => None,
        None => None,
    };
    let mut report = serde_json::json!({
        "host": host,
        "binary": binary,
        "session": env.get("HERDR_SESSION").filter(|value| !value.is_empty()),
        "workspace_id": env.get("HERDR_WORKSPACE_ID").filter(|value| !value.is_empty()),
        "tab_id": env.get("HERDR_TAB_ID").filter(|value| !value.is_empty()),
        "pane_id": env.get("HERDR_PANE_ID").filter(|value| !value.is_empty()),
        "backend_selection": backend_selection,
        "explicit": detected.explicit,
    });
    if detected.backend.is_none() {
        report["refusal"] = Value::String(NO_SUPPORTED_HOST.into());
    }
    report
}

/// Build a backend from an environment map. `ONLYNE_BACKEND` wins when it
/// names `herdr`, `orca`, `zellij`, `exec` (alias `headless`), `acp`, or
/// `fake`. An empty or `auto` value probes herdr, then orca, then zellij.
/// `exec`, `acp` and `fake` are never discovered. No match returns
/// [`NoSupportedHost`].
pub fn select_backend_from_env(
    env: &BTreeMap<String, String>,
    runner: Arc<dyn Runner>,
    policy: WorktreePolicy,
    acp: &AcpOptions,
) -> Result<Box<dyn SessionBackend>> {
    let detected = detect_host(env);
    match detected.backend {
        Some(name) => backend_by_name(name.as_str(), runner, policy, acp),
        None if detected
            .explicit
            .as_deref()
            .is_some_and(|name| !name.eq_ignore_ascii_case("auto")) =>
        {
            Err(anyhow::anyhow!(
                "unknown session backend: {}; accepted: {}",
                detected.explicit.unwrap_or_default(),
                BACKEND_NAMES
            ))
        }
        None => Err(NoSupportedHost.into()),
    }
}

/// Probe the process environment. Live `HERDR_*` / `ORCA_*` / `ZELLIJ` values
/// on the developer machine affect this path; tests use
/// [`select_backend_from_env`].
pub fn select_backend(
    runner: Arc<dyn Runner>,
    policy: WorktreePolicy,
    acp: &AcpOptions,
) -> Result<Box<dyn SessionBackend>> {
    let mut env = process_env();
    env.remove("ONLYNE_BACKEND");
    select_backend_from_env(&env, runner, policy, acp)
}

pub fn backend_by_name(
    name: &str,
    runner: Arc<dyn Runner>,
    policy: WorktreePolicy,
    acp: &AcpOptions,
) -> Result<Box<dyn SessionBackend>> {
    match BackendName::parse(name) {
        Some(BackendName::Herdr) => Ok(Box::new(herdr::HerdrBackend::new(runner))),
        Some(BackendName::Orca) => Ok(Box::new(orca::OrcaBackend::with_policy(runner, policy))),
        Some(BackendName::Zellij) => Ok(Box::new(zellij::ZellijBackend::new(runner))),
        Some(BackendName::Fake) => Ok(Box::new(fake::FakeBackend::new())),
        Some(BackendName::Exec) => Ok(Box::new(exec::ExecBackend::new())),
        Some(BackendName::Acp) => Ok(Box::new(acp::AcpBackend::new(acp.clone()))),
        None => Err(anyhow::anyhow!(
            "unknown session backend: {name}; accepted: {BACKEND_NAMES}"
        )),
    }
}

/// Resolve a backend name to a concrete backend. `auto` and empty probe the
/// supplied runner's process environment through [`select_backend`]. Named
/// values stay exact: an unknown name errors and names the accepted set.
pub fn backend_for(
    requested: &str,
    runner: Arc<dyn Runner>,
    policy: WorktreePolicy,
    acp: &AcpOptions,
) -> Result<Box<dyn SessionBackend>> {
    backend_for_env(requested, &process_env(), runner, policy, acp)
}

pub fn backend_for_env(
    requested: &str,
    env: &BTreeMap<String, String>,
    runner: Arc<dyn Runner>,
    policy: WorktreePolicy,
    acp: &AcpOptions,
) -> Result<Box<dyn SessionBackend>> {
    let name = requested.trim();
    if name.eq_ignore_ascii_case("auto") || name.is_empty() {
        let mut probe = env.clone();
        probe.remove("ONLYNE_BACKEND");
        return select_backend_from_env(&probe, runner, policy, acp);
    }
    backend_by_name(name, runner, policy, acp)
}

/// Client default backend: driven by `ONLYNE_BACKEND`
/// (`herdr` | `orca` | `zellij` | `exec`/`headless` | `acp` | `fake` | `auto`).
/// An empty value probes herdr, then orca, then zellij. `exec`, `acp` and
/// `fake` stay opt-in. `headless` selects [`BackendName::Exec`];
/// [`BackendName::as_str`] still answers `exec`.
///
/// `worktree` is the workspace config's `[orca] worktree` policy; only the
/// Orca backend reads it. `acp` is the `[acp]` table, read only by the ACP
/// backend.
pub fn default_backend(
    worktree: WorktreePolicy,
    acp: &AcpOptions,
) -> Result<Box<dyn SessionBackend>> {
    let env = process_env();
    backend_for_env(
        env.get("ONLYNE_BACKEND").map(String::as_str).unwrap_or(""),
        &env,
        Arc::new(ProcessRunner),
        worktree,
        acp,
    )
}
