# Development logs

This file records engineering history and release evidence. User-facing installation, operation, and architecture guidance lives in `README.md` and `README.zh-CN.md`. Dated release narratives and historical test readings live in `CHANGELOG.md` and `docs/live-evidence-1.4.0.md`.

## v1.4.0 release window

- The workspace version is 1.4.0 across nineteen Rust crates.
- Git tag `v1.4.0` points to release commit `b3c776ef080d73302267a465c8dbd540321f9adb`.
- All nineteen crates are on crates.io at 1.4.0. Each package passed Cargo's packaging sandbox
  build and reached the registry on its first upload attempt, in the order recorded by
  `scripts/publish.py`.
- The npm package `pi-onlyne` is version 1.2.0.
- The GitHub binary-release pipeline did not run; v1.4.0 has no GitHub assets, rendered Homebrew
  formula, or install.sh release channel. The registry packages are the installation channel.
- The local gate passed: formatting, clippy, and workspace tests. The test run recorded 1101 passed, 0 failed, and 1 ignored across 69 targets.
- The Windows CI job on the release commit reports handbook line-ending failures and a restart timing failure. The Linux job is green.
- The shipped handbooks are regular files under `crates/onlyne-cli/skills/`. `onlyne skill export` writes the same bytes into a workspace skill tree.
- The current `onlyne` binary reports version 1.4.0 and protocol 1. The installed daemon binaries
  resolve from `~/.cargo/bin`.

## Product changes recorded during this window

- The client session lifecycle derives state from plugin reports, heartbeat liveness, durable delivery records, and reconnect policy.
- `reuse` and `session_sync` were removed. Each task owns a session, and the client-to-server vocabulary has twelve verbs.
- `onlyne_handoff{to, text, image}` continues a task family from a plugin session. `onlyne_send{kind:"task"}` starts a new family.
- Task causality carries `family`, `hop_budget`, `origin`, `deadline`, and `labels`.
- `requeue_ttl_secs` can settle returned in-flight rows and never-pulled deliverable rows whose
  recipient role has no live connection as `expired` with reason `requeue_ttl`. The default value
  0 leaves indefinite queueing unchanged.
- The TUI accepts `--once --state active|all`; `a` toggles the global view; detail views accept `Home`, `End`, and `G`.
- The seven supervisor verbs require `--force` and `--yes-i-am-supervisor-not-other-role` before opening a socket.
- The pi adapter carries the handoff tool and assignment hop information.

## Evidence

The live acceptance record is `docs/live-evidence-1.4.0.md`. It contains the readings behind the release notes and the field observations that shaped the fixes. The historical release ledger is `CHANGELOG.md`.

## 中文摘要

以上英文内容是规范文本；本节提供当前开发与发布状态的中文镜像。

### v1.4.0 发布窗口

- workspace version 为 1.4.0，覆盖十九个 Rust crate。
- Git tag `v1.4.0` 指向 release commit `b3c776ef080d73302267a465c8dbd540321f9adb`。
- 十九个 crate 均以 1.4.0 发布到 crates.io；每个 package 均通过 Cargo packaging sandbox build，并在首次上传时到达 registry，顺序记录于 `scripts/publish.py`。
- npm package `pi-onlyne` 版本为 1.2.0。
- GitHub binary-release pipeline 未运行；v1.4.0 没有 GitHub assets、渲染后的 Homebrew formula 或 `install.sh` release channel。registry packages 是安装渠道。
- local gate 通过：formatting、clippy 和 workspace tests。测试记录为 1101 passed、0 failed、1 ignored、69 targets。
- release commit 的 Windows CI job 报告 handbook line-ending failures 和 restart timing failure；Linux job 为 green。
- shipped handbooks 是 `crates/onlyne-cli/skills/` 下的 regular files。`onlyne skill export` 将相同字节写入 workspace skill tree。
- 当前 `onlyne` binary 报告 version 1.4.0、protocol 1；已安装 daemon binaries 从 `~/.cargo/bin` 解析。

### 本发布窗口记录的产品变更

- 客户端 session lifecycle 由 plugin reports、heartbeat liveness、durable delivery records 和 reconnect policy 推导状态。
- `reuse` 和 `session_sync` 已移除。每个 task 拥有 session，客户端到服务器协议包含十二个 verb。
- `onlyne_handoff{to, text, image}` 从 plugin session 延续 task family；`onlyne_send{kind:"task"}` 启动新 family。
- Task causality 包含 `family`、`hop_budget`、`origin`、`deadline` 和 `labels`。
- `requeue_ttl_secs` 可将已返回的 in-flight row，以及 recipient role 没有 live connection 的 never-pulled deliverable row，结算为 reason 为 `requeue_ttl` 的 `expired`。默认值 0 保持无限排队行为不变。
- TUI 接受 `--once --state active|all`；`a` 切换 global view；detail view 接受 `Home`、`End` 和 `G`。
- 七个 supervisor verb 在打开 socket 前要求 `--force` 和 `--yes-i-am-supervisor-not-other-role`。
- pi adapter 携带 handoff tool 和 assignment hop information。

### 证据

live acceptance record 为 `docs/live-evidence-1.4.0.md`，包含 release notes 背后的 readings 与塑造修复的现场观察。historical release ledger 为 `CHANGELOG.md`。
