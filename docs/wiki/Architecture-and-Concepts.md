# Architecture & Concepts / 架构与核心概念

Onlyne is a local-first channel and routing layer for coding agents. It enforces three core principles:
1. **The transport delivers mail; identity belongs to the runtime and the role.** Onlyne injects the rendered delivery, three tool descriptions (`onlyne_send`, `onlyne_handoff`, `onlyne_complete`), and at most one turn-end nudge per delivery.
2. **Every class of fact has one owner.** Delivery state belongs to the server ledger; task verdict and session binding belong to the client; session conversation content belongs to the runtime; role definitions belong to `spec.toml`; placement belongs to the machine running the client.
3. **Declarative constraints are enforced mechanically at one checkpoint.** ACLs (`allowed_targets`), relay obligations (`owes_targets`), and hop budgets (`hop_budget`) are checked at the boundary.

---

## 1. Vocabulary / 核心术语表

| Term | Meaning | 中文释义 |
|---|---|---|
| **role** | One `[[client]]` entry in `<server-root>/.onlyne/spec.toml`: name, public key, ACL, prose, runtime, and session policy. | `spec.toml` 中的一个 `[[client]]` 条目，定义角色名、公钥、ACL、角色提示词与会话策略。 |
| **client** | One `onlyne-client` daemon per role workspace: holds the server TLS link, the local `client.db` session table, and the session backend. | 每个角色工作区运行的一个守护进程，持有与服务端的 TLS 链路、本地会话表及会话驱动。 |
| **runtime** | The program that executes the model conversation (`pi`, an ACP agent, a Hosting Runtime like DSH, or a command). | 实际运行模型对话的程序（如 `pi`、ACP 智能体、DSH 等外部驻留运行时）。 |
| **session** | One conversation inside a runtime; keyed by `session_id` and bound to deliveries via `session_tasks`. | 运行时内部的一段会话；以 `session_id` 为主键，通过 `session_tasks` 表绑定投递任务。 |
| **drive** | How the client communicates with the runtime (`plugin`, `acp`, `exec`). Configured in `spec.toml` `[client.runtime]`. | 客户端与运行时的通信方式（`plugin`、`acp`、`exec`），声明在 `spec.toml` 中。 |
| **placement** | Where the runtime process is displayed on the host machine (`orca`, `zellij`, `tern`, `headless`, `external`). Configured in `<workspace>/.onlyne/config.toml` or `ONLYNE_BACKEND`. | 运行时进程在本机的展示位置（`orca`、`zellij`、`tern`、`headless`、`external`），声明在工作区配置中。 |
| **task family** | A chain of handoffs sharing one root task ID (`causality.family`), bounded by `hop_budget` and reporting to `origin`. | 共享同一个根任务 ID（`causality.family`）的多跳交接链。 |
| **gateway** | The `onlyne-im` host process: connects as one cluster role and serves all IM conversations under `conversations/<slug>/`. | `onlyne-im` 宿主进程，作为集群中的一个角色连接服务端，通过 `<chat:KEY>` 承载多条聊天会话。 |
| **backend** | A platform-facing child process (`onlyne-im-backend-*`) spawned by `onlyne-im` over stdio. | 由 `onlyne-im` 通过 stdio 拉起的平台适配子进程，隔离平台 SDK。 |

---

## 2. Ownership of Facts / 数据所有权

| Fact | Authoritative Owner | Where Else It Appears |
|---|---|---|
| **Delivery state** (`queued` → `in_flight` → `acked`/`rejected`/`expired`) | Server ledger (`<server-root>/.onlyne/state.db`) | Client keeps an outbound intent cache for replay |
| **Task verdict** (`done` / `failed` / `cancelled` / `blocked`) | Client (`<workspace>/.onlyne/client.db`) | Server mirrors it in `sessions` and completion rows |
| **Delivery-to-session binding** | Client (`session_tasks` table) | Server mirrors `session_tasks` for display |
| **Session conversation content** | Runtime (`pi` session store, ACP journal) | Client stores only an opaque reference (`backend_ref`) |
| **Role definition & ACL** | `<server-root>/.onlyne/spec.toml` | Client receives its own role slice in `Welcome` |

---

## 3. Session Scopes (`oneshot` / `task` / `role`) / 会话作用域

Configured per role in `<server-root>/.onlyne/spec.toml`:

```toml
[[client]]
role = "coder"
key = "ed25519/..."
max_sessions = 2

[client.session]
scope = "task"          # oneshot (default) | task | role
idle_close = "2h"
```

| Scope | What One Session Serves | When It Closes | On Restart / Resume |
|---|---|---|---|
| `oneshot` (default) | Exactly one delivery | Immediately when the delivery settles | Unsettled delivery is requeued |
| `task` | Every delivery in the same task family (`causality.family`) addressed to this role | After `idle_close` of inactivity or operator `recycle`/`cancel` | Runs `[client.runtime].resume` (e.g. `pi --session {session}`) to reopen the same conversation |
| `role` | Standing pool of up to `max_sessions` long-lived sessions for the role | After `idle_close` or operator `recycle` | Resumes the pooled conversation where the runtime declares `resume` |

---

## 4. Drive × Placement Matrix / 驱动与放置配对矩阵

`drive` belongs to the role's runtime (`spec.toml`), while `placement` belongs to the machine hosting the workspace (`<workspace>/.onlyne/config.toml` or `ONLYNE_BACKEND`):

| `drive` \ `placement` | `orca` / `zellij` / `tern` | `headless` | `external` |
|---|---|---|---|
| **`plugin`** | Client opens a terminal pane/tab; runtime plugin dials back to client UDS (1 session/process) | Client spawns runtime in background; plugin dials back (1 session/process) | External Hosting Runtime (e.g. DSH) is already resident and dials client UDS (`Capability::Open`, multi-session) |
| **`acp`** | Refused at config load (stdio is used by ACP JSON-RPC) | Client spawns ACP agent as child over stdio and mounts `onlyne mcp` (multi-session) | Refused at config load |
| **`exec`** | Client runs command in pane | Client runs command in background and reads exit code | Client runs command |

---

## 5. Declarative Route Edges: Reach vs. Obligation / 路由可达性与交接义务

In `spec.toml`, reach and obligation are declared separately on each `[[client]]`:

```toml
[[client]]
role = "coder"
key = "ed25519/..."
allowed_senders = ["*", "planner", "reviewer"]
allowed_targets = ["planner", "reviewer"]
owes_targets    = ["reviewer"]
```

- **`allowed_targets`** is the **ACL (reach)**: `coder` may send or hand off to `planner` and `reviewer`.
- **`owes_targets`** is the **obligation**: before a session of `coder` may file a terminal `onlyne_complete`, it must have delivered at least one message or handoff to `reviewer`. If `owes_targets` is omitted, the role owes no outbound relay before completing.

---

## 6. Resident & Pre-Started Agent Handshake ("Session Before Client") / 先起常驻 Agent 再拉起集群

In supervisor or federation workflows, operators often prefer to open an interactive coding agent (`omp` / `pi`) **first**, and start or restart `onlyne up` / `onlyne-client` **later**.

### How It Works

1. **Identity & Predictable Session ID (`residentIdentity`)**:
   `pi-onlyne` enters resident mode without requiring a pre-existing `ONLYNE_TASK_ID` whenever any of the following is present:
   - Environment variable `ONLYNE_ROLE=<role>` (with optional `ONLYNE_SESSION_ID=<alias>` and `ONLYNE_WORKSPACE=<dir>`),
   - `<cwd>/.pi/onlyne.json` declaring `{"role": "<role>", "sessionId": "<alias>", "resident": true}`, or
   - `<workspace>/.onlyne/config.toml` declaring `placement = "external"`, `[client.session] scope = "role"`, or `[client.session] session_id = "<alias>"` (or `session_alias = "<alias>"`).
2. **Deterministic Socket Polling (`resolveResidentSocketPath`)**:
   Before `onlyne-client` starts, `<runtime_dir>/<digest>.json` does not exist yet. `pi-onlyne` computes the deterministic v2 socket path `/tmp/onlyne-<uid>/<workspaceDigest>.sock` (and re-queries `/tmp/onlyne-<uid>/*.json` by `role` on each reconnect tick) while quietly polling in the background (`reconnecting`).
3. **Automatic Handshake & Reuse**:
   As soon as `onlyne up` or `onlyne-client run` binds the workspace socket, the resident `omp`/`pi` session completes `hello`/`welcome` automatically, injects the role prose into its system prompt, and parks in `onlyne-client`. When tasks arrive for that role, `onlyne-client` dispatches them directly to the parked resident session without spawning a duplicate terminal pane, and completing a task keeps the resident agent alive for subsequent deliveries.

### Example Configuration

```toml
# <workspace>/.onlyne/config.toml
role = "supervisor"
placement = "external"

[client.session]
scope = "role"
session_id = "supervisor-main"
```

Or start `omp` / `pi` directly from the shell before starting the cluster:

```bash
# Terminal 1: start resident supervisor agent first
ONLYNE_ROLE=supervisor omp

# Terminal 2: initialize and bring up the cluster later; Terminal 1 connects automatically
onlyne server init --root ./my-cluster --roles supervisor,coder
onlyne up --root ./my-cluster
```
