# Onlyne Sessions（Orca 插件）

Orca 桌面里的**只读 role 会话管理器**。零配置发现每个带
`<role workspace>/.onlyne/cache/orca-tabs.jsonl` 的 Orca worktree，把三方 join 成一块看板：

```
Orca worktree 列表  ──┐
mapping 文件         ──┼─→ 看板：role → task → tab 活体 → 会话状态
orca terminal list   ──┤
onlyne client socket ──┘
```

- **pluginApi v1**（Orca 1.4.198 起），API 仍标 EXPERIMENTAL。
- **零 npm 依赖**，只用 Node 内置模块。
- **只读纪律**：不建/不关/不改名任何 tab，不写任何 onlyne 文件，也不写自己的安装目录；
  唯一的写操作是你在命令面板主动触发 `onlyne-sessions.focus` 时的 `orca terminal switch`。

---

## 1. 安装（人工，三条命令面都没有 CLI 安装入口）

1. 打开 **Settings → Plugins → Install plugin**，选 **Local path** 页，填本目录的绝对路径
   （例如 `<repo>/integrations/orca-plugin`），点安装。
   - Orca 会把目录拷成 `<userData>/plugins/onlyne.onlyne-sessions/<content-hash>/`，
     并写 `current` 指针、lock、provenance。之后每次 refresh 都会校验该目录的内容哈希，
     **所以装完不要手改安装目录里的文件**；要改就改源目录再重装一次。
   - 也可以走 **Development** 区：把源目录加成 dev path（边改边生效，仍然要授权）。
2. 在列表里**启用**这个插件。
3. 授权（consent）——本插件只申请三项，逐项如下（Orca 的原话）：

   | capability | Orca 的说明 | 本插件拿它做什么 |
   | --- | --- | --- |
   | `workspace:read` | Read the name, branch, and terminal list of your focused worktree | 面板里的「当前工作区」读取按钮 |
   | `notifications:show` | Show desktop notifications labeled with the plugin name | 推送看板、跳转成功/失败、取上下文 |
   | `events:subscribe` | Get notified when worktrees are created or removed and when agent status changes | 事件驱动的 2s 防抖重扫 |

   未授权的降级：没有 `events:subscribe` → 不订阅事件（命令仍能用，日志会写原因）；
   没有 `notifications:show` → 通知静默进插件日志。本插件**不申请** `terminal:send`。

4. 可选配置（零配置也能跑）：`~/.config/onlyne-sessions/config.json`

   ```json
   { "orcaBin": "/opt/homebrew/bin/orca", "onlyneBin": "/path/to/v1.0.0/onlyne" }
   ```

   为什么可能需要它：plugin worker 的环境被 Orca 洗白（只保留 `PATH`/`HOME`/`LANG` 等
   16 项），从 Dock 启动的 Orca 常常没有 homebrew 的 PATH。插件按
   `PATH → /opt/homebrew/bin → /usr/local/bin → ~/.local/bin → ~/bin → ~/.cargo/bin` 找二进制，
   找不到就在日志里说明并降级。`BIN_DIR`（仓库 e2e 的约定）在文件确实存在时优先于自动发现，
   所以对刚构建的二进制跑冒烟是
   `BIN_DIR=target/debug node tools/smoke.mjs`。**注意**：`onlyne` 0.6.0（旧 CLI）不认识
   `sessions` 动词，会以 `cli_surface_mismatch` 降级；v1.0.0 的 CLI 通常在
   `target/debug/onlyne`，用上面的 `onlyneBin` 指过去即可。

## 2. 命令（命令面板里搜 “Onlyne Sessions”）

| 命令 | 行为 | 参数 |
| --- | --- | --- |
| `onlyne-sessions.board` | 立即推送一块看板（通知 + 插件日志） | 无 |
| `onlyne-sessions.refresh` | 重扫一遍再推送 | 无 |
| `onlyne-sessions.focus` | 唯一命中时 `orca terminal switch` 跳到那个 tab | `args.task`：task 前缀 |
| `onlyne-sessions.copy-agent-context` | 给出 `pane_key`/`handle`/`orca selector` 三件套 | `args.task`：task 前缀 |

**参数边界（实测）**：Orca 的命令面板调用插件命令时**不传参数**（`plugin-command-execution.ts`
只传 `pluginKey`/`commandId`）。所以：

- 不带前缀时，`focus` / `copy-agent-context` 只在**恰好一个活 tab**的情况下生效；
  多个（或没有）命中会推一条通知列出候选，而不是瞎猜。
- 需要带前缀时用 RPC/IPC 面调用（返回结构化结果）：

  ```json
  { "pluginKey": "onlyne.onlyne-sessions",
    "commandId": "onlyne-sessions.focus",
    "args": { "task": "task8" } }
  ```

- `focus` 会切前台焦点（`orca terminal switch` 的副作用），只在你自己触发时发生。
- `copy-agent-context` 的“复制”是通知形式：pluginApi v1 **没有剪贴板 host 方法**，
  也没有 host 侧的拷贝能力，所以通知里给出三件套文本 + 结构化结果，手工复制即可。

## 3. 看板长什么样 / 去哪儿看

```
Onlyne sessions · 2 roles · 3 live tabs · 1 working
planner  (2/3 live)
  ● task8a1b · working/running · 12s · 45e603f7:b6d067b6
  ○ task9c2d · idle/gone · 3m · 970e3c58:17fc0aac
builder  (1/1 live)
  ● task7f3e · idle · 5s · 24d62468:4ce346ca
```

- summary 行：`N roles · M live tabs · K working`（working = `public_lifecycle=working` 或 `agent=running`）。
- 行：`task 短形 · lifecycle/agent（拿不到 client socket 时退化为 mapping 的 state）· lastOutputAt 相对时间 · pane_key 短形`。
- 图例：`●` 活 tab（`connected` 且 mapping `state=spawned`）· `○` 只有 mapping 行（tab 不在了）·
  `✕` worktree 已移除（保留 10 分钟便于对照）。
- 空态一句话：**还没有 role 工作区：等 supervisor 拉起 role client**（role 工作区里要出现
  `.onlyne/cache/orca-tabs.jsonl`）。

看板出现在三个地方：

1. **桌面通知**：结构性变化（role/task/tab 活体/worktree 移除）时推一条，30s 冷却；
2. **设置 → Plugins → 该插件的日志**：每次变化写一行 summary，`board` 命令写整块看板；
3. **命令返回**：`plugins.invokeCommand` 的调用方能拿到结构化看板（JSON），命令面板会丢弃返回值。

### 面板是静态的（这条很重要）

Orca 1.4.198 的插件面板是一个 `srcdoc` 沙箱文档：CSP `default-src 'none'; connect-src 'none'`
（不能 fetch），并且只能调用三个 host 方法
（`workspace.readContext` / `terminal.sendText` / `notifications.show`），
不能读 worker 状态、不能调用插件命令，宿主也不会把插件数据推进去
（`plugin-host-api.ts` 的 `PLUGIN_PANEL_ACTIONS`、`plugin-panel-bridge.ts:42` 的 schema refine、
`plugin-capabilities.ts` 的 9→7 个 capability）。面板只在**打开时**和**插件刷新事件**时重新读取一次
入口 HTML，而入口文件是哈希寻址的不可变文件。

所以本插件的面板是**静态仪表盘**：图例 + 安装/授权状态 + 行为边界 + 一个「当前工作区」实时读取
按钮 + 四个「去命令面板跑某条命令」的按钮。**实时映射走通知与插件日志**，
这不是偷懒，是 pluginApi v1 的面板边界；面板里写明了这一点。

## 4. 数据契约（本插件如何读）

`<role workspace>/.onlyne/cache/orca-tabs.jsonl`（append-only，一行一 JSON）：

```json
{"pane_key":"<tabId:leafId>","handle":"term_<uuid>","task_id":"…","session_id":"…",
 "role":"planner","worktree_selector":"path:/abs/ws","title":"onlyne:<task_id>",
 "state":"spawned|closed","updated_at":"<rfc3339>"}
```

- 同一 `pane_key` **后行覆盖前行**；`state:"closed"` 是墓碑，不再出现在看板上。
- 文件不存在（fake/zellij backend，或非 role 工作区）→ 该 worktree 直接被跳过，不算错误。
- join 规则：
  - `pane_key`：Orca 1.4.198 的 `terminal list` 行里**没有** `paneKey` 字段，插件用
    `${tabId}:${leafId}` 现拼；mapping 行按同一个拼法匹配（先按 `pane_key`，再退化按 `handle`）。
  - session：先按 `task_id`，再按 `session_id` 匹配；**不按 role 猜**（一个 role 会有多个 task）。
  - 会话状态只从**该 workspace 自己的 client socket**（`<ws>/.onlyne/run/s`）读，并且必须显式
    指定 surface：`onlyne --socket <ws>/.onlyne/run/s --as client sessions --json`。2026-09-11 用
    `target/debug/onlyne` 实测：不写 `--as client` 时 CLI 会因为 `.onlyne/run/s` 后缀把 surface
    推断成 admin，把 admin 帧发给 client socket，socket 直接关连接
    （`{"ok":false,"error":{"code":"internal","message":"socket closed before an answer arrived"}}`，
    退出码 1）；写上 `--as client` 同一命令回 `{"ok":true,"data":{"sessions":[…]}}`，而 server link
    断开时回 `{"ok":false,"error":{"code":"internal","message":"the connection is not ready"}}`，
    按下面的降级矩阵退化为 mapping 的 `state`。

### 降级矩阵（都不算失败，会写进通知/日志）

| 情况 | 结果 |
| --- | --- |
| mapping 文件不存在 | 该 worktree 不是 role 工作区，跳过 |
| `orca terminal list` 失败 | 该 workspace 的行保留，`terminal=null`，看板记 `○` + 错误写日志 |
| `<ws>/.onlyne/run/s` 不存在 | 不调 onlyne，`state` 列退化为 mapping 的 `state` |
| onlyne CLI 不认识 `sessions`（如 0.6.0） | `cli_surface_mismatch`，同样退化 |
| `onlyne` 二进制找不到 | `missing_binary`，同样退化 |
| `orca worktree list` 失败 | 整块看板为空 + 通知/日志里带错误码 |

## 5. 已知边界

- **人工安装**：pluginApi v1 没有 CLI 安装面（`plugins:install` 只有桌面 IPC / serve RPC），
  装/启/授权都得在桌面点。
- **实验 API**：`pluginApi` 尚未冻结；Orca 升级后先跑一次「重新扫描」确认 join 还成立。
- **worker 会被回收**：5s 兜底重扫只在 worker 活着时跑；下一个事件或命令会把 worker 重新拉起。
  因此「事件驱动 + 兜底」不是硬实时保证。
- **不碰生命周期**：spawn/close/rename 属于 onlyne backend（防双主），本插件一律不碰。
- **不写任何文件**：包括自己的安装目录（哈希校验）与 onlyne 的 cache。
- **JSON 兼容**：mapping 里的未知字段被忽略；`state` 只对 `spawned`/`closed` 做语义解释，
  其它值原样显示且不算 live。

## 6. 卸载 / 升级

- **禁用**即可：worker 停 → 不扫、不通知、不订阅事件（面板还在，但读不到数据即报错）。
- **卸载**：Settings → Plugins 里卸载，Orca 会删掉 `<userData>/plugins/onlyne.onlyne-sessions`
  （hash 目录 + `current` 指针）。本插件没有在别处留状态（无 storage、无 secrets、无设置写入）。
- **升级**：改源目录 → 重新 Install（新 hash 目录）→ 能力指纹变了才需要重新授权。

## 7. 开发与验证

```
integrations/orca-plugin/
  orca-plugin.json     manifest（pluginApi 1）
  main.mjs             worker 入口：activate/deactivate + 四个命令 + 三个事件
  panel.html           静态面板（沙箱文档）
  src/
    runner.mjs         可注入 exec、CLI JSON 解析（含 stdout 里的错误体）
    orca-cli.mjs       worktree list / terminal list / terminal switch / status
    onlyne-cli.mjs     --socket <ws>/.onlyne/run/s sessions，失败归一化
    mapping.mjs        append-only + 覆盖语义 + 墓碑
    discover.mjs       发现 → join → 看板模型（含 graveyard 灰显）
    render.mjs         文本渲染（通知/日志共用）
    commands.mjs       refresh / board / focus / copy-agent-context
    board-state.mjs    防抖、5s 兜底、结构指纹、通知冷却
    *.test.mjs         node:test（75 用例，零依赖）
  tools/smoke.mjs      真机只读冒烟（带突变守卫）
```

```bash
node --test                     # 75 pass
BIN_DIR=target/debug node tools/smoke.mjs
                                # 真机只读：orca status / worktree list / 发现 / join demo
```

冒烟脚本的 runner 会拦截任何 `terminal switch|create|close|rename|send` 与
`worktree create|rm`，一旦出现就退出码 1 —— 也就是说「跑一次冒烟」本身证明不了会动你的 tab。

## 8. 给 onlyne backend 的契约核对

按收到的契约逐字实现，以下是实现时才暴露出来、需要 backend 确认的点（不改契约，只报告）：

1. **`pane_key` 的拼法**：Orca 1.4.198 的 `terminal list` 行里没有 `paneKey`，
   插件用 `${tabId}:${leafId}` 现拼。如果 backend 写 mapping 时用的是别的拼法，join 会静默断
   （表现为看板全部 `○`）。
2. **`state` 词表**：插件只把 `spawned`/`closed` 当语义值（liveness / 墓碑），其它值原样显示；
   如果 backend 以后换成 `running`/`attached` 之类，需要同步。
3. **墓碑的作用域**：`state:"closed"` 被当作“该 pane_key 的当前状态”，
   而不是“这一行的历史事件”——同 pane_key 后续再有 `spawned` 行会让它重新出现（符合 append-only 语义）。
4. **session 匹配键**：只有 `task_id` / `session_id` 两个键能对上；
   两个都缺或与 server 侧不一致时，看板退化为 mapping 的 `state` 而不是猜。
5. **`worktree_selector` 只做展示**：寻址用 Orca worktree 行自己的 `path`
   （`path:<abs>`），因为只有已注册的 worktree 才能被 `terminal list` 定位。
6. **额外字段**：`tab_id`/`leaf_id`/`worktree_id` 之类的额外字段目前被忽略（不影响 join）。
7. **role 缺失**：mapping 行的 `role` 为空时看板归到 `(unknown role)` 分组。
