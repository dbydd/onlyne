# Operations & Troubleshooting / 运维、服务托管与故障恢复

---

## 1. Runtime Directory & Socket Discovery / 运行时目录与 Socket 机制

Every local socket lives outside the workspace tree in one machine-level runtime directory:
- **Path**: `/tmp/onlyne-<uid>/` (mode `0700`), overridable via `$ONLYNE_RUNTIME_DIR`.
- **Files per daemon**:
  - `<digest>.sock`: Unix domain socket (mode `0600`), where `<digest>` is the first 16 hex chars of the SHA-256 of the canonical root path.
  - `<digest>.json`: Registration metadata (`kind`, `role`, `root`, `pid`, `version`, `runtime`, `placement`).
- **Heartbeat rebind**: Because macOS and some Linux distributions periodically sweep idle files in `/tmp`, both `onlyne-server` and `onlyne-client` verify their socket and registration file on each heartbeat and rebind automatically if swept.
- **PID reuse protection**: `onlyne ls` and `onlyne-server status` verify both `kill -0 <pid>` and the process command line (`ps -p <pid> -o command=`), preventing recycled OS PIDs from reporting a dead daemon as alive.

---

## 2. Running Under `systemd` and `launchd` / 生产环境服务托管

### Linux (`systemd`)
Templates are shipped under `packaging/systemd/`:
- `packaging/systemd/onlyne-server.service`
- `packaging/systemd/onlyne-client@.service`

**Important (`PrivateTmp=false`)**: Both unit files set `PrivateTmp=false` and include `/tmp` in `ReadWritePaths`. Do not set `PrivateTmp=true` unless you also set `Environment=ONLYNE_RUNTIME_DIR=/run/onlyne` across both the service units and operator shells; otherwise systemd places `/tmp/onlyne-<uid>/` in a private mount namespace invisible to `onlyne ls` and `onlyne status`.

### macOS (`launchd`)
Templates are shipped under `packaging/launchd/`:
- `packaging/launchd/se.onlyne.server.plist`
- `packaging/launchd/se.onlyne.client.plist`

Note on Apple Silicon Homebrew installs: Homebrew on Apple Silicon installs binaries under `/opt/homebrew/bin` rather than `/usr/local/bin`; update `ProgramArguments` in the plist accordingly. Because `KeepAlive.SuccessfulExit` is `false`, stopping a launchd-managed daemon with `kill` will cause `launchd` to respawn it—stop it with:

```bash
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/se.onlyne.server.plist
# or request an ordered zero-exit shutdown over the admin socket:
onlyne --server-root /srv/onlyne-server shutdown --drain-secs 5
```

---

## 3. Database Schema Markers & Upgrades / 数据库版本门禁与升级

Onlyne uses explicit schema markers in SQLite (`schema_marker` table) rather than automatic in-place migrations across breaking revisions:
- Server `<server-root>/.onlyne/state.db`: `onlyne-server`, schema version `6`, protocol version `1`.
- Client `<workspace>/.onlyne/client.db`: `onlyne-client`, schema version `3`, protocol version `1`.

When a daemon encounters a database or workspace from an older revision, it exits with code **`6` (`EXIT_NEEDS_MIGRATION`)** and prints the file path, the marker found, and the remedy (drain the cluster, move the old `.db` file aside, and start fresh).

---

## 4. Common Troubleshooting / 常见故障排查

| Symptom / Error | Cause | Fix |
|---|---|---|
| `onlyne: no onlyne socket found` + `hint: server root at <dir> is not running` | The `.onlyne/` tree exists on disk, and its daemon has not been started yet. | Run `onlyne up --server-root <dir>` or `onlyne server run --root <dir>`. |
| `spec.toml:N: backend is gone; v2 splits it into drive and placement` | Legacy v1 `backend = "..."` key in `spec.toml` or workspace `config.toml`. | Put `drive = "plugin" \| "acp" \| "exec"` under `[client.runtime]` in `spec.toml`, and `placement = "orca" \| "zellij" \| "tern" \| "headless" \| "external"` in `<workspace>/.onlyne/config.toml`. |
| `spec.toml:N: relay_required is gone` | Legacy v1 relay keys in `spec.toml` or workspace `config.toml`. | Replace with `allowed_targets = [...]` (reach) and `owes_targets = [...]` (obligation) on `[[client]]` in `spec.toml`. |
| `im_unplaced_delivery` fault in `onlyne faults` | An agent replied to an IM delivery with an unknown or ambiguous `<msg:PREFIX>` tag. | Check `<gateway-root>/conversations/<slug>/chat.jsonl` for valid message IDs; ensure the agent copies `<msg:PREFIX>` verbatim from the inbound delivery. |
| Session stuck in `working` or `blocked` | Agent process hung, or ended two turns without calling `onlyne_complete`. | Run `onlyne --server-root <root> triage --apply` for automatic System-1 recovery, or run `onlyne --server-root <root> repair retry --task <id> --reason "manual retry"`. |
