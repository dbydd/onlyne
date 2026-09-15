# GOAL — onlyne 1.1.0：headless 后端转正 + Windows 平移，发中版本

## Objective

在 1.0.9 基线上交付 1.1.0：无头（headless）会话后端成为一等公民，整仓向 Windows（x86_64/aarch64 MSVC）平移并挂上 CI，全部六 crate 版本号统一 1.1.0，过发布门。

执行窗口：夜间谷时计价带，子代理大批并行。

## Scope（in）

### P1 headless 补全（2026-09-15 修订：exec.rs 即 headless 后端本体，零新结构体）

实证（`backend/exec.rs:1-29,215-246`）：stdin 常开管道（rpc 模式 EOF 语义已写进注释）、stdout/stderr 并流进 `.onlyne/logs/session-<task>.log`（输出落盘=天然的"环"，比内存环强）、`process_group(0)`、TERM→5s→KILL 收组再收身、持柄 `try_wait` 精确 probe（detail 带 `{"exit": code}`）、`focus=false` 已 unsupported、显式 `ONLYNE_BACKEND=exec` 天然跳宿主探测。**不新造 headless 结构体（AGENTS §11 品味条款），P1 剩余缺口是：**

- config 面：`[client] backend = "exec"` 字段落 `onlyne-config/client.rs`，`runloop.rs:133` 选择链改成 env `ONLYNE_BACKEND` > config.backend > auto；deny_unknown_fields 下新增可选字段，无 marker bump（1.0.9 `stall_report_secs` 先例）。
- 别名：`BackendName::parse`/`backend_by_name` 认 `"headless"`→Exec；`as_str()` 维持 `"exec"`，投影/事件字节不变。
- Windows 杀阶梯（exec.rs 内 cfg 分支，不拆共享函数出去）：spawn 加 `CREATE_NEW_PROCESS_GROUP`；stop = `GenerateConsoleCtrlEvent(CTRL_BREAK)`→grace→`child.kill()`；无柄判活 windows 分支 = `GetExitCodeProcess`（`windows-sys`，与 interprocess 同版本对齐，workspace 依赖）。
- 输出尾上投影：会话退出时 reconcile 读 log 尾部 N 行（常量入 exec.rs）填 `ResourceProbe.detail` 的 `output_tail` 键——proto SessionState 的 detail 是透传 JSON，无 schema 改动。
- 红线：trait、reconcile 判定序、1.0.9 claims/adoption、stall 检测、四个 pane 后端文件零改动；`exec` 语义只加不改（现有 `kill -0`/`process_group` unix 路径逐字节等价）。
- 测试：exec.rs 单测补跨平台用例（sleep 子进程精确退码、log 文件增长、TERM 阶梯 unix-gated）；fake e2e 新场景 `exec-headless.sh`（fake agent 作 session_command：assign→子进程跑→log 断言→exit 码→reconcile 标 exited→report 链路→exit 5 豁免用 config 面）；case4/case15 重跑零回归。
- docs：client README 后端表加 `headless` 别名行 + config 字段；operations 增 headless 运维段（log 路径、TERM 语义、`pi --mode rpc` 示例：stdout 归 log、消息面走 adapter socket）；spec 示例注释一句。

### P2 Windows 平移（2026-09-15 修订：AF_UNIX 前提被编译器驳回，走 GOAL 预埋的 named-pipe fallback）

- 实证：tokio 1.52 `Unix*` = `cfg(all(unix, feature="net"))`，Windows 无此类型；std 同类在 nightly（rust-lang#150487）、mio#1609 未合 → AF_UNIX 在 stable 1.85 不可用。
- 定案：**`interprocess` 2.4.4 `features=["tokio"]` 的 `local_socket`**——Windows byte-mode named pipe，Unix 仍 UDS（`GenericFilePath` + `mode(0o600)` + `reclaim_name(false)`，unlink 归 onlyne）。frame 协议零改动（IO 层已泛型）。全文实证在 local://windows-seam-feasibility.md。
- 落点：a) `onlyne-layout` 新增 `to_local_name` + Windows marker（`.onlyne/run/s` 变普通 marker 文件存 pipe 名；NPFS 名 = `v1:` 派生 `sha256(std::path::absolute 归一化)[..16]hex`，**禁 canonicalize**）；b) `server/admin.rs`、`client/adapter_socket.rs`、`cli/wire.rs`、`gateway/host.rs`、`adapter::connect_unix` 换类型；c) Windows bind 必带 owner-only SDDL `D:P(A;;GA;;;OW)(A;;GA;;;SY)`（默认 DACL 给 Everyone 读=权限回归红线）；d) `apply_private_mode` 非 unix 维持 no-op。
- 其余 cfg 点：
  1. `cli/forward.rs`：`MetadataExt`/`CommandExt::exec` 双实现（Windows=spawn+wait、可执行判定=存在+is_file）。
  2. `orca.rs` 的 `Command::new("sh")` 包壳改 argv 直传；`nix::fs` 一处、`zellij.rs` 的 `MetadataExt` 属主检查：cfg 门。
  3. 测试：`#!/bin/sh` stub 与 `PermissionsExt`/`std::os::unix::net` 断言按 OS 换（`.cmd`/`current_exe --helper` 模式），0600 断言 cfg 门。
  4. `server/cli.rs` 的 `CommandExt`/`kill -KILL`。
- 信号面：`tokio::signal::windows::ctrl_c` 接现有 SIGINT 收尾路径；SIGTERM/SIGHUP 语义在 Windows 以 `onlyne shutdown` / `onlyne reload`（现成 admin 动词）为准，写进文档。
- CI：新建 `.github/workflows/ci.yml`——`ubuntu-latest` + `windows-latest` 双 job：`cargo test -p onlyne-proto -p onlyne-config -p onlyne-net -p onlyne-session -p onlyne-store -p onlyne-frame -p onlyne-adapter -p onlyne-server -p onlyne-client -p onlyne-gateway -p onlyne-layout -p onlyne-cli -p onlyne-tui`；fake e2e 在 windows job 里能跑多少跑多少（headless/cli 面必跑，pane 面按 OS 门）。本地开发环配 `cargo check --target x86_64-pc-windows-msvc`。
- 首验硬点：深层 workspace 路径下双 socket（marker+派生 pipe 名）bind/accept/connect 在 windows-latest 实测通过；`ERROR_PIPE_BUSY` 重试被 `--timeout` 罩住；owner-only SDDL 生效（第二个 Windows 用户/进程连不上，CI 上用 LOCAL SERVICE 或 SID 断言验证）。
- 明确不做：防火墙/端口耗尽类环境问题、Win7/Server2019、MSVC 之外的 windows 工具链、ConPTY（宿主自理）。

### P3 宿主上报映射（机会性，不拦发布）

- `zellij.rs` probe 读 `list-panes` 的 pane 状态/退出码 → 映射 `unknown` 为 `exited(code)`；`herdr.rs`/`orca.rs` 若其 CLI 暴露同等字段则跟进。改动圈在各后端文件内部；宿主不暴露就维持 `unknown`。

### P4 版本与发布

- 六 crate + cli + tui 版本统一 `1.1.0`；CHANGELOG 新段（headless/Windows/CI/宿主映射）。
- 全量门：`cargo test --workspace` 零红、clippy/fmt 绿、fake e2e 全数（含新 headless 场景）、`--target x86_64-pc-windows-msvc` 全仓 check 绿。
- release build + codesign（本机，沿用 ad-hoc 方式）+ 装 `~/.cargo/bin`；crates.io publish 走网络窗口纪律：有界 watcher，先 publish 后 `cargo new` 冷编译消费验证；README/docs 发布记录。

## Out of scope

- web admin、调度器、模型 runtime、workspace 文件同步（AGENTS.md §0）。
- named-pipe 传输（除非深路径 bind 实测失败才议）。
- 任何对 trait/reconcile/claims/stall 的改动。
- Server Core/老 Windows、gnu 工具链。

## Completion criteria

1. headless 后端 e2e 新场景绿，case4/case15 零回归。
2. windows-latest CI job 绿（workspace test 或按 OS 门的等价集合 + 深路径 socket 实测过）。
3. 本机三平台门全绿 + `1.1.0` 六件套发布、装机、冷编译验证。
4. docs：README/CHANGELOG/cli/socket-api/verification 的 Windows 与 headless 口径落齐。

## Contract（给并行 worker 的共享裁定，2026-09-15 修订版）

- backend 正名 `exec`，`"headless"` 为 parse 别名；优先级 env `ONLYNE_BACKEND` > config `[client] backend` > auto。投影/事件里 backend 字符串维持 `"exec"` 字节不变。
- 输出环=现状 log 文件；退出尾部 N 行进 `ResourceProbe.detail.output_tail`（常量 `OUTPUT_TAIL_LINES` 入 exec.rs），proto 零改动。
- 杀阶梯留在 exec.rs 内 cfg 分支（unix 路径逐字节等价）；windows 用 `windows-sys`（0.61，features 只开 Console/Process）：CREATE_NEW_PROCESS_GROUP + CTRL_BREAK→grace→TerminateProcess；无柄判活 = GetExitCodeProcess。
- socket seam：`interprocess = "2.4.4"` features `["tokio"]` 入 workspace.dependencies；Name 派生 + Windows marker + owner-only SDDL `D:P(A;;GA;;;OW)(A;;GA;;;SY)` 全放 `onlyne-layout`（新 `local_socket` 模块）；unix `GenericFilePath`+`mode(0o600)`+`reclaim_name(false)`，unlink 归 onlyne 现有逻辑；`--socket` 以 `\\.\pipe\` 开头原样透传；marker 内容 `v1:<pipe名>`，缺失时双方独立复算（`std::path::absolute` 归一化 sha256 前 16B hex，禁 canonicalize）。
- exit 码/消息字节兼容：2/3/4/5 全保留；`ERROR_PIPE_BUSY` 在 cli `--timeout` 罩内重试。
- 所有权互斥：W1(Headless)=session backend mod/exec、config client.rs、client runloop.rs、testkit e2e 新脚本、client README/operations docs；W2(Windows)=layout、server admin/cli、client adapter_socket/daemon、cli 全 src+tests、adapter lib、gateway host/main/onboarding、net identity/tls、根 Cargo.toml、ci.yml。root Cargo.toml 只有 W2 碰；W1 的 windows-sys 写在 onlyne-session/Cargo.toml 局部。
- 版本 bump 不属于 worker：统一 1.1.0 由 Release 阶段一次做。
