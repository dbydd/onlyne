# GOAL — onlyne：payload-v2 结项报告收口为 v1.2.2 发布

## Objective

在 `v1.2.1`（tag `38aece8`，工作树基线 `86d34ff`）之上交付下一轮的代码面：agent 写的那一份结项文件从「一行结论」扩成「一行结论 + 至多八行转手」，读它的文法收在一个模块里，写它的人可以在停手前自己校验，client 把转手线沿该 role 已有的连接送出去。附带把「查文档才能知道的东西」搬进 `--help` 与 fragment 注释。

本轮把这些面收进一次发布：工作区版本 `1.2.1` → `1.2.2`，CHANGELOG 的对应节从开发中标题改为带日期的发布标题，提交、打 tag、发 crates.io、刷新本机安装。

## Scope（in）

### P1 文法单一权威（proto）

- `crates/onlyne-proto/src/payload.rs`：`parse` 把一个报告文件读成 `PayloadV2::{Done, Failed, Blocked, Invalid}`，`Handoff::text_or` 给转手线兜底正文，`GRAMMAR_V2` 是 prompt 与 CLI help 共用的文法全文，`MAX_REPORT_LINES = 16`、`MAX_REPORT_HANDOFFS = 8`。
- 前缀：`hop-done:` / `hop-failed:` / `hop-blocked:` / `handoff: <role>[ | <一行>]`；`#` 注释行与空行跳过；CRLF 与孤立 CR 在分类前归一；行号按物理行报；读不到的文件一律 `Invalid` 且零转手（fail closed）。
- 17 条内联用例（`TestPayload`）。

### P2 acp 后端读报告（session）

- `emit_handoffs` 先于 `remove_file`；`Invalid` 不删文件，重写即可消费。
- `head_kind` 把「agent 自报阻塞」与断链分开：`hop-blocked:` → `Outcome::Failed` + note，`Invalid` → `Cancelled`。
- `payload` journal 记录带 `handoffs`，被拒时带 `error`；每条待路由的转手线另记 `handoff` 记录。

### P3 client 路由（handoff）

- `crates/onlyne-client/src/handoff.rs`：转手 = 现有 `send` 通路上的 `MsgKind::Task`，`parent_task` 指向本轮，`hop + 1`，正文前缀常量 `RELAY_BODY_PREFIX = "handoff: "`，整组 6 秒预算。
- `acl_denied` / `unknown_role` 判 `NetError::Rejected`：写 client.db 的 `handoff_denied` events 行 + 首条同名 fault 行，verdict 与 outcome kind 保持原样；链路本身无应答时回落 durable intents（与其余出站帧同路）。
- 消费点在 `dispatch.rs on_out`；`SessionSlot.hop` / `hop_of()` 提供 hop 继承。

### P4 本地校验动词族（cli）

- `onlyne report path | check | write | validate`，`crates/onlyne-cli/src/report.rs`。只读写工作区文件，零 socket。
- 退出码形状：0 成功 / 2 无效、缺失、读不到、参数被拒 / 1 io 失败；3 留给 socket 解析。
- 8 条集成用例跑真二进制（`tests/cli_report.rs`）。

### P5 可发现性

- `onlyne-client init` fragment 三组注释（`BACKEND_COMMENTS` / `ACP_COMMENTS` / `KNOB_COMMENTS`），默认值取自 parser 的 `Default`。
- `admin.rs` 七个 repair 动词的 doc + 两段 `after_help`；`NO_SUPPORTED_HOST` 三行点名 `[acp]` 键；`BACKEND_NAMES` 常量，`unknown session backend` 带 accepted 列表。
- 文档：`README.md` / `README.zh-CN.md`、`docs/v1-CONTRACT.md`、`v1-ARCHITECTURE.md`、`v1-PLAN.md`、`docs/operations.md`（payload-v2 节、journal 字段、`onlyne report` 动词表、退出码）、`skills/onlyne-role/SKILL.md`。
- `template.rs` 新增 `NoRoleMatches` + `available_roles()`；`relay.rs` 四处键集提示共用一常量。

### P6 路径拼写上提到 layout

- `CONTENT_INDEX_FILE_NAME` 与 `RoleWorkspace::{out_dir, report_path, session_log_path, session_events_path, content_index_path}`（`onlyne-layout/src/lib.rs`）。
- `acp.rs`、`exec.rs`、`content.rs`、`report.rs` 改走 accessor；两处副本常量删除。

### P7 client 配置格式宽松化

- `local_cli.rs`：`is_plugin_table_header` 容忍 `[[ plugin ]]` 与表头行内注释；`PluginsEdit{primary, duplicates}` + 倒序删重复行；`migrate_plugin_blocks` 合并重复表头；仅文本变化时原子写；四类拒绝消息统一带 `(line N)`。
- `agent_install` 在碰文件系统前用 `config_lists_plugin` 拒绝重复注册；操作员串改为 `registered plugin <id> in plugins = [...]` / `deregistered plugin <id> from plugins`。

### P8 门禁与 e2e

- `cargo fmt --all`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`。
- 新建 `crates/onlyne-testkit/e2e/acp-payload-v2.sh`（case 19）：两条路由、一条被 ACL 拒、一条 blocked、一条 invalid 后重写；断言 `parent_task`、`hop + 1`、`handoff: ` 前缀、completion receipt、`handoff_denied` 且零 child row。`acp-agent.py` 新增可选 `--caller-report-marker`，缺省路径逐字节不变。

### P9 收口文书

- `CHANGELOG.md` `[Unreleased]` 段；`docs/STATUS.md` 计数、case 19、payload-v2 一句；本文件。

## 已决事项（用户拍板，2026-09-19 深夜）

- 转手链条深度：协议与 server 对 hop 不做上限判定，一条报告可同时交给至多八个 role、每条自带正文——这是初衷。授权模型保持 spec 的 `allowed_targets` 边 + ACL 逐条判定，操作员剪边即收束链条。零代码改动，口径写在 `docs/operations.md` 的结项报告节。
- `repair fail --notify` 与 `repair adopt --session-id`：两个字段删除（通知有替代渠道：`onlyne send --from <supervisor> --to <role>` 或订阅 `ledger_state` 事件）。
- `$NAME` 间接取值接到 client 的读配置路径，与 gateway 面口径一致。
- `onlyne complete --head-from` 给缺省值 `local`，`--text` 改为只在 `local` 分支必填；`onlyne schema` 定为长期公共面，补 CLI 用例与 README/契约记载。
- `--backend-ref` 收 JSON：可整体解析的值按该值上线，其余按 JSON 字符串上线，缺省 null；rebind/adopt 的操作员从此能表达行内对象引用。
- skill 文件保持仓库侧文档，`server generate` 往工作区复制的内容维持现状（模板普通文件、`.pi` 树、按需的 `agent/<pkg>`）；文法权威在 `onlyne report --help` 内嵌的 `GRAMMAR_V2`。

## Out of scope

- 版本 bump、tag、crates.io 发布、装机与冷编译验证（下一轮发布窗口）。
- web admin、调度器、模型 runtime、workspace 文件同步（AGENTS.md §0）。
- trait / reconcile 判定序 / claims / stall 检测的改动。

## Completion criteria

1. `cargo test --workspace` 零红。实测：951 passed / 0 failed / 1 ignored（`herdr_live_probe`）/ 68 suites，较 1.2.1 的 887 增 64。
2. fmt 与 clippy `-D warnings` 全绿。实测：`cargo clippy --workspace --all-targets -- -D warnings` Finished dev 零告警。
3. case 19 连跑两次 exit 0，`acp-session.sh` 与 `running-lights.sh` 零回归，进程树无残留。实测：三者均 PASS，`pgrep` 干净。
4. 文法只有一处定义：全仓 `grep` 只有 `payload.rs` 持前缀与行序规则，CLI help 与 prompt 打印 `GRAMMAR_V2`。
5. 文书齐：CHANGELOG `[Unreleased]`、STATUS 计数与 case 19、operations payload 节、README 双语、SKILL.md。

## 待用户拍板（本轮零实现）

- 装饰旗标：`repair fail --notify`（`faults.rs` 零消费）、`repair adopt --session-id`（`faults.rs:249` 只取 `task_id`）→ 删字段或补服务端行为。
- token 路线（`HandoffGrant` / `accept_handoff` / grant topic）→ 建或删文档段。
- `wait-ready` 退出码、`complete --head-from` 默认值、`NO_SOCKET_MESSAGE` 是否加指引、`$NAME` 间接值是否接线、`onlyne schema` 与顶层退出码表是否长期保留、skill 是否由 `server generate` 复制进工作区、`--backend-ref` 打包。
