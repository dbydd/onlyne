# onlyne-client

One workspace, one role, one daemon. The role runs many sessions at once.

## Verbs

| verb | one line |
| --- | --- |
| `run --workspace <dir>` | Foreground role runtime: connect, handshake, pull, dispatch, report. Backgrounding is the operator's job, never the client's. |
| `status --workspace <dir>` | Print uptime, socket path, recorded fault count, and whether the server link is up. |
| `doctor` | Print host-detection JSON. No workspace, no socket. Exit 0. |
| `init --workspace <dir> --role <r> --server-root <dir>` | Build the minimal role workspace and print the `[[client]]` spec fragment. |
| `roles --workspace <dir>` | Answer role prose from the local cache. |
| `sessions --workspace <dir>` | Reserved for the live role runtime. |
| `watch --workspace <dir>` | Reserved for the live role runtime. |
| `history --workspace <dir>` | Reserved for the live role runtime. |

`run` is the only launch verb, and it stays in the foreground. `--workspace` takes a relative path and resolves it to an absolute path before use, so the daemon, its generated sessions, and herdr's `--cwd` all read one location. A supervisor that wants the client in the background owns that decision — a visible terminal tab, `launchd`, `nohup` — so the client never detaches, writes no pid file, and nothing signals it by number. A `run` whose adapter socket cannot be bound ends there with exit 1 and names the failure on stderr; an `accept` error after a successful bind logs at `error` level (`adapter socket accept failed; retrying`) and retries every 100 ms with the listener held.
`status` prints `onlyne: client running uptime <n>s socket <path> faults <n>`. The `<path>` is the served socket path read through the owner tree — the canonical `run/s`, or the short derived path a deep workspace serves from, the answer `<workspace>/.onlyne/run/socket` also carries. The uptime is the age of the socket file, and a client counts as running only when that socket answers an `admin` `hello`, so a socket file an unclean exit left behind reads as not running. When the answering client holds no server link it adds `onlyne: client not connected` on stderr.

The printed `[[client]]` fragment is a complete role entry: it carries `role`, `key`, `admin`, `max_sessions`, the ACL lists, `prose`, and `session_command`. Paste it into `spec.toml` and reload; the client can then spawn sessions for that role.

## Workspace layout

Both `init` and `run` create these paths under `--workspace`:

| path | mode | content |
| --- | --- | --- |
| `.onlyne/config.toml` | | role, `cert_pin`, `key_path`, `plugins = [...]`, `[server]` host and port, `[orca]` worktree |
| `.onlyne/client.db` | | SQLite: `intents`, `sessions`, `faults`, `prose_cache`, `config_cache`, `events` |
| `.onlyne/keys/role.key` | `0600` | 32 raw ed25519 bytes, generated once |
| `.onlyne/run/` | `0700` | runtime directory |
| `.onlyne/run/s` | `0600` | adapter socket, the canonical spelling; `run` binds it while the path fits 103 bytes |
| `.onlyne/run/socket` | `0600` | one line naming the path actually served — the canonical `run/s`, or, for a tree deeper than the bound, a short derived path under the system temporary directory |
| `.onlyne/logs/client.log` | | stdout and stderr, when the operator starts `run` under a shell that redirects them |
| `.onlyne/agent/<id>/` | | installed plugin package with `plugin.toml` |
| `.onlyne/cache/orca-tabs.jsonl` | | append-only Orca tab to session map: a supervisor/display side-channel, not the identity (the adapter protocol owns that) |

`init` never writes `spec.toml`. A workspace holding the pre-v1 layout is refused before any write: exit 2 and the byte-exact line `onlyne: legacy workspace layout; v1.0.0 does not migrate`.

Three config values take a `$NAME` spelling: `cert_pin`, `key_path`, and `[server] host`. At startup `run` reads the environment variable named after the `$`, then puts its value where the config line sits. The gateway plugins use that same idiom for platform tokens. A name the environment carries no value for — absent, or present and blank — stops the launch with exit 1 and one line on stderr naming both the field and the variable: `onlyne-client: missing secret $ONLYNE_CERT for cert_pin; set the environment variable`. A value with no leading `$` travels verbatim, so a literal `$` inside a value stays part of the string.

## Exit codes

| code | meaning |
| --- | --- |
| 0 | the verb finished |
| 1 | the verb failed; the reason is one line on stderr |
| 1 | `run` could not bind the adapter socket; stderr is `onlyne-client: bind the workspace socket <canonical path>: <detail>`, the detail naming the served path, both byte lengths, and the OS reason |
| 2 | `status` found no client answering its socket, printed as `onlyne: client not running` |
| 2 | `status` found a client with no server link, printed as `onlyne: client not connected` |
| 2 | the workspace holds the legacy layout |
| 5 | `run` selected no host; stderr is `onlyne: no supported host detected; run inside herdr, orca, or zellij, or set ONLYNE_BACKEND` |

`status` exits 0 only for a client that is up and connected to its server. That is the fact a script reads.
`doctor` exits 0 for every host-detection result, including `host: null`.

## Backends

Selection is env `ONLYNE_BACKEND` (nonempty) > workspace `config.toml` `backend` > auto.

| name | parse aliases | how it is chosen | notes |
| --- | --- | --- | --- |
| `herdr` | | env, config, or auto probe (first) | pane host |
| `orca` | | env, config, or auto probe | tab host |
| `zellij` | | env, config, or auto probe | pane host; probe maps EXITED / `exit_status` |
| `exec` | `headless` | env or config only | projections record the backend as `exec` |
| `fake` | | env or config only | in-process, for tests |
| `auto` | empty string | default when env and config are empty | probes herdr, then orca, then zellij |

A nonempty value that names `herdr`, `orca`, `zellij`, `exec`/`headless`, or `fake` selects that backend. `exec` and `fake` are never discovered by auto. With no match, `onlyne-client run` exits 5 and writes `onlyne: no supported host detected; run inside herdr, orca, or zellij, or set ONLYNE_BACKEND`.

`fake` runs sessions in-process and needs no external tool; the end-to-end scripts set `ONLYNE_BACKEND=fake`. `exec` spawns the role's `session_command` as a child of the client, holds stdin open, and appends the child's output to `.onlyne/logs/session-<task>.log`. On child exit, `probe` may fill `detail.output_tail` (at most 200 lines / 16 KiB). `crates/onlyne-testkit/e2e/pi-live.sh` and `exec-headless.sh` set this path. Windows close uses `CREATE_NEW_PROCESS_GROUP` plus `CTRL_BREAK`, then `kill`; a process with no console terminates the child directly. Operator-facing graceful stop of the daemons is `onlyne shutdown`.

### herdr

A herdr session is inherited from the client process environment; a pi child running in a pane inherits it too. One server root/topology maps to one herdr workspace labelled `onlyne:<cluster>`. `<cluster>` is the server's own `[server] name`, which the client reads from `welcome.cluster` and passes to every pane it creates as `ONLYNE_CLUSTER`. One role maps to one tab. One onlyne session maps to one pane. Close is `herdr pane close`, and a `pane_not_found` answer is that close succeeding, logged `herdr pane already closed` at debug. Ids look like `wF`, `wF:t1`, `wF:p1`. A named session such as `onlyne-test` is the `HERDR_SESSION` value already in the client environment. The backend addresses a workspace by the label `onlyne:<cluster>` and a tab by the role's own name. An operator who wants a particular workspace or tab used renames it before the client spawns sessions: `herdr workspace rename <WORKSPACE_ID> onlyne:<cluster>` and `herdr tab rename <TAB_ID> <role>`. A workspace label that differs yields a second workspace, a tab name that differs yields a second tab, and the client logs a warning naming the label and the created workspace each time it takes that create path.

The client persists that address on the `sessions` row as `backend_ref`:

```json
{"herdr":{"workspace_id":"wF","tab_id":"wF:t1","pane_id":"wF:p1","agent":"onlyne-planner-abcd1234","workspace_label":"onlyne:lab","base_pane":"wF:p1","split_direction":"right"}}
```

Spawn uses two tracks. When the first token of `session_command` matches a known agent name (`pi`, `omp`, and the rest of herdr's `--kind` table), the backend runs `herdr agent start <name> --kind <k> --pane <id> --timeout 25000 -- --session-id <id> --session-dir .pi/sessions`: `--kind` selects the executable named by token 0, and the remaining `session_command` tokens travel after the `--` separator, the call shape herdr 0.9.0 documents. Commands whose first token is absent from that table run `herdr pane run <pane_id> '<one shell line>'`. `pane run` emits no JSON. The command is `shell_quote`d into a single argv token. The client injects `ONLYNE_SOCKET`, the served adapter-socket path, into every session it spawns, so a shell inside a role pane reaches `onlyne` verbs without spelling the socket. `workspace create`, `tab create`, and `pane split` pass `--cwd` absolute, the spelling herdr resolves against its own working directory.

Split direction is `PanePlacement::from_pane_count`. `(count + 1).is_power_of_two()` maps to `right`. Remaining counts map to `down`. Ratio is `0.5`. `count` is `result.tabs[].pane_count` from `herdr tab list --workspace W`. A missing field is `0`. The production spawn path passes `placement: None`, so the backend reads that live count.

Focus issues `herdr workspace focus <workspace_id>`, then `herdr tab focus <tab_id>` (positional arguments; the tab restores its last focused pane). A managed-agent pane then takes `herdr agent focus <pane_id>`. `agent focus` accepts a managed agent. A shell pane from `pane run` answers `agent_not_found`, so that track walks `herdr pane focus --pane <base_pane> --direction <split_direction>`: the neighbour of the anchor the split recorded. `herdr pane get <pane_id>` is the confirmation step; `result.pane.focused` must be true, and a hop landing elsewhere reports the pane holding focus. The control plane is `ControlOp::Focus{task_id}`; a failed backend `focus()` records `Report::Fault{kind:"focus"}`. CLI: `onlyne control --from <role> focus --task <id>`. TUI: `F`.

A role at `max_sessions` keeps pulling with `control_only`, which is the path that lets `focus`, `recycle`, and `cancel` reach the session occupying the last free slot.

An agent that mounts naming no session — the always-running plugin — parks as the connection for the next staged session. The claim binds that socket to the session it takes, and a mount that arrives after a work item hands that item over on the spot. A connection that named no session releases only the transports sharing its socket.

### doctor

`onlyne-client doctor` is a read-only verb. It prints one JSON object and exits 0. Fields:

| field | meaning |
| --- | --- |
| `host` | selected backend name, or `null` |
| `backend_selection` | `explicit`, `env`, or `none` |
| `explicit` | raw `ONLYNE_BACKEND` when nonempty |
| `binary` | CLI path or name for herdr/orca/zellij; `null` for exec, fake, and no host |
| `session` | `HERDR_SESSION` |
| `workspace_id` | `HERDR_WORKSPACE_ID` |
| `tab_id` | `HERDR_TAB_ID` |
| `pane_id` | `HERDR_PANE_ID` |
| `refusal` | the `NO_SUPPORTED_HOST` line, present when `host` is `null` |

A missing host yields `host: null` plus `refusal` and exit 0. The verb is a pre-deploy check.

`[orca] worktree` in `config.toml` sets which Orca tab list a session tab joins. Three states:

* `host` (the default) reads `ORCA_WORKTREE_ID`, the worktree id Orca exports to the tab the supervisor started the client in and which the daemon inherits. Every session tab lands flat in that worktree's tab list, beside the supervisor's own tabs. Start the client outside an Orca tab and the variable is absent, so the policy behaves like `inherit`.
* `inherit` passes no selector and leaves the choice to Orca's active worktree.
* Any other value is used verbatim as an Orca worktree selector (`id:<…>`, `path:<abs>`, `name:<…>`, `branch:<…>`).

Tab ownership and working directory are independent. The selector decides which tab list the tab joins; the spawned command's own `cd` decides where the agent runs. The role workspace therefore never has to exist in Orca: it is not registered, not opened, and not cleaned up. That is the whole reason a generated (non-git) role workspace works at all. Orca's public registration command accepts git checkouts only, so a `path:<workspace>` selector would fail for exactly the directories this client hands out.

## Sessions

`max_sessions` from the role's spec entry caps how many sessions a role runs at once. A session whose stored lifecycle reads `exited` spends none of that cap: the rows of sessions the role has ended stay in `client.db` as its history and stay queryable. The client keeps pulling while fewer than `max_sessions` sessions have not exited. Each task gets its own session and its own spawn. A session that has finished one task takes no further task; its slot releases, its host resource closes, and it stops counting against `max_sessions`.

The host resource retires with the session: a pane, tab, zellij session, or exec child closes once that session holds no task and no plugin transport is attached. Three paths do the closing — a graceful plugin `detach` closes each idle session that connection served, a settle with no attached agent closes at settle time, and the 250 ms readiness tick closes any tracked session whose stored lifecycle projects `exited` with a stored outcome while its agent is gone, taking the reason from that outcome (`Completed`, `Fault`, or `Cancelled`). One case keeps the resource: a connection that dropped without a `detach`, where that agent may reconnect. Past `[client] reconnect_grace_secs` that agent is gone, and the sweep settles the task the session still owed `failed` and refuses that task's delivery with reason `session_dead`: the row leaves `in_flight`, so the ledger carries the ending an operator reads and `repair retry` is what brings the work back. The same pass publishes the session's own projection — the heartbeat report every ordinary ending travels — so the server's mirrored row for it reads `exited` at once, instead of reading `working` until the server's stale observer records a `stale_working` or `heartbeat_missing` fault. A retirement with the stored resource still open refreshes a stale `backend_ref` through `attach`, projects `resource_closed`, logs `retiring idle session resource` with task, backend, resource, and reason, then closes the resource and drops the slot; a close that fails is a warning.

## Server link

The client reconnects on a ladder of 1, 2, 4, 8, 16, 32, 60 seconds; 60 seconds repeats for every later attempt. After a reconnect the order is handshake, welcome, intent flush, pull resume.

A link failure or a `bye` frame sets `accept_new = false`. Queued deliveries wait on the server, running sessions continue to their terminal state, and the completions those sessions produce enter the intent queue. `accept_new = false` blocks new session spawns and new pulls. The gate follows the connection rather than any one frame: the runloop sets it from the link's readiness, and a frame that could not leave — a request past its own deadline with the link still up — goes to the intent queue alone.

A delivery the pull already had in hand when the gate shut is left unanswered: the row stays in flight and the next `hello` requeues it. A refusal would settle that row `rejected`, which is terminal, so the work would come back only through an operator's `repair retry`.

## Intent queue

Every outbound envelope lands in `client.db` `intents` before the first socket write, keyed by `op_id`.

| state | meaning | next state |
| --- | --- | --- |
| `pending` | enqueued, first attempt owed | `accepted`, `retrying`, `exhausted` |
| `retrying` | waiting for a later attempt | `accepted`, `retrying`, `exhausted` |
| `accepted` | receipt stored | terminal |
| `exhausted` | attempt ceiling reached with a recorded fault | terminal |

The role spec sets the attempt ceiling in `intent.attempts` and the delay between attempts in `intent.backoff_ms`. The queue lives in the client database, so a restart resumes it.

| answer | rule |
| --- | --- |
| `ok = true` | store the receipt, mark `accepted` |
| `duplicate` | replay the stored receipt from the first attempt |
| `acl_denied`, `invalid`, `conflict`, `forbidden`, `unknown_role`, `not_admin`, `bad_frame`, `frame_too_large`, `protocol_version` | drop the row without a retry |
| `internal`, connection loss | count the attempt and retry after the ladder |

Hitting the ceiling records fault kind `intent_exhausted` and sends `report{kind:"fault"}` once the server link exists. An exhausted row is never dropped silently.

## Role prose

`welcome.prose` is cached in `prose_cache`, keyed by role with `spec_hash`. `roles` and the cluster prose export read that record.

---

# onlyne-client（中文版 / Chinese Mirror）

一个工作区、一个角色、一个守护进程。该角色可同时运行多个会话。

## 动词

| 动词 | 一句话说明 |
| --- | --- |
| `run --workspace <dir>` | 前台角色运行时：连接、握手、拉取、分派、报告。后台运行由操作者负责，客户端自身不负责。 |
| `status --workspace <dir>` | 打印运行时长、socket 路径、已记录故障数以及服务器链接是否可用。 |
| `doctor` | 打印主机检测 JSON。无需工作区或 socket。退出码为 0。 |
| `init --workspace <dir> --role <r> --server-root <dir>` | 构建最小角色工作区并打印 `[[client]]` spec 片段。 |
| `roles --workspace <dir>` | 从本地缓存回答角色 prose。 |
| `sessions --workspace <dir>` | 为实时角色运行时保留。 |
| `watch --workspace <dir>` | 为实时角色运行时保留。 |
| `history --workspace <dir>` | 为实时角色运行时保留。 |

`run` 是唯一的启动动词，并始终留在前台。`--workspace` 接受相对路径，并在使用前将其解析为绝对路径，因此守护进程、它所生成的会话以及 herdr 的 `--cwd` 都会读取同一位置。需要让客户端在后台运行的管理器负责这一决定——可见终端标签页、`launchd`、`nohup`——客户端自身不会分离、不写 pid 文件，也没有东西按编号向其发送信号。无法绑定适配器 socket 的 `run` 会就此结束，退出码为 1，并在 stderr 指明失败原因；成功绑定后出现 `accept` 错误时，会以 `error` 级别记录（`adapter socket accept failed; retrying`），并保持监听器、每 100 ms 重试一次。
`status` 打印 `onlyne: client running uptime <n>s socket <path> faults <n>`。`<path>` 是通过所有者树读取的已提供服务 socket 路径——可以是规范的 `run/s`，也可以是深层工作区实际服务所用的短派生路径；回答 `<workspace>/.onlyne/run/socket` 也包含该信息。运行时长取自 socket 文件的存续时间；只有该 socket 回应 `admin` `hello` 时，客户端才计为运行中，因此异常退出遗留的 socket 文件会显示为未运行。作出响应的客户端若没有服务器链接，会在 stderr 附加 `onlyne: client not connected`。

打印出的 `[[client]]` 片段是完整的角色条目：其中包含 `role`、`key`、`admin`、`max_sessions`、ACL 列表、`prose` 和 `session_command`。将其粘贴到 `spec.toml` 并重新加载后，客户端便可为该角色生成会话。

## 工作区布局

`init` 和 `run` 都会在 `--workspace` 下创建以下路径：

| 路径 | 模式 | 内容 |
| --- | --- | --- |
| `.onlyne/config.toml` | | 角色、`cert_pin`、`key_path`、`plugins = [...]`、`[server]` 主机和端口、`[orca]` 工作树 |
| `.onlyne/client.db` | | SQLite：`intents`、`sessions`、`faults`、`prose_cache`、`config_cache`、`events` |
| `.onlyne/keys/role.key` | `0600` | 32 个原始 ed25519 字节，只生成一次 |
| `.onlyne/run/` | `0700` | 运行时目录 |
| `.onlyne/run/s` | `0600` | 适配器 socket 的规范拼写；路径不超过 103 字节时，`run` 将其绑定 |
| `.onlyne/run/socket` | `0600` | 一行内容，指明实际提供服务的路径——规范路径 `run/s`，或者树比绑定路径更深时位于系统临时目录下的短派生路径 |
| `.onlyne/logs/client.log` | | 操作者通过会重定向输出的 shell 启动 `run` 时的 stdout 和 stderr |
| `.onlyne/agent/<id>/` | | 包含 `plugin.toml` 的已安装插件包 |
| `.onlyne/cache/orca-tabs.jsonl` | | 仅追加的 Orca 标签页到会话映射：供管理器/显示使用的旁路信息，不是身份来源（身份由适配器协议管理） |

`init` 绝不会写入 `spec.toml`。如果工作区采用 v1 之前的布局，程序会在任何写入之前拒绝处理：退出码为 2，并逐字节输出 `onlyne: legacy workspace layout; v1.0.0 does not migrate`。

三个配置值支持 `$NAME` 写法：`cert_pin`、`key_path` 和 `[server] host`。启动时，`run` 读取以 `$` 之后名称命名的环境变量，再把其值填入配置行所在位置。网关插件也以相同方式处理平台令牌。如果某个名称对应的环境变量没有值——不存在，或存在但为空白——启动会停止，退出码为 1，并在 stderr 输出一行，同时指明字段和变量：`onlyne-client: missing secret $ONLYNE_CERT for cert_pin; set the environment variable`。不以 `$` 开头的值会原样传递，因此值中的字面量 `$` 仍是字符串的一部分。

## 退出码

| 代码 | 含义 |
| --- | --- |
| 0 | 动词已完成 |
| 1 | 动词失败；原因以一行形式写入 stderr |
| 1 | `run` 无法绑定适配器 socket；stderr 为 `onlyne-client: bind the workspace socket <canonical path>: <detail>`，详情指明所服务的路径、两个字节长度和 OS 原因 |
| 2 | `status` 未发现客户端回应其 socket，打印 `onlyne: client not running` |
| 2 | `status` 发现客户端没有服务器链接，打印 `onlyne: client not connected` |
| 2 | 工作区采用旧版布局 |
| 5 | `run` 未选择主机；stderr 为 `onlyne: no supported host detected; run inside herdr, orca, or zellij, or set ONLYNE_BACKEND` |

只有客户端正在运行且已连接到服务器时，`status` 才退出 0。脚本读取的就是这一事实。
`doctor` 对每一种主机检测结果（包括 `host: null`）都退出 0。

## 后端

选择优先级为环境变量 `ONLYNE_BACKEND`（非空）> 工作区 `config.toml` 的 `backend` > auto。

| 名称 | 解析别名 | 选择方式 | 备注 |
| --- | --- | --- | --- |
| `herdr` | | 环境变量、配置或 auto 探测（首个） | 窗格主机 |
| `orca` | | 环境变量、配置或 auto 探测 | 标签页主机 |
| `zellij` | | 环境变量、配置或 auto 探测 | 窗格主机；探测会映射 EXITED / `exit_status` |
| `exec` | `headless` | 仅环境变量或配置 | 投影将后端记录为 `exec` |
| `fake` | | 仅环境变量或配置 | 进程内运行，用于测试 |
| `auto` | 空字符串 | 环境变量和配置均为空时的默认值 | 依次探测 herdr、orca、zellij |

非空值若为 `herdr`、`orca`、`zellij`、`exec`/`headless` 或 `fake`，就会选择相应后端。auto 从不发现 `exec` 和 `fake`。没有匹配项时，`onlyne-client run` 退出 5，并写入 `onlyne: no supported host detected; run inside herdr, orca, or zellij, or set ONLYNE_BACKEND`。

`fake` 在进程内运行会话，无需外部工具；端到端脚本会设置 `ONLYNE_BACKEND=fake`。`exec` 将角色的 `session_command` 作为客户端的子进程生成，保持 stdin 打开，并把子进程输出追加到 `.onlyne/logs/session-<task>.log`。子进程退出时，`probe` 可以填充 `detail.output_tail`（最多 200 行 / 16 KiB）。`crates/onlyne-testkit/e2e/pi-live.sh` 和 `exec-headless.sh` 会设置此路径。在 Windows 上关闭时，使用 `CREATE_NEW_PROCESS_GROUP` 加 `CTRL_BREAK`，然后 `kill`；没有控制台的进程会直接终止子进程。面向操作者、用于优雅停止守护进程的命令是 `onlyne shutdown`。

### herdr

herdr 会话从客户端进程环境继承；在窗格中运行的 pi 子进程也会继承它。一个服务器根节点/拓扑对应一个标签为 `onlyne:<cluster>` 的 herdr 工作区。`<cluster>` 是服务器自身的 `[server] name`；客户端从 `welcome.cluster` 读取它，并作为 `ONLYNE_CLUSTER` 传给所创建的每个窗格。一个角色对应一个标签页。一个 onlyne 会话对应一个窗格。关闭方式是 `herdr pane close`；收到 `pane_not_found` 表示关闭成功，并以 debug 级别记录 `herdr pane already closed`。Id 形如 `wF`、`wF:t1`、`wF:p1`。诸如 `onlyne-test` 的命名会话就是客户端环境中已有的 `HERDR_SESSION` 值。后端以标签 `onlyne:<cluster>` 寻址工作区，以角色自身名称寻址标签页。需要使用特定工作区或标签页的操作者，应在客户端生成会话前重命名它：`herdr workspace rename <WORKSPACE_ID> onlyne:<cluster>` 和 `herdr tab rename <TAB_ID> <role>`。工作区标签不同会创建第二个工作区，标签页名称不同会创建第二个标签页；每次走创建路径时，客户端都会记录警告，指明该标签和所创建的工作区。

客户端将该地址作为 `backend_ref` 持久化在 `sessions` 行上：

```json
{"herdr":{"workspace_id":"wF","tab_id":"wF:t1","pane_id":"wF:p1","agent":"onlyne-planner-abcd1234","workspace_label":"onlyne:lab","base_pane":"wF:p1","split_direction":"right"}}
```

生成进程使用两条路径。当 `session_command` 的第一个词元与已知代理名称（`pi`、`omp` 以及 herdr `--kind` 表中的其余名称）匹配时，后端运行 `herdr agent start <name> --kind <k> --pane <id> --timeout 25000 -- --session-id <id> --session-dir .pi/sessions`：`--kind` 选择词元 0 指定的可执行文件，`session_command` 的其余词元位于 `--` 分隔符之后；这是 herdr 0.9.0 记录的调用形式。第一个词元不在该表中的命令会运行 `herdr pane run <pane_id> '<one shell line>'`。`pane run` 不发出 JSON。命令经 `shell_quote` 处理，成为单个 argv 词元。客户端会把自己生成的每个会话都注入 `ONLYNE_SOCKET`（所提供服务的适配器 socket 路径），这样角色窗格内的 shell 无需写明 socket 路径即可调用 `onlyne` 动词。`workspace create`、`tab create` 和 `pane split` 接收绝对路径形式的 `--cwd`，herdr 会相对于自身工作目录解析它。

拆分方向由 `PanePlacement::from_pane_count` 决定。`(count + 1).is_power_of_two()` 对应 `right`，其余计数对应 `down`。比例为 `0.5`。`count` 来自 `herdr tab list --workspace W` 的 `result.tabs[].pane_count`。字段缺失时取 `0`。生产环境中的生成路径传入 `placement: None`，因此后端会读取实时计数。

聚焦会依次发出 `herdr workspace focus <workspace_id>` 和 `herdr tab focus <tab_id>`（位置参数；标签页会恢复上次聚焦的窗格）。对于托管代理窗格，接着执行 `herdr agent focus <pane_id>`。`agent focus` 接受托管代理。由 `pane run` 创建的 shell 窗格会回答 `agent_not_found`，因此该路径执行 `herdr pane focus --pane <base_pane> --direction <split_direction>`：沿拆分时记录的锚点方向移动到相邻窗格。`herdr pane get <pane_id>` 是确认步骤；`result.pane.focused` 必须为 true，如果跳转落在其他位置，则报告当前持有焦点的窗格。控制面是 `ControlOp::Focus{task_id}`；后端 `focus()` 失败会记录 `Report::Fault{kind:"focus"}`。CLI：`onlyne control --from <role> focus --task <id>`。TUI：`F`。

角色达到 `max_sessions` 后，仍会通过 `control_only` 拉取；这条路径可让 `focus`、`recycle` 和 `cancel` 到达占用最后一个空槽位的会话。

挂载时未指定会话的代理——即始终运行的插件——会作为下一个待定会话的连接等待。认领会把该 socket 绑定到所接管的会话；工作项之后才到达的挂载会立即移交给该工作项。未指定会话名称的连接只释放共享其 socket 的传输。

### doctor

`onlyne-client doctor` 是只读动词。它打印一个 JSON 对象并退出 0。字段如下：

| 字段 | 含义 |
| --- | --- |
| `host` | 所选后端名称，或 `null` |
| `backend_selection` | `explicit`、`env` 或 `none` |
| `explicit` | 非空时的原始 `ONLYNE_BACKEND` |
| `binary` | herdr/orca/zellij 的 CLI 路径或名称；对于 exec、fake 和无主机情况为 `null` |
| `session` | `HERDR_SESSION` |
| `workspace_id` | `HERDR_WORKSPACE_ID` |
| `tab_id` | `HERDR_TAB_ID` |
| `pane_id` | `HERDR_PANE_ID` |
| `refusal` | `NO_SUPPORTED_HOST` 行；当 `host` 为 `null` 时存在 |

未检测到主机会得到 `host: null` 和 `refusal`，并退出 0。该动词用于部署前检查。

`config.toml` 中的 `[orca] worktree` 决定会话标签页加入哪个 Orca 标签页列表。三种状态为：

* `host`（默认）读取 `ORCA_WORKTREE_ID`，即 Orca 导出到管理器启动客户端所在标签页、并由守护进程继承的工作树 id。每个会话标签页都会平铺到该工作树的标签页列表中，与管理器自身的标签页并列。如果在 Orca 标签页外启动客户端，该变量不存在，因此此策略的行为与 `inherit` 相同。
* `inherit` 不传选择器，将选择交给 Orca 的活动工作树。
* 任何其他值都会原样用作 Orca 工作树选择器（`id:<…>`、`path:<abs>`、`name:<…>`、`branch:<…>`）。

标签页归属与工作目录彼此独立。选择器决定标签页加入哪个标签页列表；所生成命令自身的 `cd` 决定代理的运行位置。因此，角色工作区从不需要存在于 Orca：它不会被注册、打开或清理。这正是生成式（非 git）角色工作区能够工作的原因。Orca 的公共注册命令只接受 git 检出，所以 `path:<workspace>` 选择器恰好会因本客户端分发的目录而失败。

## 会话

角色 spec 条目中的 `max_sessions` 限制该角色同时运行的会话数量。存储的 lifecycle 状态为 `exited` 的会话不占该配额：角色已结束会话对应的行作为历史保留在 `client.db` 中，仍可查询。只要尚未退出的会话少于 `max_sessions`，客户端就会继续拉取。每个任务都有自己的会话和自己的生成过程。完成一个任务的会话不再接收任务；其槽位释放，主机资源关闭，并且不再计入 `max_sessions`。

主机资源随会话退役：当会话不再持有任务且没有插件传输连接时，窗格、标签页、zellij 会话或 exec 子进程就会关闭。共有三条关闭路径——插件正常 `detach` 会关闭该连接服务过的每个空闲会话；结算时没有已连接代理，则在结算时关闭；250 ms 就绪检查则在代理消失且存储的 lifecycle 投影为 `exited`、并带有存储 outcome 时，关闭任何被跟踪的会话，原因取自该 outcome（`Completed`、`Fault` 或 `Cancelled`）。有一种情况会保留资源：连接在未执行 `detach` 的情况下中断，因为该代理可能重连。超过 `[client] reconnect_grace_secs` 后，即认为该代理已经消失；清理流程会将会话仍欠下的任务结算为 `failed`，并以 `session_dead` 为原因拒绝对应任务交付：该行会离开 `in_flight`，因此账本保留了操作者可见的结束结果，而工作只能由 `repair retry` 重新带回。同一次处理还会发布会话自身的投影——每次正常结束都会发送的心跳报告——因此服务器上镜像的行会立即读取为 `exited`，无需在服务器的过期观察器记录 `stale_working` 或 `heartbeat_missing` 故障之前一直读取为 `working`。如果退役时存储的资源仍处于打开状态，会通过 `attach` 刷新过期的 `backend_ref`，投影 `resource_closed`，以任务、后端、资源和原因为由记录 `retiring idle session resource`，然后关闭资源并释放槽位；关闭失败只产生警告。

## 服务器链接

客户端按 1、2、4、8、16、32、60 秒的阶梯重连；之后每次尝试都重复 60 秒间隔。重连后的顺序为握手、welcome、intent 刷新、恢复拉取。

链接失败或收到 `bye` 帧会设置 `accept_new = false`。队列中的交付会在服务器等待，正在运行的会话继续到达终态，这些会话产生的完成事件则进入 intent 队列。`accept_new = false` 会阻止生成新会话和执行新拉取。此开关跟随连接，而非某一个帧：运行循环根据链接就绪状态设置它；无法发出的帧——请求超过自身期限但链接仍在时——只进入 intent 队列。

开关关闭时，拉取已经持有的交付不会得到回答：该行保持飞行状态，下一次 `hello` 会重新入队。拒绝会将该行结算为 `rejected`，这是终态，因此工作只能通过操作者执行 `repair retry` 再次返回。

## Intent 队列

每个出站 envelope 都会以 `op_id` 为键，在首次 socket 写入之前落入 `client.db` 的 `intents`。

| 状态 | 含义 | 下一状态 |
| --- | --- | --- |
| `pending` | 已入队，尚需首次尝试 | `accepted`、`retrying`、`exhausted` |
| `retrying` | 等待后续尝试 | `accepted`、`retrying`、`exhausted` |
| `accepted` | 已存储回执 | 终态 |
| `exhausted` | 达到尝试次数上限并记录故障 | 终态 |

角色 spec 通过 `intent.attempts` 设置尝试次数上限，通过 `intent.backoff_ms` 设置尝试之间的延迟。队列位于客户端数据库中，因此重启后会恢复。

| 回答 | 规则 |
| --- | --- |
| `ok = true` | 存储回执，标记为 `accepted` |
| `duplicate` | 重放首次尝试时存储的回执 |
| `acl_denied`、`invalid`、`conflict`、`forbidden`、`unknown_role`、`not_admin`、`bad_frame`、`frame_too_large`、`protocol_version` | 删除该行，不重试 |
| `internal`、连接丢失 | 计入此次尝试，并按阶梯延迟后重试 |

达到上限会记录 `intent_exhausted` 故障类型，并在服务器链接存在后发送一次 `report{kind:"fault"}`。已耗尽的行绝不会被静默丢弃。

## 角色 prose

`welcome.prose` 会缓存在 `prose_cache` 中，以角色为键，并带有 `spec_hash`。`roles` 和集群 prose 导出会读取该记录。
