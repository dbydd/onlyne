# onlyne-testkit

The test kit provides three fixtures for adapter protocol conformance: `HostSim`, `FakeAgent`, and `FakeGateway`.

## Fake agent script

`onlyne-agent-fake` reads one JSON object, either from `--script FILE` or, with `--stdin-script`, from standard input. The script shape is:

```json
{"hello":{"capabilities":["register","report","inject","recycle"]},"steps":[{"wait_assign":true},{"assert_prose_equals":"<prose from the role spec entry>"},{"report":"ready"},{"report":"heartbeat"},{"complete":{"outcome":"done","head_from":"assign_body"}},{"echo_prose_to":"prose.log"}]}
```

Supported steps are `wait_assign`, `report` (`ready` or `heartbeat`), `complete`, `fail`, `exit`, `sleep_ms`, `assert_prose_equals`, `assert_field`, and `echo_prose_to`. An unknown step fails with a message naming the step. `--capabilities` takes a comma-separated capability list and overrides the script hello list. `--workspace DIR` resolves the adapter socket through the owner tree — `DIR/.onlyne/run/s` for a workspace short enough to serve from the canonical path, and the short path recorded in `DIR/.onlyne/run/socket` for a deeper one — and the mount role from `DIR/.onlyne/config.toml`. `--socket PATH` overrides the socket, and `--role NAME` overrides the role. `--once` exits after the script completes.

One process serves one session. The client hands the plugin that mounts naming no session the next session it stages, and that connection then serves that session alone — a task redelivered later is handed to a session of its own, and a session no process ever mounted is left for the grace sweep — so a case that stages several sessions starts one `onlyne-agent-fake` per session.

## Fake gateway

`onlyne-gateway-fake --platform fake --gateway-id fg1 --socket PATH` connects as a gateway mount. The binary prints each host `render_send` as `{"op":"rendered","conversation":...,"text":...,"has_image":...}`. A stdin line `{"op":"inbound","conversation":"c1","text":"hello"}` sends a `deliver` frame with `Principal::Gateway` as its sender.

The binary prints one `rendered` line per host `render_send`, and `gateway-mount.sh` asserts on that output. Inbound `deliver` frames travel from the fake gateway to the server. The e2e route sends a `Task` to the conversation and asserts that the rendered reply line appears.

The three-way conformance fixture uses the testkit stub because `onlyne-testkit` does not depend on `onlyne-session`.

## Backend choice

The three-way fixture uses the testkit stub backend because only the local crate may declare the `onlyne-session` dependency.

## E2E

Scripts live in `e2e/`. Sixteen scripts sit beside `lib.sh`. Fake-backend cases run with `ONLYNE_BACKEND=fake BIN_DIR=target/debug` from the repository root and currently pass 13/13 (cases 1-7, 9, 12, 14, 15, 16, 17). Case 16 `exec-headless.sh` covers the exec backend: workspace `backend = "headless"`, env `ONLYNE_BACKEND=exec`, session log, and `client.db` backend byte `exec`. Case 17 `socket-path-length.sh` covers the deep-workspace socket: a padded workspace past the 103-byte bound, the short served path published in `run/socket`, the canonical path left bare, and one task settled end to end through the marker.

---

# 中文说明（Chinese Translation）

测试工具包为适配器协议一致性提供三个夹具：`HostSim`、`FakeAgent` 和 `FakeGateway`。

## 假代理脚本

`onlyne-agent-fake` 会读取一个 JSON 对象，来源可以是 `--script FILE`，也可以在使用 `--stdin-script` 时从标准输入读取。脚本结构如下：

```json
{"hello":{"capabilities":["register","report","inject","recycle"]},"steps":[{"wait_assign":true},{"assert_prose_equals":"<prose from the role spec entry>"},{"report":"ready"},{"report":"heartbeat"},{"complete":{"outcome":"done","head_from":"assign_body"}},{"echo_prose_to":"prose.log"}]}
```

支持的步骤包括 `wait_assign`、`report`（`ready` 或 `heartbeat`）、`complete`、`fail`、`exit`、`sleep_ms`、`assert_prose_equals`、`assert_field` 和 `echo_prose_to`。遇到未知步骤时，执行会失败并给出一条指明该步骤名称的消息。`--capabilities` 接受以逗号分隔的能力列表，并覆盖脚本 hello 中的能力列表。`--workspace DIR` 通过所有者树解析适配器套接字：如果工作区足够短，可使用规范路径 `DIR/.onlyne/run/s`；如果路径更深，则使用 `DIR/.onlyne/run/socket` 中记录的短路径；并从 `DIR/.onlyne/config.toml` 读取挂载角色。`--socket PATH` 覆盖套接字，`--role NAME` 覆盖角色。`--once` 会在脚本完成后退出。

一个进程服务一个会话。如果客户端交给挂载插件的插件未指定会话名称，客户端会交出它接下来准备的会话；该连接随后仅为这个会话服务。之后重新投递的任务会被交给它自己的会话，而从未被任何进程挂载的会话会保留给宽限期清扫。因此，一个准备多个会话的用例需要为每个会话启动一个 `onlyne-agent-fake`。

## 假网关

`onlyne-gateway-fake --platform fake --gateway-id fg1 --socket PATH` 会以网关挂载的方式连接。该二进制文件会把每个宿主 `render_send` 打印为 `{"op":"rendered","conversation":...,"text":...,"has_image":...}`。标准输入中的一行 `{"op":"inbound","conversation":"c1","text":"hello"}` 会发送一个 `deliver` 帧，其发送者为 `Principal::Gateway`。

该二进制文件会为每个宿主 `render_send` 打印一行 `rendered`，`gateway-mount.sh` 会对该输出进行断言。传入的 `deliver` 帧从假网关传送到服务器。e2e 路由向该会话发送一个 `Task`，并断言渲染后的回复行会出现。

三方一致性夹具使用测试工具包的桩，因为 `onlyne-testkit` 不依赖 `onlyne-session`。

## 后端选择

三方夹具使用测试工具包的桩后端，因为只有本地 crate 可以声明 `onlyne-session` 依赖。

## E2E

脚本位于 `e2e/`。十六个脚本与 `lib.sh` 放在一起。假后端用例从仓库根目录使用 `ONLYNE_BACKEND=fake BIN_DIR=target/debug` 运行，目前通过 13/13（用例 1-7、9、12、14、15、16、17）。用例 16 `exec-headless.sh` 覆盖 exec 后端：工作区 `backend = "headless"`、环境变量 `ONLYNE_BACKEND=exec`、会话日志，以及 `client.db` 中值为 `exec` 的后端字节。用例 17 `socket-path-length.sh` 覆盖深层工作区套接字：长度超过 103 字节界限的填充工作区、发布在 `run/socket` 中的简短服务路径、未被使用的规范路径，以及一个通过标记端到端完成的任务。
