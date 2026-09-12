# Goal

交付 Onlyne v1.0.0 三进程重构：`onlyne-server` 只投递并持有 ledger，`onlyne-client` 在单个工作区内执行一个 role 的 session，`onlyne-gateway` 负责一个平台侧的翻译；agent 插件与 gateway 插件共用一套 adapter 协议，分挂两种 mount。

详细设计已持久化于 `docs/v1-PLAN.md`；跨 crate 约定见 `docs/v1-CONTRACT.md`；当前验证状态见 `docs/STATUS.md`。上一个长期目标（`onlyne-swarm` 的 session runtime 与调度器重构）的可用内核已按该计划迁入 `crates/onlyne-session/`，其源树随 S12 移除。

## Scope

- `crates/onlyne-proto` 承担全部 wire 类型、`Envelope::validate()` 与错误封闭集；`crates/onlyne-frame` 承担长度前缀编解码。
- `crates/onlyne-net` 承担 pinned TLS、ed25519 hello、ACL 表与重连退避；`crates/onlyne-config`／`onlyne-layout`／`onlyne-store` 承担 spec、工作区布局与 SQLite。
- `crates/onlyne-session` 承担 lifecycle reducer 与 `SessionLedger` 桥接，持久化跨 trait 边界。
- `crates/onlyne-server` 承担 router、relay、session 投影、faults、admin socket 与 `generate`。
- `crates/onlyne-client` 承担 runloop、durable intent、adapter socket、accept 与 dispatch。
- `crates/onlyne-adapter` 与 `crates/onlyne-testkit` 承担插件 SDK、FakeAgent/FakeGateway 与 conformance runner。
- `crates/onlyne-gateway` 与 `plugins/onlyne-gateway-<platform>` 承担 gateway kit 与四平台插件，SDK 由 feature 门控。
- supervisor 用 `onlyne cluster export-prose` 把子层接口交给上层，aggregate role 在父 spec 中就是一个 `[[client]]` 条目。

## Out of scope

- agent runtime、模型适配、prompt orchestration、web UI、cron、TUI。
- 旧布局迁移：v1.0.0 对 legacy `.onlyne/` 以 exit 2 拒绝，不提供迁移路径。
- 真实平台凭据：四平台验证走 `FakeGateway` 与 `check()` dry-run。
- 非本地 channel 的业务语义与远程监督服务。

## Done criteria

- `cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 全绿。
- `cargo tree -p onlyne-client` 不出现 `teloxide`、`openlark`、`wechat-ilink`、`resvg`。
- `docs/v1-PLAN.md` 的 Verification case 1-9 各有可执行证据，脚本位于 `crates/onlyne-testkit/e2e/`。
- 工作区本地不变量成立：config、state、run、logs、history 全部落在 `./.onlyne/`。
- 断连不丢消息：outbound 先落 intent，重连后按序补投；ack 幂等。
- 父层 ledger 只出现父层可见 role，子层 role 名与 prose 不出现在父层 body 与 out_head。
- 生成产物可整体搬迁：产物内不含生成期绝对路径，搬迁后 role 仍能连上并 ack。

## Implementation order

1. proto/frame/net/config/layout/store 基础层。
2. session kernel 迁移与 lifecycle reducer。
3. server 运行时：router、relay、投影、faults、admin。
4. client 运行时：runloop、intent、adapter socket、accept、dispatch。
5. adapter SDK、testkit 与 conformance。
6. gateway kit 与四平台插件（feature 门控）。
7. `generate`、递归集群路径、遗留删除与文档收口。
