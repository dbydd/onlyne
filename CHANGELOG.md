# Changelog

## [1.0.0] — tag 396d63f, published to crates.io and npm

Release channel: `cargo install onlyne-cli onlyne-server onlyne-client onlyne-gateway onlyne-tui`
(`onlyne-cli` installs the binary `onlyne`; the other four crates keep their names).
Pi adapter: `pi install npm:pi-onlyne` (1.0.0 speaks wire protocol 1).

### Fixed after the tag, in `main` (carried by the next version bump)

- cli: `onlyne reload` now arms the server unix socket it previously dropped,
  so admin-surface verbs reach a live server started from any cwd.
- config: `spec.toml` accepts `relay_required_count` as an alias of `relay_count`,
  matching the guard file's own spelling; canonical emission and the spec hash
  stay on `relay_count`.
- tui: floating stroke islands are pruned by border connectivity; a route lane
  hidden inside a role box no longer leaves orphan `│`/`─` fragments on the map.
- manifests: every internal path dependency now carries a registry version
  (`cargo publish` requirement); behaviour unchanged.
- repo: the stale `integrations/pi-onlyne` twin (pre-relay-guard code) is deleted;
  `plugins/onlyne-agent-pi` is the one canonical plugin source.

## [Unreleased] — 1.0.1 candidates

- client: record the adapter socket peer pid (kernel `LOCAL_PEERCRED`, not the
  self-reported `mount.pid`) at `hello`, and make `recycle`/`cancel` teardown
  close the backend resource AND kill that pid's group. Field report: a zombie
  `pi` process outlived its pane, reconnected to a fresh client, took a new
  assign, and relaunched its training batch by itself three times — pane dead
  plus ledger settled does not mean the agent process is dead.
- client: a takeover `hello` (same task_id, different kernel peer pid) bumps
  the session generation; reports carrying a stale generation are rejected
  with `conflict`, so two bodies cannot both write one session's history.
- client: record the session process group and tear it down with a group kill,
  so a `setsid`-detached subtree cannot outlive `recycle`/cancel (field report:
  an ownerless `run_005_resume` stole eight minutes after a bypass cancel).
- client: inject `ONLYNE_WORKSPACE` (canonicalised) and base the template
  `session_command` on `cd "$ONLYNE_WORKSPACE"`, ending worktree-mismatch
  double scenes where a relative `cd` resolves in the wrong checkout.
- server: `repair ack` replies echo the owning task/session, so attribution
  survives even though `faults.id` is a per-database autoincrement integer.
- tui/cli: the ledger rows already store the principal's `admin` flag; the
  human projections (ledger table, session view) should render it, so an audit
  read of a control row shows whether the signer acted as admin.
- docs: CLI argument shapes are versioned contracts. `--reason` sits on the
  `cancel`/`recycle` subcommands; `--from` is required on the admin surface
  and rejected on the client surface (`c243348` rule). Any future move of an
  option between top level and subcommand is a breaking change and lands here.
- docs: the `repair_*` verbs settle ledger and fault rows only; they send no
  signals. Killing a session's process goes through `control cancel|recycle`
  so the backend close path (and its respawn policy) stays in the loop;
  hand SIGTERMs on pane children invite terminal-level resurrection.
- docs: on a control frame `to` is the executing role (which client carries
  out the recycle/close), defaulting to the signer; a proxy cancel must pass
  `--to <task owner>` explicitly, or the executing client rejects ownership.
