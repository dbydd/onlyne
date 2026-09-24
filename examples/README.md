# Examples

The pre-v1 daemon is gone, and its example set went with it. The root `Cargo.toml` is a workspace
manifest, so `cargo run --example <name>` finds nothing to build here.

## End-to-end scripts

| script | covers |
| --- | --- |
| `crates/onlyne-testkit/e2e/local-task.sh` | one server, one role client, one fake agent, one task from send to acked |
| `crates/onlyne-testkit/e2e/two-cluster.sh` | parent server plus child server, with an aggregate role answering across the boundary |
| `crates/onlyne-testkit/e2e/gateway-mount.sh` | fake gateway registration and inbound routing to a role |
| `crates/onlyne-testkit/e2e/idempotency.sh` | duplicate and conflict answers for one repeated `op_id` |
| `crates/onlyne-testkit/e2e/legacy-layout.sh` | refusal of a pre-v1 workspace, exit code 2 |
| `crates/onlyne-testkit/e2e/running-lights.sh` | a six-role ring: a token handed on with `onlyne handoff` twelve times, and two TUI frames of it moving |
| `crates/onlyne-testkit/e2e/herdr-live.sh` | the herdr backend against a live herdr session: workspace per server root, role tab, split pane, `control focus`, drain |

`crates/onlyne-testkit/e2e/lib.sh` holds the shared helpers. Callers set `SRC` and `tmp` first. Every
script in the table above except `herdr-live.sh` runs with `ONLYNE_BACKEND=fake` and the `fake`
gateway; `orca-live.sh` and `pi-live.sh` pick their own host the same way `herdr-live.sh` does.
`herdr-live.sh` needs a sacrificial herdr session and takes `HERDR_SESSION` (default `onlyne-test`).
No real platform credential enters the run.

```bash
cargo build --workspace
cd crates/onlyne-testkit/e2e
bash local-task.sh
```

## Configuration example tree

`.onlyne.example/` holds three things: a comment-dense `spec.toml`, a `templates/dev/` tree for the
planner, builder, and reviewer roles, and a README that maps every example file to its real path.

## CLI vocabulary

`docs/v1-CONTRACT.md` lists the verbs for each binary: `onlyne-server run|init|reload|generate`,
`onlyne-client run|init`, `onlyne send|reply|complete|handoff|control`, and
`onlyne gateway run|list|status|auth`.

# 中文

## 示例

pre-v1 守护进程已经移除，随之移除的还有它的示例集。根目录的 `Cargo.toml` 是工作区清单，因此在这里运行 `cargo run --example <name>` 找不到可构建的内容。

## 端到端脚本

| 脚本 | 覆盖内容 |
| --- | --- |
| `crates/onlyne-testkit/e2e/local-task.sh` | 一个服务器、一个角色客户端、一个模拟代理，以及一个从发送到确认的任务 |
| `crates/onlyne-testkit/e2e/two-cluster.sh` | 父服务器和子服务器，并使用一个跨边界应答的聚合角色 |
| `crates/onlyne-testkit/e2e/gateway-mount.sh` | 模拟网关注册，以及到某个角色的入站路由 |
| `crates/onlyne-testkit/e2e/idempotency.sh` | 对一个重复的 `op_id` 给出重复应答和冲突应答 |
| `crates/onlyne-testkit/e2e/legacy-layout.sh` | 拒绝 pre-v1 工作区，退出代码为 2 |
| `crates/onlyne-testkit/e2e/running-lights.sh` | 一个由六个角色组成的环：使用 `onlyne handoff` 传递令牌十二次，以及令牌移动过程中的两个 TUI 帧 |
| `crates/onlyne-testkit/e2e/herdr-live.sh` | 针对实时 herdr 会话的 herdr 后端：每个服务器根目录一个工作区、角色标签页、拆分窗格、`control focus`、排空 |

`crates/onlyne-testkit/e2e/lib.sh` 包含共享辅助函数。调用方必须先设置 `SRC` 和 `tmp`。表中除 `herdr-live.sh` 外的每个脚本都使用 `ONLYNE_BACKEND=fake` 和 `fake` 网关运行；`orca-live.sh` 和 `pi-live.sh` 以与 `herdr-live.sh` 相同的方式自行选择主机。`herdr-live.sh` 需要一个用作祭品的 herdr 会话，并接受 `HERDR_SESSION`（默认值为 `onlyne-test`）。运行过程中不会输入任何真实的平台凭据。

```bash
cargo build --workspace
cd crates/onlyne-testkit/e2e
bash local-task.sh
```

## 配置示例目录树

`.onlyne.example/` 包含三项内容：一个注释密集的 `spec.toml`，供规划器、构建器和审查者角色使用的 `templates/dev/` 目录树，以及一个将每个示例文件映射到其真实路径的 README。

## CLI 词汇

`docs/v1-CONTRACT.md` 列出了每个二进制文件的动词：`onlyne-server run|init|reload|generate`、`onlyne-client run|init`、`onlyne send|reply|complete|handoff|control`，以及 `onlyne gateway run|list|status|auth`。
