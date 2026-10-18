# Quickstart / 快速上手

This guide walks through the fastest ways to bring up an Onlyne cluster locally, dispatch work, observe it on the terminal board, and shut it down cleanly.

本指南展示在本地拉起 Onlyne 集群、派发任务、在终端看板观测以及优雅排空停机的最短路径。

---

## 1. Two-Command Cluster Bootstrap / 两条命令拉起多角色集群

With `onlyne` installed (or built via `cargo build`), you can initialize a server root together with seeded role workspaces and run the entire cluster in one foreground terminal:

安装或编译 `onlyne` 后，只需两条命令即可一次性生成服务端配置、角色密钥、角色工作区并在前台拉起整套集群：

```bash
# 1. Create ./cluster/.onlyne/spec.toml AND seed ./cluster/planner and ./cluster/coder workspaces
# 1. 创建服务端根并自动在 ./cluster/planner 与 ./cluster/coder 下生成角色密钥与工作区配置
onlyne server init --root ./cluster --roles planner,coder

# 2. Start onlyne-server and all discovered role clients in the foreground
# 2. 在前台一并启动 server 与自动发现的 planner、coder 客户端（Ctrl-C 自动排空停机）
onlyne up --server-root ./cluster
```

What happens under the hood / 背后发生了什么：
1. `onlyne server init --root ./cluster --roles planner,coder` generates the server's self-signed TLS certificate (`server.key`), pins its SPKI digest (`cert_pin`) in `./cluster/.onlyne/spec.toml` (defaulting `--listen` to `127.0.0.1:7811`), creates `./cluster/planner` and `./cluster/coder` with their Ed25519 keys (`role.key`) and `.onlyne/config.toml`, and registers both `[[client]]` blocks in `spec.toml` with mutual `allowed_targets`.
2. `onlyne up --server-root ./cluster` spawns `onlyne-server run`, waits for its admin socket in `/tmp/onlyne-<uid>/` to answer `status`, discovers `./cluster/planner` and `./cluster/coder`, spawns `onlyne-client run` for each, and prefixes all logs with `[server]`, `[client:planner]`, and `[client:coder]`.

---

## 2. Live Hot-Registration of a New Role / 在线热注册新角色

While `onlyne up` (or `onlyne-server run`) is running in one terminal, you can add a new role workspace from another terminal without restarting the server or manually editing `spec.toml`:

当集群正在运行时，可以在另一个终端初始化新角色并传入 `--register`：

```bash
onlyne-client init \
  --workspace ./cluster/reviewer \
  --role reviewer \
  --server-root ./cluster \
  --prose "Review diffs and report blockers." \
  --register
```

- **When the server is online**: `--register` connects to the server's admin socket, runs `SpecGet` + `SpecApply` (`UpsertRole`, `SetTargets`, `SetSenders`, `SetRuntime`), preserves comments in `spec.toml` via `toml_edit`, and hot-reloads the running server immediately (`onlyne-client: role "reviewer" registered on the running server`).
- **When the server is offline**: `--register` appends the generated `[[client]]` block to `<server-root>/.onlyne/spec.toml` so it takes effect on the next start.

---

## 3. Dispatching Work & Observing the Cluster / 派发任务与看板观测

In a second terminal, check online roles, send a task, and open the TUI board:

在第二个终端中查询角色状态、发送任务并打开 TUI 看板：

```bash
# List registered roles and their online state
onlyne --server-root ./cluster roles

# Dispatch a task from the operator surface
onlyne --server-root ./cluster send \
  --from planner --to coder \
  --text "Inspect src/lib.rs and summarize the public API" \
  --force --yes-i-am-supervisor-not-other-role

# Open the interactive 3-page terminal board (or pass --once for a single frame)
onlyne --server-root ./cluster tui
```

Inside an agent session (`pi` or an ACP agent), the model never types `--force --yes-i-am-supervisor-not-other-role`; it calls the three registered tools directly:
- `onlyne_send(to, text, kind?, image?)`
- `onlyne_handoff(to, text, image?)`
- `onlyne_complete(outcome, summary, details?, files?)`

---

## 4. Ordered Shutdown / 优雅排空停机

Either press **Ctrl-C** in the terminal running `onlyne up`, or request an ordered shutdown over the admin socket from any terminal:

在运行 `onlyne up` 的终端按 **Ctrl-C**，或在任意终端通过管理 socket 请求排空停机：

```bash
onlyne --server-root ./cluster shutdown --drain-secs 5 --reason "maintenance"
```

---

## 5. Zero-Model Scripted Quickstart (`fake` backend) / 免模型脚本化自测

If you want to verify the entire delivery → session → completion → ledger loop from a source checkout without invoking an LLM:

如果希望在源码仓库中不配置任何大模型凭据、直接验证端到端投递与结算闭环：

```bash
cargo build -p onlyne-cli -p onlyne-server -p onlyne-client -p onlyne-testkit

tmp=$(mktemp -d)
target/debug/onlyne server init --root "$tmp/cluster" --listen 127.0.0.1:17899 --roles planner
ONLYNE_BACKEND=fake target/debug/onlyne up --server-root "$tmp/cluster" >"$tmp/up.log" 2>&1 &
up_pid=$!

target/debug/onlyne --server-root "$tmp/cluster" wait-ready
target/debug/onlyne-agent-fake \
  --workspace "$tmp/cluster/planner" \
  --script crates/onlyne-testkit/scripts/echo-complete.json \
  >"$tmp/fake.log" 2>&1 &

until target/debug/onlyne --server-root "$tmp/cluster" roles \
  | grep -q '"state":"online"'; do sleep 0.2; done

send=$(target/debug/onlyne --server-root "$tmp/cluster" send \
  --from planner --to planner --text "hello v2" \
  --force --yes-i-am-supervisor-not-other-role)
task=$(printf '%s\n' "$send" | sed -n 's/.*"task":"\([^"]*\)".*/\1/p')

until target/debug/onlyne --server-root "$tmp/cluster" ledger --task "$task" \
  | grep -q '"state":"acked"'; do sleep 0.2; done

target/debug/onlyne --server-root "$tmp/cluster" ledger --task "$task" --pretty
target/debug/onlyne --server-root "$tmp/cluster" shutdown --drain-secs 0
wait "$up_pid" 2>/dev/null || true
rm -rf "$tmp"
```
