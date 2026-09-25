# Onlyne Status

Release `v1.4.1` is commit `b2e0d9c`. Nineteen Rust crates carry version 1.4.1 and
`pi-onlyne` 1.2.1 is published to npm; the crates.io upload for 1.4.1 has not run, so the
registry still carries 1.4.0. `v1.4.1` is the first release with a GitHub binary channel:
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

## Three-process shape

- `onlyne-server` routes envelopes and holds the ledger.
- `onlyne-client` owns one role workspace and its session execution.
- `onlyne-gateway` translates one chat platform through a feature-gated plugin.
- Agent plugins and gateway plugins use one adapter protocol over two mount kinds.

## Crate state

Counts come from the release-window verification recorded in `Devlogs.md`: 1101 passed, 0 failed,
and 1 ignored across 69 targets. The release commit's Linux gate is green. Each line below covers
one crate, with its libraries and integration targets summed. The 2026-09-26 redundancy pass
removed 92 cases from the tree after that reading; the current tree's gate records 1029 passed,
0 failed, and 1 ignored across 68 targets.

- [x] `onlyne-proto` green with envelope, frame variants, ops, errors, events, and the payload-v2 report grammar: 76 unit + 5 wire vectors (86 fixtures) + 2 sizes, where a wire vector is a recorded protocol fixture.
- [x] `onlyne-acp` green with the ACP v1 client, its stdio transport, and the protocol fixtures: 40 unit + 14 scripted-peer + 1 doc example.
- [x] `onlyne-frame` green with length-prefixed codec: 9.
- [x] `onlyne-config` green with spec parse and reload: 11 template + 39 config contract + 17 ACL table + 3 spec example.
- [x] `onlyne-layout` green with legacy refusal exit 2, the local-socket seam, and the one spelling of the per-task report, log, events, and content-index paths: 30.
- [x] `onlyne-store` green with ledger and local DB: 31 unit + 2 schema statements.
- [x] `onlyne-session` green with the lifecycle port and the session backends (zellij, Orca, exec, acp, fake, herdr): 131 unit + 18 herdr.
- [x] `onlyne-net` green with TLS, handshake, ACL, and backoff: 25.
- [x] `onlyne-adapter` green with SDK and protocol schema: 5 unit + 3 conformance + 1 protocol doc.
- [x] `onlyne-server` green with router, relay, projection, faults, admin, and generate: 14 unit + 84 delivery + 29 generate + 2 stale.
- [x] `onlyne-client` green with runloop, intents, adapter socket, host detection, dispatch, secret resolution at launch, the plugin config repair, the reconnect grace, the held connection, and the redelivery guard: 82 unit + 1 binary + 53 scenarios + 4 init-template.
- [x] `onlyne-tui` green with the role network graph, the swarm monitor, and the key table: 74 unit + 2 one-shot snapshots.
- [x] `onlyne-gateway` green with shared kit: 47.
- [x] `onlyne-cli` green with entrypoint, socket resolution, the admin verbs, the local config surface, and the `report` family: 8 unit + 40 cli + 8 report.
- [x] `onlyne-testkit` green with fake agent, fake gateway, and conformance: 3 unit + 2 binaries + 11 conformance.
- [x] Four gateway plugins green behind `telegram`, `feishu`, `qqbot`, and `weixin` features: 11, 10, 10, 13.

## Wave plan status

- [x] Wave 1 closed: proto, frame, session kernel, config/layout/store, net.
- [x] Wave 2 closed: server runtime, client runtime, adapter SDK plus testkit, gateway kit.
- [x] Wave 3 closed: generate, federation path, legacy deletion, docs.

## Verification cases

Each case is a script under `crates/onlyne-testkit/e2e/`, run with `ONLYNE_BACKEND=fake BIN_DIR=target/debug` from the repository root. The directory holds eighteen case scripts besides `lib.sh`, the shared harness, and `acp-agent.py`, the scripted ACP peer cases 18 and 19 drive. The 2026-09-19 sweep is historical. The final release-window record in `docs/live-evidence-1.4.0.md` reports 19/19 scripts green; the release commit's local workspace gate in `Devlogs.md` reports 1101 passed, 0 failed, and 1 ignored across 69 targets, and the tree after the 2026-09-26 redundancy pass reports 1029 passed, 0 failed, and 1 ignored across 68 targets. `running-lights` is the long one, and `gateway-mount` the quick one. Case 16 `exec-headless` joined the set for 1.1.0, and case 17 `socket-path-length` joined on 2026-09-17 as the field fix for the deep-workspace socket. Case 18 `acp-session` joins as the ACP backend proof: a real server and client against a scripted ACP agent as the role's `session_command`, so the case stands apart from the fake-backend set and from the live set. Case 19 `acp-payload-v2` joins beside it as the closing-report proof, where the closing report is the one file a settled task leaves. One acp role authors its report through the shipped `onlyne report` verbs, and a fake role receives what the client routes. The case asserts the routed child rows' `parent_task`, `hop + 1` (one hop deeper, where a hop is one step along the chain of handed-on tasks), literal `handoff: ` body prefix, and completion receipts. It then covers the three endings that must invent nothing: a relay (one handoff the client sends) to a role the ACL cannot reach records `handoff_denied` and creates no task; a `hop-blocked:` report settles failed with zero relays; and a malformed report cancels its turn while leaving the file for a rewrite that then checks valid. On a host without a live-case requirement, that case prints `SKIP` and exits 0, so a green line says "passed here" and a skip says "not exercised here".

- [x] Case 1 `local-task.sh`: single-machine fake-backend task reaches `acked`.
- [x] Case 2 `acl-reject.sh`: ACL refusal emits `acl_denied`.
- [x] Case 3 `idempotency.sh`: a repeated `op_id` emits `duplicate`. A changed body emits `conflict`.
- [x] Case 4 `reconnect-requeue.sh`: disconnect keeps queue state and reconnect flushes in order.
- [x] Case 5 `two-cluster.sh`: aggregate-role federation preserves the parent ledger boundary.
- [x] Case 6 `gateway-mount.sh`: gateway mount delivers platform traffic.
- [x] Case 7 `legacy-layout.sh`: legacy workspace exits 2.
- [x] Case 8: formatting, lint, workspace tests, and binary firewall checks pass. `.github/workflows/ci.yml` splits this across two jobs: Linux fmt/clippy/`cargo test --workspace`; Windows `cargo test` on the core crate subset. Both jobs are green on `main`. The Windows job checks out with the repository's own line endings and leaves `a_restart_re_dispatching_a_row_of_its_own_runs_the_task` out by name.
- [x] Case 9 `generate-relocate.sh`: generate produces relocatable workspaces.
- [x] Case 10 `orca-live.sh`: the Orca backend against the live app. Under the `host` policy the tab lands flat in the supervisor's own worktree. Probe inputs come in four parts, and the tab map carries that identity. SIGTERM drains back to the tab count it started with, and no Orca registration is created.
- [x] Case 11 `pi-live.sh`: the `plugins/onlyne-agent-pi` pi extension against a real `onlyne-client`. `ONLYNE_BACKEND=exec` spawns the workspace's `session_command` as a child of the client with stdin held open. pi loads the plugin, and the task text reaches pi's context. The plugin's `report.complete` then settles the ledger to `acked` with the model's answer in `out_head`, and the session projects `exited`/`done`. SKIP semantics: pi not on PATH, or a one-turn credential probe that does not answer, prints `SKIP pi-live` and exits 0. A host without a model must not read as a product failure. The plugin's own protocol path is covered without a model by `node --test` in `plugins/onlyne-agent-pi`: framing, protocol vocabulary, the agent state machine against a fake host, and a live handshake against a really-running `onlyne-client`.
- [x] Case 12 `running-lights.sh`: six roles in a closed ring, one token. A `send` starts it on `light1`. Each role runs `onlyne handoff` to pass a child task to its neighbour and settles its own. The agent that meets hop 11 keeps the task and does not pass it on, so the ledger ends with twelve `task` rows all `acked` at hops `0..11`, each naming its parent, plus the twelve `completion` receipts. `onlyne-tui --page 2 --once` is sampled mid-run: two frames three seconds apart put the working session on different roles. Each sighting pairs the role's `working` row with the hop it travels on in its `in-flight` cell, and with the same `in_flight` edge in the ledger.
- [x] Case 13 `herdr-live.sh`: the herdr backend against the live herdr session `onlyne-test`. `ONLYNE_BACKEND=herdr` with `HERDR_ENV=1` and `HERDR_SESSION` set, `session_command = ["sleep", "600"]` on the pane-run track, `max_sessions` left at the seed value `1` so the role is saturated by its own session. The case polls `herdr workspace list` `.result.workspaces[]` for the label built from the spec it generated (`onlyne:<[server] name>`, the value the client passes to each pane as `ONLYNE_CLUSTER`). It then polls `herdr tab list --workspace W` `.result.tabs[]` for tab label `planner`, then that tab's `pane_count` reaching 2 with the two ids from `herdr pane list` `.result.panes[]`. The 1-pane moment is left unasserted: the split lands within a few hundred ms of the tab's creation, so the poller would race the product. `backend_ref` from `client.db` `sessions` names the workspace, the tab, and the session's pane; the pane in that tab beside it is the tab's root. Then `onlyne control --from planner focus --task` (the saturated-role delivery `pull{control_only}` is what carries it) and `herdr pane get` `.result.pane.focused` to true. The next step reads the session pane's shell pid through `herdr pane process-info` with `kill -0` proving that pid live, then drives `onlyne control --from planner recycle --task`, and waits for the reclaim: `herdr pane list` drops that pane id and `kill -0` finds the recorded pid gone. `pane get` on the closed id answers rc 1 with `pane_not_found`, `pane_count` is back to 1, and the surviving pane equals the recorded root. SIGTERM then drains the client with that session's host resource already released. `workspace close` returns the session's workspace list to the pre-run snapshot, and cleanup closes any workspace that appeared after that snapshot, so a failing assertion leaves nothing behind. SKIP: missing herdr binary or an unreachable `HERDR_SESSION` prints `SKIP herdr-live` and exits 0.
- [x] Case 14 `heartbeat-watch.sh`: the server's own heartbeat watch against real processes. The spec's `[server]` carries `stale_watch_secs = 2` and `heartbeat_grace_secs = 4`, a scripted agent reports `ready`, lands one heartbeat, and then sleeps inside the assignment. The `sessions` answer reads `working` with `heartbeat_stale` absent while beats flow, the `faults` table gains `heartbeat_missing` for the task once the grace passes, and the same row keeps `working` all the way: the server flags, the supervisor decides.
- [x] Case 15 `requeue-claim.sh`: the hello claim across a server restart on real processes. A task sits `in_flight` in a live client session when `kill -9` takes the server; the restarted server answers `wait-ready`, the client reconnects and declares its live slot at `hello`. The row keeps `in_flight` through adoption, the task's ledger arc counts one delivery event and zero requeues, and the sessions axis keeps exactly one `working` row. The scripted completion lands the same row `acked`.
- [x] Case 16 `exec-headless.sh`: the exec backend with workspace `backend = "headless"` (parse alias) and env `ONLYNE_BACKEND=exec`. Fake agent as `session_command` writes a banner into `.onlyne/logs/session-<task>.log`, the ledger settles `acked`, the session projects `exited`/`done`, and `client.db` stores backend `exec`.
- [x] Case 17 `socket-path-length.sh`: the deep-workspace socket. The case pads a workspace path until the canonical `<workspace>/.onlyne/run/s` spelling passes 103 bytes, then runs the client and the fake agent there. It asserts the served path is short, published in `run/socket`, and holding a bound socket, with the canonical path left bare and the client log naming the served path; `onlyne --workspace <deep ws> who` answers `planner` through the marker, and one task settles `acked` with the session `exited`/`done`.
- [x] Case 18 `acp-session.sh`: the ACP session backend against a scripted ACP v1 agent the case installs as the role's `session_command`. The workspace config names `backend = "acp"` plus an `[acp]` table whose mode, model and reasoning effort the agent's own trace proves it received. The agent holds its one turn at a gate, so while it is held the case asserts the journal carries the client's dispatch record alone. Releasing the gate settles the ledger `acked` and the session `exited`/`done`, and leaves the events journal, the content index, and the rendered log in agreement — one turn journalled three ways under `.onlyne/logs/`, with the ACP child gone once its client drains.
- [x] Case 19 `acp-payload-v2.sh`: the closing report and its routing, end to end. An acp role writes its report through `onlyne report write`, the case checks it through `onlyne report check`, and the scripted agent leaves the file to its caller, so the report stays the only settlement input. A done report with two handoff lines settles the turn `acked`, and it creates two child tasks naming the settled task as `parent_task` at `hop + 1`, with bodies carrying the literal `handoff: ` prefix. Each child files its own completion receipt. A handoff naming a role the ACL cannot reach settles the turn as written, records `handoff_denied` with the settled task and the refused role, and creates no child row. A `hop-blocked:` report settles `failed` with its own reason and routes nothing. A malformed report costs the turn its cancellation and stays on disk, where `report write` replaces it and `report check` then reads it clean.

## CI

`.github/workflows/ci.yml` defines two jobs on `push` to `main`, pull requests, and `workflow_dispatch`.

- `linux` (`ubuntu-latest`): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- `windows-latest`: `cargo test --no-fail-fast` on `onlyne-proto`, `onlyne-frame`, `onlyne-config`, `onlyne-layout`, `onlyne-store`, `onlyne-session`, `onlyne-net`, `onlyne-adapter`, `onlyne-server`, `onlyne-client`, `onlyne-gateway`, `onlyne-cli`, `onlyne-tui`, then `onlyne-client` on its own with `--skip a_restart_re_dispatching_a_row_of_its_own_runs_the_task`. Two runner-shape steps precede the tests: the checkout turns `core.autocrlf` off and lays the tree down from the index, because the cases that hold the shipped handbooks equal read the bytes this checkout produced; and the skipped case is the one that waits for a restarted client to reach its own assignment inside a bound this runner does not meet. Both jobs are green on `main`.

## 中文状态摘要

以上英文内容是规范文本；本节提供当前运行事实的中文镜像。

### 发布与历史状态

`v1.4.1` 对应提交 `b2e0d9c`。十九个 Rust crate 为 1.4.1，`pi-onlyne` 1.2.1 已发布到 npm；1.4.1 的 crates.io 上传尚未执行，registry 上仍是 1.4.0。`v1.4.1` 是第一个带 GitHub 二进制渠道的发行版：tag 触发的运行构建了五个平台的五个二进制，把归档、各自的 `.sha256` 与 `SHA256SUMS` 附加到 release，并按该 checksum 列表提交渲染出的 `Formula/onlyne.rb` —— 这个路径让本仓库就是 Homebrew 读的那个 tap。在此之前 pipeline、installer 与 formula renderer 都在索引之外，因此更早的 tag 都没有 assets。

`v1.4.0` 对应提交 `b3c776ef080d73302267a465c8dbd540321f9adb`。十九个 Rust crate 均以 1.4.0 发布到 crates.io，`pi-onlyne` 1.2.0 已发布到 npm。v1.4.0 未发布 GitHub 二进制发行版；registry 包是 v1.4.0 的安装渠道。客户端会话生命周期以插件上报的 frame 和 heartbeat 存活状态为依据，任务结果进入独立的 `task` 表；`session_sync` 已移除，客户端到服务器的协议包含十二个 verb。客户端数据库 schema marker 为 2，服务器数据库为 4；旧布局会返回 `onlyne: unsupported schema; v1.0.0 does not migrate`。

`v1.3.1` 标记为 `v1.3.1`（`9d72e50`），十九个 crate 版本为 1.3.1。该版本处理已在此角色完成的任务被再次投递的问题，并修复 `onlyne server generate` 漏写模板文件的问题。当时完整工作区检查为 970 cases、50 suites、1 ignored。

`v1.3.0` 标记为 `v1.3.0`（`5fadaa8`），十九个 crate 版本为 1.3.0。它增加 `[client] reconnect_grace_secs`（默认六十秒，设为 `0` 时关闭），支持重连期间保留会话，移除 `[client.timeout] running_ms`，并为 `onlyne server generate` 增加逐文件内容保护。更早版本的记录在 `CHANGELOG.md`，规范见 `docs/v1-PLAN.md`，工作拆分见 `docs/v1-CONTRACT.md`。

### 三进程结构

- `onlyne-server` 路由 envelope 并持有 ledger。
- `onlyne-client` 拥有一个角色 workspace 及其 session 执行。
- `onlyne-gateway` 通过 feature-gated plugin 转换一个聊天平台。
- Agent plugin 与 gateway plugin 通过同一 adapter protocol 使用两种 mount kind。

### Crate 状态

`Devlogs.md` 记录的 release-window 检查为：1101 passed、0 failed、1 ignored，分布于 69 targets。release commit 的 Linux gate 为 green。2026-09-26 的冗余清理在该读数之后从 tree 中删除 92 个用例；当前 tree 的 gate 记录为 1029 passed、0 failed、1 ignored，分布于 68 targets。

- `onlyne-proto`：76 unit + 5 wire vectors（86 fixtures）+ 2 sizes。
- `onlyne-acp`：40 unit + 14 scripted-peer + 1 doc example。
- `onlyne-frame`：9。
- `onlyne-config`：11 template + 39 config contract + 17 ACL table + 3 spec example。
- `onlyne-layout`：30。
- `onlyne-store`：31 unit + 2 schema statements。
- `onlyne-session`：131 unit + 18 herdr。
- `onlyne-net`：25。
- `onlyne-adapter`：5 unit + 3 conformance + 1 protocol doc。
- `onlyne-server`：14 unit + 84 delivery + 29 generate + 2 stale。
- `onlyne-client`：82 unit + 1 binary + 53 scenarios + 4 init-template。
- `onlyne-tui`：74 unit + 2 one-shot snapshots。
- `onlyne-gateway`：47。
- `onlyne-cli`：8 unit + 40 cli + 8 report。
- `onlyne-testkit`：3 unit + 2 binaries + 11 conformance。
- 四个 gateway plugin：11、10、10、13，分别对应 `telegram`、`feishu`、`qqbot`、`weixin`。

Wave 1、Wave 2、Wave 3 均已关闭。

### 验证用例

用例位于 `crates/onlyne-testkit/e2e/`，从仓库根目录以 `ONLYNE_BACKEND=fake BIN_DIR=target/debug` 运行。2026-09-19 的 sweep 属于历史记录；`docs/live-evidence-1.4.0.md` 的最终 release-window 记录为 19/19 scripts green，release commit 的本地 workspace gate 为 1101 passed、0 failed、1 ignored、69 targets，2026-09-26 冗余清理之后的 tree 为 1029 passed、0 failed、1 ignored、68 targets。缺少所需 live 条件时，相应 case 打印 `SKIP` 并以 0 退出。

1. `local-task.sh`：单机 fake-backend task 到达 `acked`。
2. `acl-reject.sh`：ACL 拒绝产生 `acl_denied`。
3. `idempotency.sh`：重复 `op_id` 产生 `duplicate`，请求体变化产生 `conflict`。
4. `reconnect-requeue.sh`：断线保留队列状态，重连后按序 flush。
5. `two-cluster.sh`：aggregate-role federation 保持父 ledger 边界。
6. `gateway-mount.sh`：gateway mount 投递平台流量。
7. `legacy-layout.sh`：legacy workspace 以 2 退出。
8. 格式、lint、workspace tests 与 binary firewall 检查通过。`main` 上两个 job 均为 green。Windows job 以仓库自身的行尾检出，并按名字跳过 `a_restart_re_dispatching_a_row_of_its_own_runs_the_task`。
9. `generate-relocate.sh`：generate 生成可重定位 workspace。
10. `orca-live.sh`：Orca backend 对接 live app；`host` policy 下 tab 位于 supervisor 自身 worktree，SIGTERM 后恢复原 tab 数，且不创建 Orca registration。
11. `pi-live.sh`：`plugins/onlyne-agent-pi` 对接真实 `onlyne-client`，任务进入 pi context，`report.complete` 将 ledger 结算为 `acked`。pi 不在 PATH 或 credential probe 无响应时打印 `SKIP pi-live` 并以 0 退出；无模型的主机不视为产品失败。插件协议路径由 `plugins/onlyne-agent-pi` 中的 `node --test` 覆盖。
12. `running-lights.sh`：六个角色组成闭环，一个 token 从 `light1` 开始，最终在 hops `0..11` 产生十二条 `acked` `task` row 和十二条 `completion` receipt；`onlyne-tui --page 2 --once` 采样显示工作 session 在不同角色间移动。
13. `herdr-live.sh`：herdr backend 对接 live session `onlyne-test`，验证 workspace/tab/pane、focus、recycle、资源释放与清理。缺少 herdr binary 或 `HERDR_SESSION` 不可达时打印 `SKIP herdr-live` 并以 0 退出。
14. `heartbeat-watch.sh`：验证 `stale_watch_secs = 2`、`heartbeat_grace_secs = 4` 与 `heartbeat_missing` fault；server 标记，supervisor 决定。
15. `requeue-claim.sh`：server 被 `kill -9` 后，client 重连并在 `hello` 声明 live slot；任务保持一次 delivery、零 requeue，最终 `acked`。
16. `exec-headless.sh`：验证 `backend = "headless"`、`ONLYNE_BACKEND=exec`、日志写入、ledger 结算及 `client.db` 中的 backend `exec`。
17. `socket-path-length.sh`：验证深 workspace 下 `<workspace>/.onlyne/run/s` 超过 103 bytes 时使用短 socket，`onlyne --workspace <deep ws> who` 可用。
18. `acp-session.sh`：ACP v1 scripted agent 验证 mode、model、reasoning effort、日志一致性以及 `acked`、`exited`/`done` 状态。
19. `acp-payload-v2.sh`：验证 `onlyne report write`/`onlyne report check`、child task 的 `parent_task`、`hop + 1`、`handoff: ` 前缀、completion receipt、`handoff_denied`、`hop-blocked:` 与 malformed report 行为。

### CI

`.github/workflows/ci.yml` 在 `push` to `main`、pull requests 和 `workflow_dispatch` 上定义两个 job：

- `linux`（`ubuntu-latest`）：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`。
- `windows-latest`：对 `onlyne-proto`、`onlyne-frame`、`onlyne-config`、`onlyne-layout`、`onlyne-store`、`onlyne-session`、`onlyne-net`、`onlyne-adapter`、`onlyne-server`、`onlyne-client`、`onlyne-gateway`、`onlyne-cli`、`onlyne-tui` 运行 `cargo test --no-fail-fast`，随后单独对 `onlyne-client` 运行 `--skip a_restart_re_dispatching_a_row_of_its_own_runs_the_task`。测试前有两步适配 runner 的形态：checkout 关闭 `core.autocrlf` 并从 index 重新落盘，因为那组“两份手册字节相同”的用例读的正是本次检出的字节；被跳过的用例是唯一一个等待重启后的 client 在自身时限内收到 assignment 的用例，而这个 runner 达不到该时限。`main` 上两个 job 均为 green。
