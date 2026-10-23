# onlyne-testkit

The test kit provides three fixtures for adapter protocol conformance: `HostSim`, `FakeAgent`, and `FakeGateway`.

## Fake agent script

`onlyne-agent-fake` reads one JSON object, either from `--script FILE` or, with `--stdin-script`, from standard input. The script shape is:

```json
{"hello":{"capabilities":["register","report","inject","recycle"]},"steps":[{"wait_assign":true},{"assert_prose_equals":"<prose from the role spec entry>"},{"report":"ready"},{"report":"heartbeat"},{"complete":{"outcome":"done","head_from":"assign_body"}},{"echo_prose_to":"prose.log"}]}
```

Supported steps are `wait_assign`, `report` (`ready` or `heartbeat`), `complete`, `fail`, `exit`, `sleep_ms`, `assert_prose_equals`, `assert_field`, and `echo_prose_to`. An unknown step fails with a message naming the step. `--capabilities` takes a comma-separated capability list and overrides the script hello list. `--workspace DIR` derives the adapter socket from the workspace root alone — `<runtime-dir>/<digest>.sock`, where the runtime directory is `/tmp/onlyne-<uid>/` unless `$ONLYNE_RUNTIME_DIR` replaces it, and `<digest>` is the first 16 hex characters of `sha256` over the canonical root — and reads the mount role from `DIR/.onlyne/config.toml`. There is no length rule and nothing is bound in the tree. `--socket PATH` overrides the socket, and `--role NAME` overrides the role. `--once` exits after the script completes.

One process serves every session it is given, not one. A plugin that mounts naming no session is handed the next session the role stages, and a later staging is handed to the same connection: a task redelivered later gets a session of its own, and a session no process ever mounted is left for the grace sweep. A case that wants one agent per session should say so by scripting one, because the client no longer forces it.

A plugin that declares `open` in its hello is the other shape entirely: it is never handed a session. It registers, and each session the role opens goes to it as an `open` request it has to answer — see `crates/onlyne-adapter/HOSTING-RUNTIME.md`. Two script steps exist for that: `mark_mounted` writes a file so a case can wait for the mount, and `require_opened` fails the script when the client never asked, which is the only way a case can tell an asked-for session from a handed-over one.

Two script preconditions the client enforces, both of which now fail loudly instead of hanging. A script whose first step is `wait_assign` must declare the `inject` capability in its hello. And a script that reaches `complete` must have reported a heartbeat first: the client records a turn only from a heartbeat whose agent phase reads `running`, so a completion for a session that never ran one is refused whole (`settle_without_turn`) while the task stays open. The refusal names the step to add.

## Fake gateway

`onlyne-gateway-fake --platform fake --gateway-id fg1 --socket PATH` connects as a gateway mount. The binary prints each host `render_send` as `{"op":"rendered","conversation":...,"text":...,"has_image":...}`. A stdin line `{"op":"inbound","conversation":"c1","text":"hello"}` sends a `deliver` frame with `Principal::Gateway` as its sender.

The binary prints one `rendered` line per host `render_send`, and `gateway-mount.sh` asserts on that output. Inbound `deliver` frames travel from the fake gateway to the server. The e2e route sends a `Task` to the conversation and asserts that the rendered reply line appears.

The three-way conformance fixture uses the testkit stub because the session backends live in `onlyne-client` and `onlyne-testkit` does not depend on it.

## Backend choice

The three-way fixture uses the testkit stub backend because only `onlyne-client` may declare a session backend, and a leaf crate does not take a dependency on a sibling daemon.

## E2E

Scripts live in `e2e/`. Sixteen scripts sit beside `lib.sh`. `lib.sh` holds the shared helpers, including the one place a script derives a socket path: `runtime_socket <root>` and `runtime_registration <root>` compute `<runtime-dir>/<digest>.sock` and `<runtime-dir>/<digest>.json` exactly the way `onlyne_wire::socket` does, and `wait_for_socket <root>` polls the first of them. No script spells `.onlyne/run/s` or reads a `run/socket` marker, because v2 creates neither.

Fake-backend cases run with `ONLYNE_BACKEND=fake BIN_DIR=target/debug` from the repository root. Case 16 `exec-headless.sh` covers the exec backend: workspace `placement = "headless"`, env `ONLYNE_BACKEND=exec`, session log, and `client.db` backend byte `exec`. Case 17 `socket-path-length.sh` covers the deep-workspace socket. The v1 rule it used to pin is gone — v1 bound `<workspace>/.onlyne/run/s` while it fit `sun_path` and moved a deeper tree to a short derived path recorded in `run/socket` — so the case now pins the v2 invariant in its place: a workspace padded far past the old 103-byte bound still serves one short `<runtime-dir>/<digest>.sock`, its `<digest>.json` registration is named for that root's digest and names the same canonical root, kind, and role, nothing exists under `<workspace>/.onlyne/run/`, and one task settles end to end.

---

# 中文说明（Chinese Translation）

测试工具包为适配器协议一致性提供三个夹具：`HostSim`、`FakeAgent` 和 `FakeGateway`。

## 假代理脚本

`onlyne-agent-fake` 会读取一个 JSON 对象，来源可以是 `--script FILE`，也可以在使用 `--stdin-script` 时从标准输入读取。脚本结构如下：

```json
{"hello":{"capabilities":["register","report","inject","recycle"]},"steps":[{"wait_assign":true},{"assert_prose_equals":"<prose from the role spec entry>"},{"report":"ready"},{"report":"heartbeat"},{"complete":{"outcome":"done","head_from":"assign_body"}},{"echo_prose_to":"prose.log"}]}
```

支持的步骤包括 `wait_assign`、`report`（`ready` 或 `heartbeat`）、`complete`、`fail`、`exit`、`sleep_ms`、`assert_prose_equals`、`assert_field` 和 `echo_prose_to`。遇到未知步骤时，执行会失败并给出一条指明该步骤名称的消息。`--capabilities` 接受以逗号分隔的能力列表，并覆盖脚本 hello 中的能力列表。`--workspace DIR` 仅从工作区根路径推导适配器套接字——`<runtime-dir>/<digest>.sock`，其中运行时目录默认为 `/tmp/onlyne-<uid>/`，除非 `$ONLYNE_RUNTIME_DIR` 替换它，`<digest>` 是规范根路径 `sha256` 的前 16 个十六进制字符——并从 `DIR/.onlyne/config.toml` 读取挂载角色。不存在路径长度规则，目录树中也不绑定任何东西。`--socket PATH` 覆盖套接字，`--role NAME` 覆盖角色。`--once` 会在脚本完成后退出。

一个进程服务的是递给它的每个会话，不是一个。未指定会话名称就挂载的插件，会被递上这个角色接下来暂存的会话；之后再暂存的会话仍然交给同一条连接。之后重新投递的任务会拿到自己的会话，而从未被任何进程挂载的会话保留给宽限期清扫。需要「一个代理一个会话」的用例应当用脚本明说，因为 client 不再强制这一点。

在 hello 里声明 `open` 的插件是完全另一种形状：它永远不会被递交会话。它只登记，
这个角色要开的每个会话都以 `open` 请求送到它面前，由它作答——见
`crates/onlyne-adapter/HOSTING-RUNTIME.md`。脚本为此有两个步骤：`mark_mounted`
写一个文件，让用例可以等挂载落地；`require_opened` 在 client 从没问过时让脚本失
败，这是用例唯一能分辨「被问到的会话」与「被递交的会话」的办法。

client 强制两条脚本前提，现在两者都会直接大声失败，而不是挂住。首个步骤为
`wait_assign` 的脚本必须在 hello 中声明 `inject` 能力。而走到 `complete` 的脚本
必须先上报过一次 heartbeat：client 只从 agent phase 读作 `running` 的 heartbeat
记录一轮，因此对从未跑过一轮的 session，completion 会被整条拒绝
（`settle_without_turn`），同时任务保持打开。拒绝文案会点名该补上哪一步。

## 假网关

`onlyne-gateway-fake --platform fake --gateway-id fg1 --socket PATH` 会以网关挂载的方式连接。该二进制文件会把每个宿主 `render_send` 打印为 `{"op":"rendered","conversation":...,"text":...,"has_image":...}`。标准输入中的一行 `{"op":"inbound","conversation":"c1","text":"hello"}` 会发送一个 `deliver` 帧，其发送者为 `Principal::Gateway`。

该二进制文件会为每个宿主 `render_send` 打印一行 `rendered`，`gateway-mount.sh` 会对该输出进行断言。传入的 `deliver` 帧从假网关传送到服务器。e2e 路由向该会话发送一个 `Task`，并断言渲染后的回复行会出现。

三方一致性夹具使用测试工具包的桩，因为 session backend 现在位于 `onlyne-client`，而 `onlyne-testkit` 不依赖它。

## 后端选择

三方夹具使用测试工具包的桩后端，因为只有 `onlyne-client` 可以声明 session backend，而叶子 crate 不依赖兄弟 daemon。

## E2E

脚本位于 `e2e/`。十六个脚本与 `lib.sh` 放在一起。`lib.sh` 保存共享辅助函数，其中包括脚本推导 socket 路径的唯一位置：`runtime_socket <root>` 和 `runtime_registration <root>` 完全按照 `onlyne_wire::socket` 的方式计算 `<runtime-dir>/<digest>.sock` 与 `<runtime-dir>/<digest>.json`，`wait_for_socket <root>` 轮询前者。没有任何脚本会拼出 `.onlyne/run/s` 或读取 `run/socket` 标记，因为 v2 两者都不创建。

假后端用例从仓库根目录使用 `ONLYNE_BACKEND=fake BIN_DIR=target/debug` 运行。用例 16 `exec-headless.sh` 覆盖 exec 后端：工作区 `placement = "headless"`、环境变量 `ONLYNE_BACKEND=exec`、会话日志，以及 `client.db` 中值为 `exec` 的后端字节。用例 17 `socket-path-length.sh` 覆盖深层工作区套接字。它原先钉住的 v1 规则已经不存在——v1 在规范路径 `<workspace>/.onlyne/run/s` 满足 `sun_path` 时直接绑定，更深的树则改用记录在 `run/socket` 中的短派生路径——因此该用例改为钉住取代它的 v2 不变式：远超出旧 103 字节界限的填充工作区仍然只服务一个短小的 `<runtime-dir>/<digest>.sock`，它的 `<digest>.json` 注册文件以该根路径的摘要命名，并指明同一个规范根路径、kind 和角色，`<workspace>/.onlyne/run/` 下不存在任何东西，且有一个任务端到端结清。
