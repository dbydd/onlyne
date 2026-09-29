# Onlyne Status

**What this file is.** Two registers, and the boundary between them is the point of the first
one. The paragraphs under *Release receipts* are a record of what shipped at each v1 tag; they
are history and are not rewritten. Everything under *The tree as it stands* describes `main` as
it is today and is the part that goes stale if a phase lands.

The design of record for the current tree is `AGENTS.md` (the execution contract) and
`docs/v2-PLAN.md` (the settled design). `CHANGELOG.md` and the `docs/v1-*.md` files are the
record of v1 and describe a tree that no longer exists on this branch.

v2 lands in phases, and all four have run. This file still carries no per-slice v2 report:
`AGENTS.md` §0 holds the phase table and the per-slice list, and `docs/v2-REMAINING.md` holds
the open items. `CHANGELOG.md`'s 2.0.0 entry is what an operator has to act on.

## Release receipts

Release `v1.4.1` is commit `b2e0d9c`. Nineteen Rust crates carry version 1.4.1, all nineteen
reached crates.io on 2026-09-26 through `cargo publish --locked` at that tag, and `pi-onlyne`
1.2.2 is published to npm. `v1.4.1` is the first release with a GitHub binary channel:
the tag's run built five binaries for `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`, and `x86_64-pc-windows-msvc`,
attached the archives, their `.sha256` files, and `SHA256SUMS` to the release, and
committed `Formula/onlyne.rb` rendered from that checksum list, which makes this repository the
tap Homebrew reads. The pipeline,
the installer, and the formula renderer lived outside the index before this release,
which is why every earlier tag carries no assets.

Release `v1.4.0` is commit `b3c776ef080d73302267a465c8dbd540321f9adb`. All nineteen Rust crates
are published to crates.io at 1.4.0 and `pi-onlyne` 1.2.0 is published to npm. No v1.4.0
GitHub binary release is published; the registry packages are the v1.4.0 installation channel.
The release rebuilds the client's session
lifecycle around one rule: in plugin mode a session's state comes from the frames the
mounted plugin reports and from heartbeat liveness. Three sources competed before it —
the adapter frames, a backend probe that read a pane or a tab, and stored rows read as
current fact — and a fourth, the client's own clocks, decided death on a schedule no
plugin had witnessed. A plugin beat now moves `agent`, `resource` and `host`, and the
client supplies `delivery`, `recovery` and the reconcile policy with its counters from
its own record before the reducer reads it, so a beat cannot clear an intent the server
has not receipted. The task's result left the session tuple for a `task` table of its
own, so a session row describes a session and the task table answers for a task; the
public lifecycle left the tuple as well and is derived where it is read. `session_sync`
is gone and the client-to-server vocabulary is twelve verbs, with `report`'s heartbeat
variant the only state carrier. Death is one clock with three starts — a session's
birth, a lost connection, a graceful goodbye while work is owed — and the sweep that
reads it takes every session, settles the task a dead session owed, and closes the
resource with the reason that task's state earns. The receipt reaches the reducer at
last, so a completed task exits through `Done` beside `Accepted`, which is the exit the
adapter protocol promises. Two databases move: the client's schema marker to 2 with the
new `task` table, the server's to 4 with `public_lifecycle` dropped and the lifecycle
read out of the stored projection, and a database from the previous layout is refused
with `onlyne: unsupported schema; v1.0.0 does not migrate`.

v1.3.1 is tagged `v1.3.1` (`9d72e50`), nineteen crates at 1.3.1, and it stops two bleedings. A delivery row re-offered for a task this role already finished is now acked where it stands: the client reads the verdict from the task's own record through `DispatchState::task_completed_here` and runs nothing, which closes the path where a requeued row read as new work and the dispatcher's `reuse` branch staged it on whichever session sat idle — one chain's task executing inside another conversation, its second answer aimed at the ledger row the first had settled. `Done` is the outcome that closes the door, so a killed or crashed session leaves its task open for `requeue`, `repair_retry`, and `control retry`. The second stop is the generator: `onlyne server generate` wrote `config.toml` and the key into a fresh workspace and left every template file out, because its write loop asked the overwrite guard's predicate, and that predicate's `false` for a missing path is the guard's correct answer and the writer's inverted one. Nine cases in `crates/onlyne-server/tests/generate.rs` had been red since 1.3.0, where the release check covered `onlyne-client` and `onlyne-config` alone. The full workspace gate is green here at 970 cases across 50 suites with 1 ignored, and no wire type, config key, or generated schema moves.

v1.3.0 (tag `v1.3.0`, `5fadaa8`) shipped nineteen crates at 1.3.0. The release turns the client's promise about a plugin that went away from unbounded to bounded: a connection that ends without a `detach` frame keeps its session for `[client] reconnect_grace_secs`, sixty seconds by default and off at `0`, past which the client retires the task-free session it left behind — the shape that held a role's only capacity slot open for a process that was simply gone. A connection returning for a session a retry already answers is held: it is handed no assignment, what it sends rides that task's closing handoff as one marked message per downstream role, and it is told to leave once the merge is out. The same round deletes `[client.timeout] running_ms` from the parser, the wire, the schema, and the examples, and gives `onlyne server generate` a per-file content guard, so a workspace an operator hand-edited survives a rerun. `CHANGELOG.md` keeps the details under `[1.3.0]`. Earlier releases keep their own receipts in `CHANGELOG.md`. `docs/v1-PLAN.md` is the settled spec. `docs/v1-CONTRACT.md` owns the work split. Root README files are the user manual.

## The tree as it stands

Everything below describes `main` with all four v2 phases landed. Where it disagrees with a
release receipt above, the receipt is right about its own tag and wrong about today.

### Binary shape

Two daemons, the operator entry point, and the optional front end. `AGENTS.md` §5 is the
table of record; this is the same list in prose.

- `onlyne-server` routes envelopes, holds the ledger, mirrors session state, records faults, and
  exposes the admin operations. Subcommands: `init`, `run`, `status`, `generate`, all on one
  server root.
- `onlyne-client` owns one role workspace and its session execution. Subcommands: `init`, `run`,
  `status`, `roles`, `sessions`, `watch`, `history`, `agent`, `doctor`.
- `onlyne` is the operator entrypoint: the queries, the admin verbs, `init` and `generate`, the
  built-in TUI, and the MCP tool bridge for agents. Every verb is implemented in this process.
  The daemons forward nothing — v1 exec'd verbs to sibling binaries and lost the global flags at
  every forwarding point, which is what the merge removed.
- `onlyne-web` is the optional graphical front end: boards, the route graph, and typed spec
  edits over the admin socket. It is built and excluded from the workspace, so the core build
  needs no Node. It has four known protocol gaps for a *hosting* runtime, listed in
  `crates/onlyne-adapter/HOSTING-RUNTIME.md`.

There is no gateway process. The four IM gateway plugins and their shared kit are frozen and
live off this branch; the protocol keeps the `bridge` mount kind for them. Agent plugins and
bridge plugins still speak one adapter protocol over two mount kinds.

Sockets bind in the machine-level runtime directory `/tmp/onlyne-<uid>/` (`ONLYNE_RUNTIME_DIR`
overrides it), as `<digest>.sock` with a `<digest>.json` registration beside it. Nothing binds
inside a workspace tree.

### Crate state

Eleven crates are workspace members, and `onlyne-web` is the twelfth crate, built and kept
outside the workspace so the core build needs no Node.

- [x] `onlyne-proto` — protocol vocabulary, ops, errors, events, and the session reducer. No
  tokio.
- [x] `onlyne-wire` — the frame codec, the one link implementation the server, client, adapter
  SDK, CLI, TUI and web all share, the runtime directory, and the registration files.
- [x] `onlyne-net` — TLS, admission, redial.
- [x] `onlyne-config` — the spec, the client config, the workspace trees, and the templates.
- [x] `onlyne-store` — SQLite persistence, one module each for the server ledger and the client
  database.
- [x] `onlyne-acp` — the ACP v1 client and its stdio transport.
- [x] `onlyne-adapter` — the plugin SDK and the protocol schema.
- [x] `onlyne-server` — the server daemon: router, relay, projection, faults, admin, generate.
- [x] `onlyne-client` — the client daemon: runloop, intents, adapter socket, dispatch, the
  renderer that builds the text a model reads, and the session backends (zellij, Orca, herdr,
  exec, acp, fake, external), selected by the role's `[client.runtime]` drive against the
  workspace's `placement`.
- [x] `onlyne-cli` — the `onlyne` binary: the CLI verbs, the built-in TUI, `mcp`.
- [x] `onlyne-testkit` — the scenario harness, the fake runtime, the conformance fixtures.

The gate reading recorded in the commit receipt for `aebbe11` is a green
`ONLYNE_BACKEND=fake cargo test --workspace` with formatting and clippy clean, in which
`onlyne-client` carries **324 passed, 0 failed, 1 ignored**. Case counts are not maintained here:
they move with every phase, and a figure that ages into a lie is worse than none. The v1 window's
figures (1029, then 1101 across 69 and 68 targets) are a record and live in `Devlogs.md` and
`docs/live-evidence-1.4.0.md`. `cargo test -p <crate>` is what produces one.

Where the v1 crates went, for a reader who knows the old names: `onlyne-frame` is now
`onlyne-wire`, `onlyne-layout` split into `onlyne-config::layout` and `onlyne-wire::socket`,
`onlyne-session`'s reducer is in `onlyne-proto` and its backends are in `onlyne-client`, the TUI
is a module of `onlyne-cli`, and `onlyne-gateway` is frozen.

### Wave plan status

v1's wave plan is closed and stays a record: wave 1 (proto, frame, session kernel,
config/layout/store, net), wave 2 (server runtime, client runtime, adapter SDK plus testkit,
gateway kit), wave 3 (generate, federation path, legacy deletion, docs).

v2 does not use waves. The phase table in `AGENTS.md` §0 is the status of record: all four
phases done, with one phase-two item closed as a named gap rather than finished — a hosting
runtime's protocol, specified and not yet implemented.

### Verification cases

**The scenario suite is the primary body.** `crates/onlyne-testkit/tests/scenarios.rs` is one
test binary driven by `Cluster::start`, which brings up a real server and real clients in a
temporary directory with a fake runtime mounted on a real socket. Eighteen scenarios, seventeen
of them running: the delivery loop, the handoff chain, permissions, idempotency, link-drop
recovery, server restart, large-frame interleaving, legacy-layout refusal, the heartbeat watchdog,
plugin conformance, generate/relocate, and the v2 session cases — `oneshot` giving every delivery
its own session, `task` keying a family to one session, the `role` pool filling to
`max_sessions`, suspension freeing a slot for the same family to resume, a runtime without
`resume` keeping its idle session's process, and a `last_seen`-only heartbeat writing no
projection. `scenario_11_federation` stays `#[ignore]` with its reason on
the attribute — the harness models one server, and the two-root case is the shell script below.
Each scenario names the script it supersedes on the line above its attribute, so the port's
progress is readable from the file itself.

**The shell cases carry what the suite cannot model and the shell reading of the paths it has.**
`crates/onlyne-testkit/e2e/` holds the shell cases beside `lib.sh` and the scripted ACP peer
`acp-agent.py`, run with `ONLYNE_BACKEND=fake BIN_DIR=target/debug` from the repository root. The
shapes the suite does not model fall into three groups:

- *live* — `pi-live.sh`, `orca-live.sh`, `herdr-live.sh`, and `handoff-live.sh`. Each needs a
  real runtime or a model on the host and prints `SKIP` with exit 0 when it is not there, so a
  green line means "passed here" and a skip means "not exercised here".
- *ACP* — `acp-session.sh`, the scripted ACP v1 agent.
- *real-process or two-root shapes the harness does not model* — `requeue-claim.sh` (a server
  taken down with `kill -9` under a live client), `two-cluster.sh` (the two-root federation
  case the ignored scenario points at), `running-lights.sh` (the long one), `gateway-mount.sh`
  (the quick one), `exec-headless.sh`, and `socket-path-length.sh`, which was rewritten for the
  v2 socket move and now pins that a deep root resolves to the same short `<runtime-dir>/<digest>.sock`
  a shallow one does.

The shell reading of a path the suite has absorbed stays beside it: `local-task.sh`,
`acl-reject.sh`, `idempotency.sh`, `reconnect-requeue.sh`, `legacy-layout.sh`,
`generate-relocate.sh`, and `heartbeat-watch.sh`.

`handoff-live.sh` is the one that joined for v2: two real pi sessions in one cluster where the
assigning session hands its task on with its own `onlyne_handoff` tool and the recipient settles
the child itself.

### CI

`.github/workflows/ci.yml` defines two jobs on `push` to `main`, pull requests, and
`workflow_dispatch`.

- `linux` (`ubuntu-latest`): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo test --workspace`. The workspace is the whole matrix here.
- `windows-latest`: `cargo test --no-fail-fast` over `onlyne-proto`, `onlyne-wire`,
  `onlyne-config`, `onlyne-store`, `onlyne-net`, `onlyne-adapter`, `onlyne-server`, and
  `onlyne-cli`, then `onlyne-client` on its own with `--skip
  a_restart_re_dispatching_a_row_of_its_own_runs_the_task`. `onlyne-acp` and `onlyne-testkit`
  are not in the Windows matrix. Two runner-shape steps precede the tests: the checkout turns
  `core.autocrlf` off and lays the tree down from the index, because the cases that hold the
  shipped handbooks equal read the bytes this checkout produced; and the skipped case is the one
  that waits for a restarted client to reach its own assignment inside a bound this runner does
  not meet.

Both jobs are green on `main`.

## 中文状态摘要

本节是上面英文部分的中文镜像，两半必须一致：*发布记录*对应 *Release receipts*，
*当前这棵树*对应 *The tree as it stands*。设计规范是 `AGENTS.md`（执行契约）与
`docs/v2-PLAN.md`（已定设计）；`CHANGELOG.md` 与 `docs/v1-*.md` 是 v1 的记录，
描述的是本分支上已不存在的树。v2 四个阶段都已走完，本文件仍不写逐片阶段报告：阶段表与
逐片已落地清单在 `AGENTS.md` §0，未完成项在 `docs/v2-REMAINING.md`，操作员要动手的部分在
`CHANGELOG.md` 的 2.0.0 条目。

### 发布记录

`v1.4.1` 对应提交 `b2e0d9c`。十九个 Rust crate 为 1.4.1，十九个都已于 2026-09-26 在该 tag 上以 `cargo publish --locked` 到达 crates.io，`pi-onlyne` 1.2.2 已发布到 npm。`v1.4.1` 是第一个带 GitHub 二进制渠道的发行版：tag 触发的运行构建了五个平台的五个二进制，把归档、各自的 `.sha256` 与 `SHA256SUMS` 附加到 release，并按该 checksum 列表提交渲染出的 `Formula/onlyne.rb` —— 这个路径让本仓库就是 Homebrew 读的那个 tap。在此之前 pipeline、installer 与 formula renderer 都在索引之外，因此更早的 tag 都没有 assets。

`v1.4.0` 对应提交 `b3c776ef080d73302267a465c8dbd540321f9adb`。十九个 Rust crate 均以 1.4.0 发布到 crates.io，`pi-onlyne` 1.2.0 已发布到 npm。v1.4.0 未发布 GitHub 二进制发行版；registry 包是 v1.4.0 的安装渠道。客户端会话生命周期以插件上报的 frame 和 heartbeat 存活状态为依据，任务结果进入独立的 `task` 表；`session_sync` 已移除，客户端到服务器的协议包含十二个 verb。客户端数据库 schema marker 为 2，服务器数据库为 4；旧布局会返回 `onlyne: unsupported schema; v1.0.0 does not migrate`。

`v1.3.1` 标记为 `v1.3.1`（`9d72e50`），十九个 crate 版本为 1.3.1。该版本处理已在此角色完成的任务被再次投递的问题，并修复 `onlyne server generate` 漏写模板文件的问题。当时完整工作区检查为 970 cases、50 suites、1 ignored。

`v1.3.0` 标记为 `v1.3.0`（`5fadaa8`），十九个 crate 版本为 1.3.0。它增加 `[client] reconnect_grace_secs`（默认六十秒，设为 `0` 时关闭），支持重连期间保留会话，移除 `[client.timeout] running_ms`，并为 `onlyne server generate` 增加逐文件内容保护。更早版本的记录在 `CHANGELOG.md`，规范见 `docs/v1-PLAN.md`，工作拆分见 `docs/v1-CONTRACT.md`。

### 当前这棵树

以下内容描述 v2 四个阶段全部落地后的 `main`。与上面的发布记录冲突时，发布记录对自己的 tag
是对的，对今天则是错的。

#### 二进制结构

本分支上有两个 daemon、一个操作者入口，以及可选的前端。规范表格见 `AGENTS.md` §5，
这里是同一份列表的文字版。

- `onlyne-server` 路由 envelope、持有 ledger、镜像 session 状态、记录 fault，并暴露 admin 操作。
  子命令为 `init`、`run`、`status`、`generate`，都对应一个 server root。
- `onlyne-client` 拥有一个角色 workspace 及其 session 执行。子命令为 `init`、`run`、`status`、
  `roles`、`sessions`、`watch`、`history`、`agent`、`doctor`。
- `onlyne` 是操作者入口：查询、admin verb、`init` 与 `generate`、内置 TUI，以及给 agent 用的
  MCP 工具桥。所有 verb 都在这个进程内实现，daemon 不再转发任何东西 —— v1 把 verb exec 到
  兄弟二进制，每一处转发都丢掉全局参数，合并就是为了去掉这一层。
- `onlyne-web` 是可选的图形前端：看板、路由图，以及走 admin socket 的带类型 spec 编辑。
  它已构建，并被排除在 workspace 之外，因此核心构建不需要 Node。hosting 运行时所需的四个
  协议缺口列在 `crates/onlyne-adapter/HOSTING-RUNTIME.md`。

本分支没有 gateway 进程。四个 IM gateway plugin 及其共享 kit 已冻结并移出本分支；协议为它们
保留 `bridge` mount kind。Agent plugin 与 bridge plugin 仍通过同一 adapter protocol 使用两种
mount kind。

socket 绑定在机器级运行目录 `/tmp/onlyne-<uid>/`（可由 `ONLYNE_RUNTIME_DIR` 覆盖），
名为 `<digest>.sock`，旁边是 `<digest>.json` 注册文件。workspace 树内不再绑定任何东西。

#### Crate 状态

十一个 crate 是 workspace 成员；`onlyne-web` 是第十二个 crate，已构建，并放在 workspace
之外，好让核心构建不需要 Node。

- `onlyne-proto` —— 协议词汇、ops、errors、events 与 session reducer。不依赖 tokio。
- `onlyne-wire` —— 帧编解码、server/client/adapter SDK/CLI/TUI/web 共用的那一套连接实现、
  运行目录与注册文件。
- `onlyne-net` —— TLS、准入、重连退避。
- `onlyne-config` —— spec、client 配置、workspace 树与模板。
- `onlyne-store` —— SQLite 持久化，server ledger 与 client 库各一个模块。
- `onlyne-acp` —— ACP v1 客户端与其 stdio 传输。
- `onlyne-adapter` —— plugin SDK 与协议 schema。
- `onlyne-server` —— server daemon：router、relay、projection、faults、admin、generate。
- `onlyne-client` —— client daemon：runloop、intents、adapter socket、dispatch、渲染模型读到的那段
  文本的渲染器，以及 session backends（zellij、Orca、herdr、exec、acp、fake、external）——
  由 role 的 `[client.runtime]` drive 与工作区的 `placement` 选定。
- `onlyne-cli` —— `onlyne` 二进制：CLI verb、内置 TUI、`mcp`。
- `onlyne-testkit` —— scenario harness、fake runtime、conformance fixture。

提交 `aebbe11` 的 commit receipt 记录的 gate 读数是 `ONLYNE_BACKEND=fake cargo test --workspace`
全绿、formatting 与 clippy 干净，其中 `onlyne-client` 为 **324 passed、0 failed、1 ignored**。
此处不维护用例数：它随每个阶段变动，一个会过期成谎言的数字比没有数字更糟。v1 窗口的数字（1029，
以及 69 与 68 targets 上的 1101）属于记录，见 `Devlogs.md` 与 `docs/live-evidence-1.4.0.md`；
要得到单个 crate 的数字用 `cargo test -p <crate>`。

给认识旧名字的读者一个去处：`onlyne-frame` 现为 `onlyne-wire`；`onlyne-layout` 拆为
`onlyne-config::layout` 与 `onlyne-wire::socket`；`onlyne-session` 的 reducer 在
`onlyne-proto`、backends 在 `onlyne-client`；TUI 是 `onlyne-cli` 的一个模块；
`onlyne-gateway` 已冻结。

#### Wave plan 状态

v1 的 wave plan 已关闭，并作为记录保留：wave 1（proto、frame、session kernel、
config/layout/store、net），wave 2（server 运行时、client 运行时、adapter SDK 与 testkit、
gateway kit），wave 3（generate、federation 路径、legacy 删除、文档）。

v2 不使用 wave。规范的状态表是 `AGENTS.md` §0 的阶段表：四个阶段都已完成，其中阶段二有
一项以「写明缺口」而非「做完」收口——hosting 运行时的协议，规范已写，实现未做。

#### 验证用例

**scenario suite 是主体。** `crates/onlyne-testkit/tests/scenarios.rs` 是单个测试二进制，
由 `Cluster::start` 驱动：它在临时目录里拉起真实的 server 与真实的 client，并把 fake runtime
挂到真实 socket 上。共十八个场景，十七个在跑：投递环、handoff 链、权限、幂等、断链恢复、
server 重启、大帧交错、legacy 布局拒绝、heartbeat 看门狗、plugin conformance、
generate/relocate，以及 v2 的会话用例 —— `oneshot` 让每次投递各起一个 session、`task` 把一个
家族拴在同一个 session、`role` 池填到 `max_sessions`、挂起腾出槽位后同一家族恢复该 session、
runtime 不支持 `resume` 时空闲 session 的进程保留、只刷新 `last_seen` 的 heartbeat 不写投影。
`scenario_11_federation` 保持 `#[ignore]`，理由写在属性上 —— harness 只建模
一个 server，双根那一例是下面的 shell 脚本。每个场景都在自己属性上方一行写明它取代哪个脚本，
所以迁移进度可以直接从文件本身读出。

**shell 用例承载 suite 建模不了的形态，以及它已吸收路径的 shell 读法。**
`crates/onlyne-testkit/e2e/` 下的 shell 用例连同 `lib.sh` 与脚本化 ACP 对端 `acp-agent.py`，
从仓库根目录以 `ONLYNE_BACKEND=fake BIN_DIR=target/debug` 运行。suite 建模不了的形态分三类：

- *live* —— `pi-live.sh`、`orca-live.sh`、`herdr-live.sh`、`handoff-live.sh`。每个都需要真实
  runtime 或主机上的模型，条件不满足时打印 `SKIP` 并以 0 退出，因此绿色行表示“在这里过了”，
  skip 表示“这里没跑到”。
- *ACP* —— `acp-session.sh`，即脚本化的 ACP v1 agent。
- *harness 建模不了的真实进程或双根形态* —— `requeue-claim.sh`（活 client 下面
  `kill -9` 掉 server）、`two-cluster.sh`（被 ignore 的场景所指向的双根 federation 形态）、
  `running-lights.sh`（长的那个）、`gateway-mount.sh`（短的那个）、`exec-headless.sh`，以及
  `socket-path-length.sh` —— 它已按 v2 socket 迁移重写，现在钉住的是深根解析出与浅根同一条短
  路径 `<runtime-dir>/<digest>.sock`。

suite 已吸收路径的 shell 读法仍留在旁边：`local-task.sh`、`acl-reject.sh`、`idempotency.sh`、
`reconnect-requeue.sh`、`legacy-layout.sh`、`generate-relocate.sh`、`heartbeat-watch.sh`。

`handoff-live.sh` 是 v2 新增的那一个：同一集群里两个真实 pi session，被派活的 session 用自己的
`onlyne_handoff` 工具把任务交出去，接收方自己结算那条 child。

#### CI

`.github/workflows/ci.yml` 在 `push` to `main`、pull requests 和 `workflow_dispatch` 上定义两个 job：

- `linux`（`ubuntu-latest`）：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`。这里整个 workspace 就是矩阵。
- `windows-latest`：对 `onlyne-proto`、`onlyne-wire`、`onlyne-config`、`onlyne-store`、`onlyne-net`、`onlyne-adapter`、`onlyne-server`、`onlyne-cli` 运行 `cargo test --no-fail-fast`，随后单独对 `onlyne-client` 运行 `--skip a_restart_re_dispatching_a_row_of_its_own_runs_the_task`。`onlyne-acp` 与 `onlyne-testkit` 不在 Windows 矩阵里。测试前有两步适配 runner 的形态：checkout 关闭 `core.autocrlf` 并从 index 重新落盘，因为那组“两份手册字节相同”的用例读的正是本次检出的字节；被跳过的用例是唯一一个等待重启后的 client 在自身时限内收到 assignment 的用例，而这个 runner 达不到该时限。

`main` 上两个 job 均为 green。
