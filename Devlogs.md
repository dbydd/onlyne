# Development logs

This file records engineering history and release evidence. User-facing installation, operation, and architecture guidance lives in `README.md` and `README.zh-CN.md`. Dated release narratives and historical test readings live in `CHANGELOG.md` and `docs/live-evidence-1.4.0.md`.

## v1.4.1 release window

- The workspace version is 1.4.1 across nineteen Rust crates. Git tag `v1.4.1` points to release commit `b2e0d9c`.
- `v1.4.1` is the first release with a GitHub binary channel. The release pipeline, the installer, and the formula renderer were local files under `.gitignore` until this window, which is why `v1.0.0` through `v1.4.0` carry no release assets.
- The tag's run gated on formatting, clippy, and the workspace suite, then built five binaries for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`, and `x86_64-pc-windows-msvc`, and attached each archive, its `.sha256` file, and the combined `SHA256SUMS` to the release.
- The formula job rendered `packaging/homebrew/onlyne.rb` from that checksum list and committed it to `main` as `packaging: the release formula for v1.4.1`.
- `PREFIX=<dir> sh packaging/install.sh v1.4.1` verified the channel: the script picked the host archive, matched it against the release's own `SHA256SUMS`, and installed five binaries that report 1.4.1.
- The first pipeline runs recorded five runner-shape facts. Cargo takes one `--bin` per invocation. The Windows archive is written by PowerShell, because Git Bash ships no `zip`. macOS ships `shasum` where Linux and Windows ship `sha256sum`. `macos-13` is retired, so the Intel macOS job runs on `macos-15-intel`. `SHA256SUMS` is assembled by each job that reads it, because an artifact does not travel between jobs.
- The npm package `pi-onlyne` is version 1.2.1. The crates.io upload for 1.4.1 has not run; the registry carries 1.4.0.
- The local gate passed: formatting, clippy, and the workspace suite. The Linux and Windows CI jobs are both green on `main`.
- The Windows CI job checks out with the repository's own line endings and skips `a_restart_re_dispatching_a_row_of_its_own_runs_the_task`, the one client scenario that waits for a restarted client to reach its own assignment inside a bound this runner does not meet.

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

### v1.4.1 发布窗口

- workspace version 为 1.4.1，覆盖十九个 Rust crate。Git tag `v1.4.1` 指向 release commit `b2e0d9c`。
- `v1.4.1` 是第一个带 GitHub 二进制渠道的发行版。release pipeline、installer 与 formula renderer 直到本窗口前都是 `.gitignore` 之下的本地文件，因此 `v1.0.0` 到 `v1.4.0` 都没有 release assets。
- tag 触发的运行先过 formatting、clippy 与 workspace suite 的 gate，再为 `aarch64-apple-darwin`、`x86_64-apple-darwin`、`aarch64-unknown-linux-gnu`、`x86_64-unknown-linux-gnu`、`x86_64-pc-windows-msvc` 构建五个二进制，并把每个归档、其 `.sha256` 与合并后的 `SHA256SUMS` 附加到 release。
- formula job 依据该 checksum 列表渲染 `packaging/homebrew/onlyne.rb`，并以 `packaging: the release formula for v1.4.1` 提交到 `main`。
- `PREFIX=<dir> sh packaging/install.sh v1.4.1` 验证了该渠道：脚本挑出本机归档，与 release 自带的 `SHA256SUMS` 比对，装入五个报告 1.4.1 的二进制。
- 前几次 pipeline 运行记录下五条 runner 形态事实：cargo 每次调用只接受一个 `--bin`；Windows 归档由 PowerShell 写出，因为 Git Bash 不带 `zip`；macOS 只有 `shasum`，`sha256sum` 在 Linux 与 Windows 上；`macos-13` 已退役，Intel macOS job 改用 `macos-15-intel`；`SHA256SUMS` 由每个读取它的 job 自行拼出，因为 artifact 不在 job 之间传递。
- npm package `pi-onlyne` 版本为 1.2.1。1.4.1 的 crates.io 上传尚未执行；registry 上仍是 1.4.0。
- local gate 通过：formatting、clippy 和 workspace suite。`main` 上 Linux 与 Windows 两个 CI job 均为 green。
- Windows CI job 以仓库自身的行尾检出，并跳过 `a_restart_re_dispatching_a_row_of_its_own_runs_the_task`：这是唯一一个等待重启后的 client 在自身时限内收到 assignment 的 client scenario，而这个 runner 达不到该时限。

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
