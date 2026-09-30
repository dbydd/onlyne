use super::*;
use onlyne_config::{Placement, validate_drive_placement};

pub fn process_env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

fn env_nonempty(env: &BTreeMap<String, String>, key: &str) -> bool {
    env.get(key).is_some_and(|value| !value.is_empty())
}

fn orca_host_present(env: &BTreeMap<String, String>) -> bool {
    env_nonempty(env, "ORCA_PANE_KEY")
        || env_nonempty(env, "ORCA_TERMINAL_HANDLE")
        || env_nonempty(env, "ORCA_WORKTREE_ID")
}

fn zellij_host_present(env: &BTreeMap<String, String>) -> bool {
    env.contains_key("ZELLIJ")
}

/// The pane this process is already inside, if any: what an absent `placement`
/// probes, in the plan's order (orca, zellij).
fn probed_placement(env: &BTreeMap<String, String>) -> Option<Placement> {
    if orca_host_present(env) {
        Some(Placement::Orca)
    } else if zellij_host_present(env) {
        Some(Placement::Zellij)
    } else {
        None
    }
}

/// Resolve the placement this client runs under.
///
/// Precedence, highest first: a nonempty `ONLYNE_BACKEND` that names a
/// placement, the placement this run declared — the workspace config's
/// `placement` key for `onlyne-client run`, an embedding's own answer
/// otherwise — then the probe over the pane hosts, and finally `headless`
/// — the fallback the plan fixes for a machine with no terminal host
/// (`docs/v2-PLAN.md` §"驱动与放置").
///
/// An explicit name that matches nothing is refused by name. It is never
/// silently replaced by the probe, because a cluster running under a placement
/// nobody chose is the failure this split exists to remove.
pub fn detect_placement(
    env: &BTreeMap<String, String>,
    declared: Option<SessionPlacement>,
) -> Result<PlacementDetection> {
    let explicit = env
        .get("ONLYNE_BACKEND")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("auto"));
    if let Some(name) = &explicit {
        let Some(placement) = SessionPlacement::parse(name) else {
            return Err(UnknownPlacement(name.clone()).into());
        };
        return Ok(PlacementDetection {
            placement,
            source: SelectionSource::Explicit,
            explicit: Some(name.clone()),
        });
    }
    if let Some(placement) = declared {
        return Ok(PlacementDetection {
            placement,
            source: SelectionSource::Declared,
            explicit: None,
        });
    }
    let probed = probed_placement(env);
    Ok(PlacementDetection {
        placement: SessionPlacement::Named(probed.unwrap_or(Placement::Headless)),
        source: if probed.is_some() {
            SelectionSource::Probe
        } else {
            SelectionSource::Fallback
        },
        explicit: None,
    })
}

/// `onlyne-client doctor`: the placement this machine resolves, as JSON, with
/// exit 0 for every answer — a refusal included, which the caller reads as the
/// `refusal` line rather than as a process failure.
pub fn doctor_report(env: &BTreeMap<String, String>) -> Value {
    let detected = detect_placement(env, None);
    let found = detected.as_ref().ok();
    let placement = found.map(|detected| detected.placement.as_str());
    let selection = found.map(|detected| match detected.source {
        SelectionSource::Explicit => "explicit",
        SelectionSource::Declared => "declared",
        SelectionSource::Probe => "probe",
        SelectionSource::Fallback => "fallback",
    });
    let binary = match placement {
        Some("orca") => Some(
            env.get("ORCA_CLI_COMMAND")
                .filter(|value| !value.is_empty())
                .cloned()
                .unwrap_or_else(|| "orca".into()),
        ),
        Some("zellij") => Some(
            env.get("ZELLIJ_COMMAND")
                .filter(|value| !value.is_empty())
                .cloned()
                .unwrap_or_else(|| "zellij".into()),
        ),
        // `headless`, `external` and `fake` run or drive a command the role
        // config names, so there is no host binary to report.
        _ => None,
    };
    let mut report = serde_json::json!({
        "placement": placement,
        "placement_selection": selection,
        "binary": binary,
        "explicit": found.and_then(|detected| detected.explicit.clone()),
    });
    if let Err(error) = &detected {
        report["refusal"] = Value::String(error.to_string());
    }
    report
}

/// Build the backend one `drive × placement` pair selects.
///
/// The drive is a property of the runtime and arrives from the role's spec with
/// `welcome`; the placement is a property of this machine. Together they are
/// the plan's four rows:
///
/// | drive | placement | who starts the runtime |
/// |---|---|---|
/// | plugin | orca / zellij | the client, in that pane; the plugin dials back |
/// | plugin | headless | the client, in the background; the plugin dials back |
/// | plugin | external | nobody: the resident runtime dials in |
/// | acp | headless | the client, as a child it speaks ACP to on stdio |
/// | exec | any placement | the client, which reads the exit code |
///
/// The pair is validated first: `acp` pairs only with `headless`, because stdio
/// carries the ACP channel and cannot also be a pane's terminal.
///
/// `fake` is the in-process runtime the test suite selects through
/// `ONLYNE_BACKEND`; it ignores the drive, because it owns no process at all.
pub fn backend_for(
    drive: onlyne_config::Drive,
    placement: SessionPlacement,
    runner: Arc<dyn Runner>,
    policy: WorktreePolicy,
    acp: &AcpOptions,
) -> Result<Box<dyn SessionBackend>> {
    if let Some(named) = placement.named() {
        validate_drive_placement(drive, named).map_err(|message| anyhow::anyhow!("{message}"))?;
    }
    Ok(match placement {
        SessionPlacement::Fake => Box::new(fake::FakeBackend::new()),
        SessionPlacement::Named(Placement::Orca) => {
            Box::new(orca::OrcaBackend::with_policy(runner, policy))
        }
        SessionPlacement::Named(Placement::Zellij) => Box::new(zellij::ZellijBackend::new(runner)),
        SessionPlacement::Named(Placement::Headless) => match drive {
            onlyne_config::Drive::Acp => Box::new(acp::AcpBackend::new(acp.clone())),
            onlyne_config::Drive::Plugin | onlyne_config::Drive::Exec => {
                Box::new(exec::ExecBackend::new())
            }
        },
        SessionPlacement::Named(Placement::External) => match drive {
            onlyne_config::Drive::Plugin => Box::new(external::ExternalBackend::new()),
            onlyne_config::Drive::Acp | onlyne_config::Drive::Exec => {
                Box::new(exec::ExecBackend::new())
            }
        },
    })
}
