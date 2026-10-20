# Examples

The pre-v1 daemon is gone, and its example set went with it. The root `Cargo.toml` is a workspace
manifest, so `cargo run --example <name>` finds nothing to build here.

## End-to-end scripts

| script | covers |
| --- | --- |
| `crates/onlyne-testkit/e2e/local-task.sh` | one server, one role client, one fake agent, one task from send to acked |
| `crates/onlyne-testkit/e2e/two-cluster.sh` | parent server plus child server, with an aggregate role answering across the boundary |
| `crates/onlyne-testkit/e2e/gateway-mount.sh` | fake gateway registration and inbound routing to a role |
| `crates/onlyne-testkit/e2e/idempotency.sh` | duplicate and conflict answers for one repeated `op_id` |
| `crates/onlyne-testkit/e2e/legacy-layout.sh` | refusal of a pre-v1 workspace, exit code 2 |
| `crates/onlyne-testkit/e2e/running-lights.sh` | a six-role ring: a token handed on with `onlyne handoff` twelve times, and two TUI frames of it moving |
| `crates/onlyne-testkit/e2e/im-live.sh` | v2.3 chat addressing on a real platform: one cluster, one gateway root, one real Telegram chat, plus the three refusals |

`crates/onlyne-testkit/e2e/im-live.sh` is the only one that needs a credential (`ONLYNE_TELEGRAM_TOKEN`, `ONLYNE_IM_CHAT_ID`); the rest are hermetic.

`crates/onlyne-testkit/e2e/lib.sh` holds the shared helpers. Callers set `SRC` and `tmp` first. Every
hermetic script in the table above runs with `ONLYNE_BACKEND=fake` and the `fake`
gateway; the live host scripts (`orca-live.sh`, `pi-live.sh`, `handoff-live.sh`) pick their own host.

```bash
cargo build --workspace
cd crates/onlyne-testkit/e2e
bash local-task.sh
```

## The IM gateway cluster

`examples/im-gateway/` lays a minimal cluster down with all three platform backends (Telegram, Feishu, QQ; WeChat is not built — its IoT-derived protocol drops after idle and needs a re-handshake) plus one credential-free fake for a hermetic round trip:

```bash
python3 examples/im-gateway/run.py up       # cluster + gateway root + one conversation per platform
python3 examples/im-gateway/run.py smoke    # a real inbound → reply → outbound cycle, no credential needed
python3 examples/im-gateway/run.py status   # roles, gateway check, each conversation's chat key
python3 examples/im-gateway/run.py stop
```

## The lifecycle and federation cluster

`examples/lifecycle/run.py` builds an isolated copy of this machine's three-root layout. It runs three Onlyne servers, eight clients, a resident adapter host, and an IM gateway with the fake backend. Alexandria and ResearchChamber each have a representative role on the central server and a matching role on the member server. The representative explicitly sends work to the member cluster and returns its result to the central role.

The tree defaults to `/tmp/onlyne-lifecycle`. Its private runtime directory is `/tmp/onlyne-lifecycle/runtime`, so it does not read the machine-wide Onlyne registrations. Set `LIFECYCLE_ROOT` and `LIFECYCLE_RUNTIME_DIR` to select other paths.

```bash
cargo build --workspace
python3 examples/lifecycle/run.py up
python3 examples/lifecycle/run.py status
python3 examples/lifecycle/run.py lifecycle
python3 examples/lifecycle/run.py federation
python3 examples/lifecycle/run.py im
python3 examples/lifecycle/run.py idle
bun examples/lifecycle/omp-live.mjs --paths-json /tmp/onlyne-lifecycle/paths.json
python3 examples/lifecycle/run.py stop
```

`lifecycle` checks which declared adapter connections keep a session alive. `federation` exercises both member clusters and cross-cluster replies. `im` drives an inbound event through the real gateway and fake backend process, then checks the outbound receipt. `idle` records CPU time and loopback traffic over a bounded idle window. `omp-live.mjs` loads the plugin into a real headless OMP parent, creates and disposes a task child, sends notes before and after a parent session replacement, and checks the session row after shutdown. The OMP step uses the configured model route and can take several minutes.

The script writes `paths.json` and evidence under `/tmp/onlyne-lifecycle`. It creates fresh keys and ledgers there. The IM round trip uses a fake backend and does not read platform credentials.

## Configuration example tree

`.onlyne.example/` holds three things: a comment-dense `spec.toml`, a `templates/dev/` tree for the
planner, builder, and reviewer roles, and a README that maps every example file to its real path.

## CLI vocabulary

`AGENTS.md` §5 lists the v2 verbs for each binary: `onlyne-server run|init|status|generate`,
`onlyne-client run|init|status|roles|agent|doctor`, `onlyne send|reply|complete|handoff|ack|reject|control|repair|reload|shutdown|tui|mcp|ls|status|roles|sessions|ledger|faults|watch|history|ghosts|spec_diff|gateway status`, and
`onlyne-im init|generate|login|setup|run|check`.

# 中文

## 示例

pre-v1 守护进程已经移除，随之移除的还有它的示例集。根目录的 `Cargo.toml` 是工作区清单，因此在这里运行 `cargo run --example <name>` 找不到可构建的内容。

## 端到端脚本

| 脚本 | 覆盖内容 |
| --- | --- |
| `crates/onlyne-testkit/e2e/local-task.sh` | 一个服务器、一个角色客户端、一个模拟代理，以及一个从发送到确认的任务 |
| `crates/onlyne-testkit/e2e/two-cluster.sh` | 父服务器和子服务器，并使用一个跨边界应答的聚合角色 |
| `crates/onlyne-testkit/e2e/gateway-mount.sh` | 模拟网关注册，以及到某个角色的入站路由 |
| `crates/onlyne-testkit/e2e/idempotency.sh` | 对一个重复的 `op_id` 给出重复应答和冲突应答 |
| `crates/onlyne-testkit/e2e/legacy-layout.sh` | 拒绝 pre-v1 工作区，退出代码为 2 |
| `crates/onlyne-testkit/e2e/running-lights.sh` | 一个由六个角色组成的环：使用 `onlyne handoff` 传递令牌十二次，以及令牌移动过程中的两个 TUI 帧 |
| `crates/onlyne-testkit/e2e/im-live.sh` | 真实平台上的 v2.3 聊天寻址：一个集群、一个网关根、一个真实 Telegram 会话，以及三类畸形拒绝验证 |

`crates/onlyne-testkit/e2e/im-live.sh` 需要真实凭据（`ONLYNE_TELEGRAM_TOKEN`、`ONLYNE_IM_CHAT_ID`），其余脚本均为本地自含测试。`crates/onlyne-testkit/e2e/lib.sh` 包含共享辅助函数，调用方先设置 `SRC` 和 `tmp`。

```bash
cargo build --workspace
cd crates/onlyne-testkit/e2e
bash local-task.sh
```

## IM 网关集群示例

`examples/im-gateway/` 演示最小集群与平台后端（Telegram、飞书、QQ）及免凭据 `fake` 后端的完整往返：

```bash
python3 examples/im-gateway/run.py up       # 启动集群、网关根并为各平台生成一条会话
python3 examples/im-gateway/run.py smoke    # 免凭据运行一次完整的入站 → 回复 → 出站循环
python3 examples/im-gateway/run.py status   # 查看角色、网关检查结果与每条会话的 chat key
python3 examples/im-gateway/run.py stop
```

## 生命周期与联邦集群

`examples/lifecycle/run.py` 会在独立目录中搭建本机三根目录拓扑的副本。它启动 3 个 Onlyne server、8 个 client、一个常驻 adapter 主机，以及使用 fake 后端的 IM 网关。Alexandria 和 ResearchChamber 在中心 server 各有一个代表 role，并在成员 server 上各有一个同名 role。代表会显式把工作发给成员集群，再把结果带回中心 role。

默认运行目录是 `/tmp/onlyne-lifecycle`，专用运行时目录是 `/tmp/onlyne-lifecycle/runtime`，不会读取机器级 Onlyne 注册文件。设置 `LIFECYCLE_ROOT` 和 `LIFECYCLE_RUNTIME_DIR` 可以选择其他目录。

```bash
cargo build --workspace
python3 examples/lifecycle/run.py up
python3 examples/lifecycle/run.py status
python3 examples/lifecycle/run.py lifecycle
python3 examples/lifecycle/run.py federation
python3 examples/lifecycle/run.py im
python3 examples/lifecycle/run.py idle
bun examples/lifecycle/omp-live.mjs --paths-json /tmp/onlyne-lifecycle/paths.json
python3 examples/lifecycle/run.py stop
```

`lifecycle` 检查连接声明如何保活会话。`federation` 实际经过两个成员集群并跨集群返回结果。`im` 通过真实网关和 fake 后端进程处理入站事件，并检查出站回执。`idle` 在限定时间内记录 CPU 时间和环回流量。`omp-live.mjs` 会在真实的无界面 OMP 父会话中加载插件、创建并回收 task 子会话、在父会话切换前后发送 note，并检查关闭后的会话行。OMP 步骤使用当前配置的模型路由，可能运行几分钟。

脚本会在 `/tmp/onlyne-lifecycle` 写入 `paths.json` 和证据文件，并在其中创建新密钥和账本。IM 往返使用 fake 后端，不读取平台凭据。

## 配置示例目录树

`.onlyne.example/` 包含三项内容：一个注释密集的 `spec.toml`，供规划器、构建器和审查者角色使用的 `templates/dev/` 目录树，以及一个将每个示例文件映射到其真实路径的 README。

## CLI 词汇

`AGENTS.md` §5 列出了每个二进制文件的 v2 动词：`onlyne-server run|init|status|generate`、`onlyne-client run|init|status|roles|agent|doctor`、`onlyne send|reply|complete|handoff|ack|reject|control|repair|reload|shutdown|tui|mcp|ls|status|roles|sessions|ledger|faults|watch|history|ghosts|spec_diff|gateway status`，以及 `onlyne-im init|generate|login|setup|run|check`。
