# swarm.sock IPC（IPC，修订一 2026-09-06）

调度器在 `<root>/.onlyne/run/swarm.sock` 监听。行分隔 JSON（与 onlyne 主仓库同构）。
请求含 `id` + `op`；响应单行 `{id, ok, data?, error?}`；`subscribe` 成功后同连接
持续推送 `{event: true, type, data}`。

## 1. 操作

| op | 参数 | 返回 |
|---|---|---|
| `status` | — | `{root, workspaces, tasks_by_state, ledger_tail}` |
| `list_workspaces` | — | `[{path, name, model, back_edges}]` |
| `list_tasks` | `{state?, limit?}` | `[{task_id, from_ws, to_ws, transfer_send_to, attempt, state, terminal}]` |
| `submit` | `{to, payload_markdown}` | `{task_id}`（调度器生成 UUID，构造 swarm 头后投递） |
| `cancel` | `{task_id, reason?}` | `{cancelled: [...]}`（血缘族，杀 terminal，记 cancelled 台账） |
| `toggle_swarm` | `{workspace, enabled}` | `{workspace, enabled}`（改该 workspace `config.toml [swarm]` 并通知 pi-onlyne 重载） |
| `subscribe` | `{priority?}` | `{subscribed: true}` 后推送事件 |

CLI `submit --to <树相对路径> --payload <文件>` 即调 `submit`。
`toggle_swarm` 也可在 TUI 内按键触发。

## 2. 事件

| type | data |
|---|---|
| `task_created` | `{task_id, from, to}` |
| `task_running` | `{task_id, to, terminal_handle}` |
| `task_done` | `{task_id, from}` |
| `task_failed` | `{task_id, to, reason}` |
| `task_closed` | `{task_id}` |
| `task_stalled_ready_timeout` | `{task_id, to}`（计数告警，不干扰运行） |
| `daemon_state` | `{workspace, state}` |
| `orphan_instance` | `{path}` |
| `dangling_link` | `{workspace, target}` |

无 `task_replied` / `callback_forwarded`：out 即 done，射后不理。

## 3. 错误

`{id, ok: false, error: {code, message}}`。code 含
`not_swarm_root`、`unknown_workspace`、`unknown_task`、`sync_failed`、`daemon_failed`。
