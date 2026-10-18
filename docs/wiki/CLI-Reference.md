# CLI & Exit Codes Reference / 命令与退出码速查

Onlyne ships four core/gateway binaries plus an optional web board and five platform backends.

---

## 1. Binaries Overview / 二进制概览

| Binary | Responsibility | Subcommands |
|---|---|---|
| `onlyne` | Operator entrypoint, admin queries, cluster lifecycle (`up`, `shutdown`, `triage`, `repair`), TUI board, and MCP bridge | `up`, `shutdown`, `triage`, `server`, `client`, `gateway`, `send`, `reply`, `complete`, `handoff`, `ack`, `reject`, `control`, `repair`, `status`, `roles`, `sessions`, `ledger`, `faults`, `watch`, `history`, `ghosts`, `spec_diff`, `reload`, `generate`, `wait-ready`, `ls`, `who`, `ping`, `tui`, `mcp`, `skill`, `version`, `schema`, `completions` |
| `onlyne-server` | Routing daemon and server-root file operations | `init`, `run`, `status`, `generate` |
| `onlyne-client` | Role workspace daemon and local workspace operations | `init`, `run`, `status`, `roles`, `agent`, `doctor` |
| `onlyne-im` | Optional IM gateway host process | `init`, `generate`, `login`, `setup`, `run`, `check` |
| `onlyne-web` | Optional graphical web board (excluded from core Cargo workspace) | `--bind <addr>`, `--open` |

---

## 2. Cluster Lifecycle Commands / 集群生命周期命令

```bash
# Seed a server root (--listen defaults to 127.0.0.1:7811) and optionally bootstrap role workspaces
onlyne server init --root ./cluster [--listen 127.0.0.1:7811] [--roles planner,coder] [--force]

# Run the server, discovered role clients, and optional IM gateway in the foreground
onlyne up --server-root ./cluster [--ws <dir>...] [--im-root <dir>] [--drain-secs 2]

# Start individual daemons in the foreground
onlyne server run --root ./cluster
onlyne client run --workspace ./cluster/planner

# Initialize a new role workspace and hot-register it (uses SpecApply if server is live, or appends to spec.toml if offline)
onlyne-client init --workspace ./cluster/reviewer --role reviewer --server-root ./cluster --register

# Reload spec.toml on a running server
onlyne --server-root ./cluster reload

# Request an ordered server shutdown over the admin socket
onlyne --server-root ./cluster shutdown --drain-secs 5 --reason "operator shutdown"
```

---

## 3. Observation & Inspection Commands / 状态观测与查询

```bash
# List all registered Onlyne sockets and PIDs on this machine (/tmp/onlyne-<uid>/)
onlyne ls [--json]

# Server health, connected role count, and event head
onlyne --server-root ./cluster status

# Role registry, presence, session counts, and runtime config
onlyne --server-root ./cluster roles [--role <name>]

# Session projections (--fresh sends a live probe first)
onlyne --server-root ./cluster sessions [--task <task-id>] [--role <name>] [--fresh]

# Durable delivery ledger rows
onlyne --server-root ./cluster ledger [--task <task-id>] [--state queued|in_flight|acked|rejected|expired]

# Open or historical fault rows
onlyne --server-root ./cluster faults [--open-only] [--task <task-id>] [--role <name>]

# Live event stream (durable, advisory, or all)
onlyne --server-root ./cluster watch [--follow] [--tier durable|advisory|all]

# Interactive 3-page terminal board (or single text snapshot with --once)
onlyne --server-root ./cluster tui [--once]
```

---

## 4. Messaging & The Supervisor Gate / 消息投递与 Supervisor 门禁

Seven verbs speak on a role's behalf from the shell: `send`, `reply`, `handoff`, `complete`, `ack`, `reject`, and `control`. Calling them from the CLI requires both attestation flags:

```text
--force --yes-i-am-supervisor-not-other-role
```

```bash
# Dispatch a root task with a hop budget and labels
onlyne --server-root ./cluster send \
  --from _supervisor --to planner \
  --text "Design the migration plan" \
  --hop-budget 6 --label env=prod \
  --force --yes-i-am-supervisor-not-other-role

# Control a running session (probe, snapshot, focus, recycle, cancel)
onlyne --server-root ./cluster control cancel \
  --from _supervisor --to planner --task <task-id> \
  --reason "superseded by new requirement" \
  --force --yes-i-am-supervisor-not-other-role
```

Inside a session (`pi` plugin or `onlyne mcp` for ACP), agents call `onlyne_send`, `onlyne_handoff`, and `onlyne_complete` without CLI flags.

---

## 5. Fault Triage & Repair / 故障自动分诊与人工恢复

```bash
# System-1 autonomic fault triage (evaluates open faults or a hook event via POST /v1/systemone)
onlyne --server-root ./cluster triage [--task <task-id>] [--apply] [--min-confidence 0.60]

# Operator repair verbs
onlyne --server-root ./cluster repair inspect --task <task-id>
onlyne --server-root ./cluster repair retry   --task <task-id> --reason "transient network failure"
onlyne --server-root ./cluster repair fail    --task <task-id> --reason "invalid input"
onlyne --server-root ./cluster repair close   --task <task-id> --reason "resolved manually"
onlyne --server-root ./cluster repair adopt   --task <task-id> --backend headless --reason "re-adopt"
onlyne --server-root ./cluster repair rebind  --task <task-id> --session-id <sid> --backend headless --reason "rebind"
onlyne --server-root ./cluster repair ack     --fault-id <id> --reason "acknowledged"
```

---

## 6. Exit Codes & Socket Resolution / 退出码与 Socket 解析顺序

| Exit Code | Constant | Meaning |
|---|---|---|
| `0` | `EXIT_OK` | Command succeeded |
| `1` | `EXIT_ANSWER_FAILED` | Daemon answered with an error, or runtime execution failed |
| `2` | `EXIT_VALIDATION` | Local flag/argument validation failure or missing supervisor gate flag |
| `3` | `EXIT_NO_SOCKET` | No active socket found (prints an unstarted-tree hint if `.onlyne/` exists locally) |
| `4` | `EXIT_REFUSAL` | Operator input or template generation refused (e.g. existing file without `--force`) |
| `5` | `EXIT_NO_BACKEND` | `onlyne-client run` received an unknown `ONLYNE_BACKEND` placement name |
| `6` | `EXIT_NEEDS_MIGRATION` | Database (`state.db` / `client.db`) or workspace layout is from an older revision |
| `127` | `EXIT_NO_SIBLING` | Required sibling daemon binary (`onlyne-server`, `onlyne-client`, `onlyne-im`) not found |

### Socket Resolution Precedence

1. `--socket <path>`
2. `ONLYNE_SOCKET` environment variable (injected into every spawned agent session)
3. `--server-root <dir>` → resolves `<runtime-dir>/<sha256-prefix-16>.sock`
4. `--workspace <dir>` or upward walk from current working directory for `.onlyne/`
