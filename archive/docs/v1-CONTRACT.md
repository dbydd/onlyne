# Onlyne v1.0.0 — internal work-split contract

Read this before touching code. The director owns commits, the root `Cargo.toml`, and scope changes.

## Repo state

Cargo workspace, `members = ["crates/*", "plugins/*"]`, 18 packages: 14 under `crates/` and the four gateway plugins. The pre-v1 daemon under `crates/onlyne-legacy/` is gone, and so is the `vendor/` snapshot of the orchestrator submodule. The lifecycle and reconcile kernel those trees carried now lives in `crates/onlyne-session/`, and S12 removed the source trees.
`docs/v1-PLAN.md` is the full design spec (527 lines, Chinese). Read the section your task names before writing code. The plan wins over a worker brief wherever they disagree; file ownership is the exception.

## Ownership

| 拥有的路径 | 交付物 |
|---|---|
| `crates/onlyne-frame/`, `crates/onlyne-proto/` | length-prefixed JSON codec, wire types, schema export |
| `crates/onlyne-session/` | lifecycle reducer, `SessionBackend`, ledger bridge |
| `crates/onlyne-config/`, `crates/onlyne-layout/`, `crates/onlyne-store/` | spec TOML, workspace layout, SQLite |
| `crates/onlyne-net/` | TLS, ed25519 handshake, ACL, backoff |
| `crates/onlyne-adapter/`, `crates/onlyne-testkit/` | adapter SDK, fake agent/gateway, conformance |
| `crates/onlyne-gateway/`, `plugins/*` | gateway kit, four platform plugins |
| `crates/onlyne-server/` | router, relay, projection, faults, admin, generate |
| `crates/onlyne-client/` | runloop, adapter socket, accept, dispatch, intent |
| `crates/onlyne-cli/` | thin human entrypoint |

## Hard rules

- Touch only your owned paths. A file another worker owns is read-only for you.
- Never edit the root `Cargo.toml`. When a dependency is missing from `[workspace.dependencies]`, declare it in your own `Cargo.toml` with an explicit version and list it in your report for consolidation.
- Never run `git`. Never delete files outside your owned paths.
- Use a private target directory so builds do not serialise on the shared lock: `CARGO_TARGET_DIR=target/<name> cargo check -p <crate>`. Crate-scoped cargo commands only.
- Zero backward compatibility: no legacy aliases, no dual read paths, no `#[allow(dead_code)]` keeping unused ported code, no `todo!()`, no stub returning `Ok(())` pretending to work.
- Byte-exact strings: when the brief quotes a user-facing message, reproduce it character for character.
- Comments and docs use direct additive sentences. Forbidden rhetoric includes `not X but Y`, `rather than`, `instead of`, `however`, `but`, `on the other hand`, and every equivalent in Chinese.
- Report files touched with line counts, the exact commands run, their real output tail, and any gap with its reason. A test claim without runner output is a gap.

## Cross-crate decoupling decisions

- `onlyne-frame` carries codec only. `onlyne-proto` carries all wire types and holds no tokio.
- `onlyne-proto` public API is the single source of names: `Envelope`, `Body`, `ImagePart`, `Causality`, `Principal`, `MsgKind`, `ControlOp`, `Outcome`, `Frame`, `ResBody`, `ErrorPayload`, `ErrorCode`, `Event`, `LedgerState`, `Presence`, `GatewayHealth`, `Lifecycle`, `EventTier`, `ClientOp`, `AdminOp`, `GatewayOp`, `Report`, `SessionProjection`, `Receipt`, `Welcome`, `HandshakeArgs`, `PluginOp`, `HostOp`, `AdapterMsg`, `Capability`, `Mount`, `HelloArgs`, `HelloAck`. Exact field lists live in `crates/onlyne-proto/src/*.rs`; read them.
- `onlyne-net` does not depend on `onlyne-config`. It owns `RoleAcl`, `AclTable`, `MsgClass`, and `acl_allows(table, from, to, class, owner: Option<&str>) -> Result<(), AclDeny>`, keyed by role-name strings, while `onlyne-config` owns `Spec` and its wildcard expansion.
- Deviation from `docs/v1-PLAN.md` §5 line 274, recorded here: the plan writes `acl_allows(spec, from, to, kind)` in `onlyne-net`, and the shipped shape is `Spec::acl_edges()` in `crates/onlyne-config/src/spec.rs` plus `AclTable::acl_allows` in `crates/onlyne-net/src/acl.rs`.
- The wildcard expands once, at load, inside `Spec::acl_edges()`, which is the single place `"*"` carries meaning.
- The reserved role `_supervisor` is one-sided on its own sends: `Spec::acl_edges()` reads that sender's `allowed_targets` alone. An empty list reaches every registered role, a non-empty list names the reachable roles exactly, and no receiver `allowed_senders` is read on its rows. Every other pair keeps the two-sided rule, so a row into `_supervisor` still needs that role to name its sender. `SUPERVISOR_ROLE` in `crates/onlyne-config/src/spec.rs` holds the one spelling of the name.
- `onlyne-net` stores concrete role names, and `AclTable::new` refuses a `"*"` endpoint as a wiring bug.
- The ordering rule from line 274 holds: `crates/onlyne-server/src/relay.rs` calls `AclTable::acl_allows` before the ledger append, and the server translates `AclDeny { reason, field }` into `ErrorCode`.
- The reason is the dependency direction in this section: `Spec` is a TOML document with `deny_unknown_fields`, line-number diagnostics, `spec_hash` canonicalisation, and a reload diff, and none of that belongs on the wire. The semantics as data live in `crates/onlyne-config/tests/acl_table.rs`.
- `onlyne-session` does not depend on `onlyne-proto`, `onlyne-store`, or `onlyne-net`. Persistence crosses the `SessionLedger` trait defined in `crates/onlyne-session/src/reconcile/ledger.rs`.
- `onlyne-store` implements `onlyne-session::SessionLedger` for the client database, and exposes the server ledger API described in §10 of the plan.
- JSON Schemas live beside their types: `crates/onlyne-proto/schema/{envelope,adapter}.schema.json`, `crates/onlyne-config/schema/{spec,config-client}.schema.json`. Each crate carries one schema tool, `gen-schema` in `onlyne-proto` and `config-schema` in `onlyne-config`, so the two binaries have their own names on disk.
- `onlyne-server` `generate` writes the vendored plugin package at `<ws>/.onlyne/agent/<pkg-name>/` and its runtime settings entry as `../.onlyne/agent/<pkg-name>`, where a runtime settings file is the `settings.json` directly under a top-level dot-directory (`<ws>/.pi`, `<ws>/.omp`); a `settings.json` nested deeper or outside a dot-directory keeps the workspace-root form. Measured on pi 0.85.1: a project `packages` path resolves against the directory holding that settings file (`<ws>/.pi`), so the `../` form is the one that loads the extension.
- Deviation from `docs/v1-PLAN.md` §11 line 374, recorded here: the plan names `.onlyne/config.toml` as a template's only departure from opaque content, and the shipped `load_tree` carries every dot-directory the template holds except `.onlyne` and `.git`, so one template can ship the project-local config of whichever agent runtime the operator runs.

## CLI vocabulary (`onlyne-cli`, output is JSON by default)

```
onlyne server init|run|start|stop|generate ...                                 # execs onlyne-server
onlyne server status|roles|sessions|ledger|faults|watch|history|repair ...     # same admin answers, in-process
onlyne client run|status|init|roles|sessions|watch|history                                                # execs onlyne-client
onlyne status|roles|sessions|ledger|faults|watch|history|spec_diff|reload|generate|wait-ready|repair ...          # admin surface, one frame per call
onlyne cluster export-prose
onlyne send --to <role> [--task <id>] [--text ...|--file -] [--image f.png] [--note] --force --yes-i-am-supervisor-not-other-role
onlyne reply --to <envelope-id> --text ... --force --yes-i-am-supervisor-not-other-role
onlyne complete --task <id> [--outcome done|failed|cancelled] [--head-from local|ledger] [--text ...] --force --yes-i-am-supervisor-not-other-role
onlyne handoff --to <role> --task <id> --text ... --force --yes-i-am-supervisor-not-other-role
onlyne ack --msg-id <id> [--op-id <id>] --reason <text> --force --yes-i-am-supervisor-not-other-role     # role surface only
onlyne reject --msg-id <id> [--op-id <id>] --reason <text> --force --yes-i-am-supervisor-not-other-role  # role surface only
onlyne control --task <id> [--to <role>] probe|snapshot --force --yes-i-am-supervisor-not-other-role
onlyne control --task <id> [--to <role>] recycle|cancel --reason <text> --force --yes-i-am-supervisor-not-other-role
onlyne gateway run <telegram|feishu|qqbot|weixin> --server-root <dir> [--token ...]
onlyne gateway list|status|auth <platform> [...]
onlyne who|ping|version|completions <bash|elvish|fish|powershell|zsh>
onlyne schema <client|spec> [--pretty]                                                # local JSON Schema, zero socket
```

Seven verbs carry `--force --yes-i-am-supervisor-not-other-role`: `send`, `reply`, `handoff`, `complete`, `ack`, `reject`, and `control`. Both flags are required together. A call missing either one exits 2 before it resolves a socket or writes anything. The refusal names the plugin tool that answers for a role where one exists (`onlyne_send` for `send`, `onlyne_handoff` for `handoff`, `onlyne_complete` for `complete`).

The CLI ships three daemons and one entrypoint: `onlyne-server`, `onlyne-client`, `onlyne-gateway`, `onlyne`. `onlyne <group> <verb>` execs the matching daemon binary. Message verbs connect straight to the local socket (UDS on unix, named pipe on Windows) and print one JSON answer.
`onlyne complete` takes its head — the short result line a completion carries — from one of two sources. `--head-from local` is the default: it builds the head by truncating `--text`, so that branch requires `--text`, and a missing one answers `onlyne: --text is required with --head-from local` and exits 2. `--head-from ledger` reads the head from the ledger row's own `out_head`, so `--text` stays optional there.
`onlyne schema <client|spec> [--pretty]` and `onlyne completions <bash|elvish|fish|powershell|zsh>` never touch the socket, so the server root and the workspace may both be absent. `onlyne schema` prints the document `onlyne-config` generates at build time and embeds with `include_str!`, and that same document validates `config.toml` and `spec.toml`.
The admin noun set resolves its socket the same way and shares the message-verb exit-code table. `wait-ready` polls admin `status` at 200 ms intervals with a 10 s bound, and prints `onlyne: server not ready after 10000ms` on failure. `generate` writes the `[[client]]` fragment to stdout and progress to stderr.
A missing daemon binary makes the CLI print exactly this to stderr and exit 127:
`onlyne: binary not found: <name>`
The line names the binary and stops there. For a `cargo install` user it means the sibling daemon is absent from the cargo bin directory, so the remedy is to install or update the crate that ships it. The e2e harness keeps its own pre-flight line, `onlyne: missing binary <path>; run cargo build --workspace`, and that advice fits only where the binaries come from a workspace build.
`onlyne cluster export-prose` prints raw prose by default and takes `--json`. It issues the existing role query and adds no protocol op.

Socket resolution runs in this order: `--socket <path>` → `--server-root <dir>` as `<dir>/.onlyne/run/s` → `--workspace <dir>` or the current directory upward for `.onlyne/run/s`. A `--socket` value that starts with `\\.\pipe\` is used as the NPFS name with no hashing. `ERROR_PIPE_BUSY` retries inside `--timeout`. Exit codes 2, 3, 4, and 5 stay. A path that never resolves answers the same way as a path that resolves to nothing on disk. Both write exactly this to stderr and exit 3, with stdout left empty so a script reads no answer body:

```
onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace
```

## Wave order

1. proto + frame finish, session kernel, config/layout/store, net. These four are independent.
2. server runtime, client runtime, adapter SDK + testkit, gateway kit.
3. generate, federation path, legacy deletion, docs.

## Process verbs versus admin queries

`onlyne-server` owns the process verbs `init`, `run`, `start`, `stop`, `status`, and `generate`; `onlyne spec_diff` prints `SpecDiff::render()`.
`onlyne server roles|sessions|ledger|faults|watch|history|repair_*` stays in `onlyne-cli`, which resolves those verbs against the admin socket and formats the answers for a human. It never execs `onlyne-server`.
`onlyne-server status` answers the process question from its own tree: pid, socket path, uptime, spec hash, and store reachability.
`onlyne status` on the CLI is the `AdminOp::Status` socket round-trip.
The two answer different questions: one describes the local process, the other describes the live cluster.
`onlyne client` and `onlyne gateway` follow the same split: each daemon binary owns `run` and its own `status` — the client never detaches itself, so it carries no `start`/`stop` — and `onlyne-cli` resolves every query verb against that daemon's socket.
Prose keeps direct additive sentences; contrastive rhetoric stays banned.

# Onlyne v1.0.0 — 内部工作拆分契约（中文）

在修改代码前阅读本文。director 负责提交、根 `Cargo.toml` 和范围变更。

## 仓库状态

Cargo workspace，`members = ["crates/*", "plugins/*"]`，18 个包：14 个位于 `crates/` 下，另有 4 个 gateway 插件。v1 之前的 daemon 位于 `crates/onlyne-legacy/`，现已移除；orchestrator submodule 的 `vendor/` 快照也已移除。那些目录承载的 lifecycle 和 reconcile kernel 现在位于 `crates/onlyne-session/`，S12 删除了源目录。
`docs/v1-PLAN.md` 是完整设计规格（527 行，中文）。编写代码前阅读任务指定的章节。两者冲突时以 plan 为准，文件归属除外。

## 归属

| owned paths | deliverable |
|---|---|
| `crates/onlyne-frame/`, `crates/onlyne-proto/` | length-prefixed JSON codec, wire types, schema export |
| `crates/onlyne-session/` | lifecycle reducer, `SessionBackend`, ledger bridge |
| `crates/onlyne-config/`, `crates/onlyne-layout/`, `crates/onlyne-store/` | spec TOML, workspace layout, SQLite |
| `crates/onlyne-net/` | TLS, ed25519 handshake, ACL, backoff |
| `crates/onlyne-adapter/`, `crates/onlyne-testkit/` | adapter SDK, fake agent/gateway, conformance |
| `crates/onlyne-gateway/`, `plugins/*` | gateway kit, four platform plugins |
| `crates/onlyne-server/` | router, relay, projection, faults, admin, generate |
| `crates/onlyne-client/` | runloop, adapter socket, accept, dispatch, intent |
| `crates/onlyne-cli/` | thin human entrypoint |

## 硬性规则

- 只能修改自己拥有的路径。其他 worker 拥有的文件对你只读。
- 不得编辑根 `Cargo.toml`。当 `[workspace.dependencies]` 缺少依赖时，在自己的 `Cargo.toml` 中用明确版本声明，并在报告中列出以便合并。
- 不得运行 `git`。不得删除自己拥有路径之外的文件。
- 使用私有 target directory，避免构建在共享锁上串行化：`CARGO_TARGET_DIR=target/<name> cargo check -p <crate>`。仅运行 crate 范围的 cargo 命令。
- 零向后兼容：不得使用 legacy alias、双重读取路径、用 `#[allow(dead_code)]` 保留未使用的移植代码、`todo!()`，或返回 `Ok(())` 假装工作的 stub。
- 字符串必须逐字节一致：brief 引用面向用户的消息时，逐字符复现。
- 注释和文档使用直接、增补式句子。禁止使用 `not X but Y`、`rather than`、`instead of`、`however`、`but`、`on the other hand` 以及中文中的同等表达。
- 报告修改的文件及行数、运行的确切命令、真实输出尾部，以及任何缺口及其原因。没有 runner 输出的测试声明属于缺口。

## 跨 crate 解耦决策

- `onlyne-frame` 只承载 codec。`onlyne-proto` 承载所有 wire types，且不持有 tokio。
- `onlyne-proto` 公共 API 是名称的唯一来源：`Envelope`、`Body`、`ImagePart`、`Causality`、`Principal`、`MsgKind`、`ControlOp`、`Outcome`、`Frame`、`ResBody`、`ErrorPayload`、`ErrorCode`、`Event`、`LedgerState`、`Presence`、`GatewayHealth`、`Lifecycle`、`EventTier`、`ClientOp`、`AdminOp`、`GatewayOp`、`Report`、`SessionProjection`、`Receipt`、`Welcome`、`HandshakeArgs`、`PluginOp`、`HostOp`、`AdapterMsg`、`Capability`、`Mount`、`HelloArgs`、`HelloAck`。确切字段列表位于 `crates/onlyne-proto/src/*.rs`；请阅读它们。
- `onlyne-net` 不依赖 `onlyne-config`。它拥有 `RoleAcl`、`AclTable`、`MsgClass`，以及 `acl_allows(table, from, to, class, owner: Option<&str>) -> Result<(), AclDeny>`，以角色名字符串为键；`onlyne-config` 拥有 `Spec` 及其 wildcard expansion。
- 记录相对于 `docs/v1-PLAN.md` §5 第 274 行的偏差：plan 在 `onlyne-net` 中写作 `acl_allows(spec, from, to, kind)`，实际交付形态是 `crates/onlyne-config/src/spec.rs` 中的 `Spec::acl_edges()`，以及 `crates/onlyne-net/src/acl.rs` 中的 `AclTable::acl_allows`。
- wildcard 在加载时通过 `Spec::acl_edges()` 扩展一次；这是 `"*"` 唯一具有含义的位置。
- 保留角色 `_supervisor` 对自身发送采用单向规则：`Spec::acl_edges()` 只读取该发送者的 `allowed_targets`。空列表可到达每个已注册角色，非空列表精确指定可到达角色，读取这些行时不读取接收者的 `allowed_senders`。其他每个配对保留双向规则，因此指向 `_supervisor` 的行仍需要该角色指定发送者。`SUPERVISOR_ROLE` 位于 `crates/onlyne-config/src/spec.rs`，保存该名称的唯一拼写。
- `onlyne-net` 存储具体角色名，且 `AclTable::new` 将 `"*"` 端点视为 wiring bug 并拒绝。
- 第 274 行的顺序规则保持不变：`crates/onlyne-server/src/relay.rs` 在追加 ledger 前调用 `AclTable::acl_allows`，server 将 `AclDeny { reason, field }` 转换为 `ErrorCode`。
- 本节中的依赖方向原因是：`Spec` 是带 `deny_unknown_fields`、行号诊断、`spec_hash` canonicalisation 和 reload diff 的 TOML 文档，这些内容不属于 wire。数据形式下的语义位于 `crates/onlyne-config/tests/acl_table.rs`。
- `onlyne-session` 不依赖 `onlyne-proto`、`onlyne-store` 或 `onlyne-net`。持久化通过 `crates/onlyne-session/src/reconcile/ledger.rs` 中定义的 `SessionLedger` trait 跨越。
- `onlyne-store` 为客户端数据库实现 `onlyne-session::SessionLedger`，并暴露 plan §10 描述的 server ledger API。
- JSON Schema 位于各自类型旁：`crates/onlyne-proto/schema/{envelope,adapter}.schema.json`、`crates/onlyne-config/schema/{spec,config-client}.schema.json`。每个 crate 各自带一个 schema tool：`onlyne-proto` 中的 `gen-schema` 和 `onlyne-config` 中的 `config-schema`，因此两个二进制文件在磁盘上有不同名称。
- `onlyne-server` 的 `generate` 将 vendored plugin package 写入 `<ws>/.onlyne/agent/<pkg-name>/`，并将其 runtime settings entry 写为 `../.onlyne/agent/<pkg-name>`，其中 runtime settings 文件是顶层 dot-directory（`<ws>/.pi`、`<ws>/.omp`）下直接存在的 `settings.json`；嵌套更深或位于 dot-directory 外的 `settings.json` 保持 workspace-root 形式。pi 0.85.1 实测：项目 `packages` 路径相对于保存该 settings 文件的目录（`<ws>/.pi`）解析，因此 `../` 形式能够加载 extension。
- 记录相对于 `docs/v1-PLAN.md` §11 第 374 行的偏差：plan 将 `.onlyne/config.toml` 指定为 template 唯一不同于 opaque content 的内容；实际 `load_tree` 保留 template 包含的每个 dot-directory，除了 `.onlyne` 和 `.git`，因此一个 template 可以随 operator 运行的 agent runtime 一同提供项目本地 config。

## CLI 词汇（`onlyne-cli`，默认输出 JSON）

```
onlyne server init|run|start|stop|generate ...                                 # execs onlyne-server
onlyne server status|roles|sessions|ledger|faults|watch|history|repair ...     # same admin answers, in-process
onlyne client run|status|init|roles|sessions|watch|history                                                # execs onlyne-client
onlyne status|roles|sessions|ledger|faults|watch|history|spec_diff|reload|generate|wait-ready|repair ...          # admin surface, one frame per call
onlyne cluster export-prose
onlyne send --to <role> [--task <id>] [--text ...|--file -] [--image f.png] [--note] --force --yes-i-am-supervisor-not-other-role
onlyne reply --to <envelope-id> --text ... --force --yes-i-am-supervisor-not-other-role
onlyne complete --task <id> [--outcome done|failed|cancelled] [--head-from local|ledger] [--text ...] --force --yes-i-am-supervisor-not-other-role
onlyne handoff --to <role> --task <id> --text ... --force --yes-i-am-supervisor-not-other-role
onlyne ack --msg-id <id> [--op-id <id>] --reason <text> --force --yes-i-am-supervisor-not-other-role     # role surface only
onlyne reject --msg-id <id> [--op-id <id>] --reason <text> --force --yes-i-am-supervisor-not-other-role  # role surface only
onlyne control --task <id> [--to <role>] probe|snapshot --force --yes-i-am-supervisor-not-other-role
onlyne control --task <id> [--to <role>] recycle|cancel --reason <text> --force --yes-i-am-supervisor-not-other-role
onlyne gateway run <telegram|feishu|qqbot|weixin> --server-root <dir> [--token ...]
onlyne gateway list|status|auth <platform> [...]
onlyne who|ping|version|completions <bash|elvish|fish|powershell|zsh>
onlyne schema <client|spec> [--pretty]                                                # local JSON Schema, zero socket
```

七个 verb 携带 `--force --yes-i-am-supervisor-not-other-role`：`send`、`reply`、`handoff`、`complete`、`ack`、`reject` 和 `control`。两个 flag 必须同时提供。缺少任意一个的 call 会在解析 socket 或写入任何内容之前退出 2。拒绝信息会指出代表某角色作答的 plugin tool（存在时：`send` 对应 `onlyne_send`，`handoff` 对应 `onlyne_handoff`，`complete` 对应 `onlyne_complete`）。

CLI 包含三个 daemon 和一个 entrypoint：`onlyne-server`、`onlyne-client`、`onlyne-gateway`、`onlyne`。`onlyne <group> <verb>` exec 匹配的 daemon binary。Message verb 直接连接本地 socket（unix 使用 UDS，Windows 使用 named pipe）并打印一个 JSON answer。
`onlyne complete` 从两个来源之一获取 head，即 completion 携带的简短结果行。`--head-from local` 是默认值：通过截断 `--text` 构建 head，因此该分支要求 `--text`，缺少时输出 `onlyne: --text is required with --head-from local` 并退出 2。`--head-from ledger` 从 ledger 行自身的 `out_head` 读取 head，因此 `--text` 在此保持可选。
`onlyne schema <client|spec> [--pretty]` 和 `onlyne completions <bash|elvish|fish|powershell|zsh>` 不接触 socket，因此 server root 和 workspace 都可以不存在。`onlyne schema` 打印 `onlyne-config` 在构建时生成并通过 `include_str!` 嵌入的文档，同一文档还验证 `config.toml` 和 `spec.toml`。
admin noun set 以同样方式解析 socket，并共享 message-verb exit-code 表。`wait-ready` 每 200 ms 轮询 admin `status`，上限为 10 s，失败时打印 `onlyne: server not ready after 10000ms`。`generate` 将 `[[client]]` fragment 写入 stdout，将 progress 写入 stderr。
daemon binary 缺失时，CLI 精确向 stderr 打印以下内容并退出 127：
`onlyne: binary not found: <name>`
该行指出 binary 名称并在此结束。对于 `cargo install` 用户，这表示 sibling daemon 不在 cargo bin directory 中，修复方式是安装或更新提供它的 crate。e2e harness 保留自己的 pre-flight 行 `onlyne: missing binary <path>; run cargo build --workspace`，该建议只适用于 binary 来自 workspace build 的场景。
`onlyne cluster export-prose` 默认打印 raw prose，并接受 `--json`。它发出既有 role query，不添加 protocol op。

Socket 解析按以下顺序执行：`--socket <path>` → `--server-root <dir>` 解析为 `<dir>/.onlyne/run/s` → `--workspace <dir>` 或从当前目录向上查找 `.onlyne/run/s`。以 `\\.\pipe\` 开头的 `--socket` 值直接作为 NPFS name 使用，不进行 hashing。`ERROR_PIPE_BUSY` 在 `--timeout` 内重试。退出码 2、3、4、5 保持不变。永远无法解析的 path 与解析到磁盘上不存在内容的 path 返回相同结果。两者都向 stderr 精确写入以下内容并退出 3，stdout 保持为空，因此脚本不会读取到 answer body：

```
onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace
```

## Wave 顺序

1. 完成 proto + frame、session kernel、config/layout/store、net。这四项相互独立。
2. server runtime、client runtime、adapter SDK + testkit、gateway kit。
3. generate、federation path、legacy deletion、docs。

## Process verbs 与 admin queries

`onlyne-server` 拥有 process verbs `init`、`run`、`start`、`stop`、`status` 和 `generate`；`onlyne spec_diff` 打印 `SpecDiff::render()`。
`onlyne server roles|sessions|ledger|faults|watch|history|repair_*` 保留在 `onlyne-cli` 中；它针对 admin socket 解析这些 verbs，并为人类格式化答案。它从不 exec `onlyne-server`。
`onlyne-server status` 从自身 tree 回答 process question：pid、socket path、uptime、spec hash 和 store reachability。
CLI 上的 `onlyne status` 是 `AdminOp::Status` socket round-trip。
两者回答不同问题：一个描述本地 process，另一个描述 live cluster。
`onlyne client` 和 `onlyne gateway` 遵循相同划分：每个 daemon binary 拥有 `run` 和自己的 `status`（client 不会自行 detach，因此不携带 `start`/`stop`），而 `onlyne-cli` 针对该 daemon 的 socket 解析每个 query verb。
Prose 使用直接、增补式句子；继续禁止对比性修辞。
