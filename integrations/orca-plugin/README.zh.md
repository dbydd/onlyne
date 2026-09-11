# Onlyne Sessions（Orca 插件）

Orca 桌面里的**只读 supervisor 看板**。它把两条彼此独立的轴合成一块看板：

```
   orca terminal list --json                        ──┐   所有 worktree 的所有 tab，
   （一次平铺调用，不带 --worktree）                    │   按 Orca 自己的顺序
                                                      │
   每个配置的 serverRoots[i]：                         ├─→  看板：root → role → task → tab
   onlyne --server-root <S> sessions --json           │   （标题正好等于 `onlyne:<task_id>` 的
   onlyne --server-root <S> roles    --json         ──┘    那个 tab 会标注到对应 task 行上）
```

- **pluginApi v1**（Orca 1.4.198 起），API 仍标 EXPERIMENTAL。
- **零 npm 依赖**，只用 Node 内置模块。
- **只读纪律**：不建/不关/不改名任何 tab，不写任何 onlyne 文件；正式安装下连自己的安装目录也不写。
  唯一的 **Orca** 写操作是你在命令面板主动触发 `onlyne-sessions.focus` 时的 `orca terminal switch`；
  唯一的文件写是自己 dev 树里的 `panel.html`（面板的数据通道，见 §3）。
- **面板就是看板**（dev 安装，Settings → Plugins → Development）：刷新用的是通知那套
  2s 防抖 / 5s 兜底。正式安装显示的是安装当时的快照，实时看板退到通知 + 插件日志。

**权威在哪。** 会话身份属于 onlyne 的 adapter / pi 插件协议。这块看板只是 supervisor 的方便视图：
它照抄 server root 的 admin 面，并用一个**任何进程都能抢走**的标题约定把 tab 标注上去（见 §4）。
标注错了或没标注，都不构成关于会话的证据。

插件不再发现 role 工作区，也不再读任何 backend 缓存文件：backend 已经不为每个 role 注册 Orca
worktree，所有会话 tab 都平铺在宿主 worktree 的列表里，而唯一的会话来源是 admin 面。

---

## 1. 安装（人工，三条命令面都没有 CLI 安装入口）

1. **添加插件**，两个入口二选一（第一个才是实时面板的前提）：

   - **Development（推荐）** —— Settings → Plugins → Development → 填本目录的绝对路径
     （例如 `<repo>/integrations/orca-plugin`）。热重载就是面板的数据通道：worker 把看板写进该
     目录的 `panel.html`，dev watcher 察觉变化，面板随之重载（见 §3「面板就是看板」）。
   - **Install plugin（降级）** —— Settings → Plugins → Install plugin → **Local path** → 同一个
     目录。Orca 会把目录拷成 `<userData>/plugins/onlyne.onlyne-sessions/<content-hash>/`，写
     `current` 指针、lock、provenance；那份目录内容寻址、每次加载面板都会校验哈希，所以 worker
     **不写、也不会写**它。此时面板显示的是安装当时的快照，实时看板走通知与插件日志。
     要推进正式安装就重装一次；**别手改安装目录里的文件**。

2. 在列表里**启用**这个插件。
3. 授权（consent）——本插件只申请两项，逐项如下（Orca 的原话）：

   | capability | Orca 的说明 | 本插件拿它做什么 |
   | --- | --- | --- |
   | `notifications:show` | Show desktop notifications labeled with the plugin name | 推送看板、跳转成功/失败、取上下文 |
   | `events:subscribe` | Get notified when worktrees are created or removed and when agent status changes | 事件驱动的 2s 防抖重扫 |

   未授权的降级：没有 `events:subscribe` → 不订阅事件（命令仍能用，日志会写原因）；
   没有 `notifications:show` → 通知静默进插件日志。本插件**不申请** `terminal:send`，
   也不申请 `storage`/`secrets`/`settings:own`——它不保存自己的任何状态。

4. 可选配置（不写也能跑）：`~/.config/onlyne-sessions/config.json`

   ```json
   {
     "serverRoots": ["/abs/path/to/server-root", "/abs/path/to/second-cluster"],
     "piWorkspaces": ["/abs/path/to/role-workspace", "/abs/path/to/another-role-workspace"],
     "orcaBin": "/opt/homebrew/bin/orca",
     "onlyneBin": "/path/to/v1.0.0/onlyne"
   }
   ```

   - `serverRoots` 就是 session 轴：一项一个 onlyne server root，寻址方式是
     `onlyne --server-root <S> …`（`<S>/.onlyne/run/s` 是该 root 的 admin socket）。
     **缺失或空数组都是合法状态**——看板只渲染平铺的 tab 轴，完全不会调用 `onlyne`。
     条目会被 trim 并去重。
   - `piWorkspaces` 是 swarm 归属的权威来源（§2 轴 A）：一项一个 role workspace，该 workspace 里的
     pi 适配器可以在 `<workspace>/.onlyne/cache/pi-pane.json` 申报自己所在的 pane。它**不是**
     `serverRoots`——workspace 是 `onlyne client run` 的运行目录，看板无法从 server root 推出来。
     缺失或空数组都是合法状态：tab 轴退回 worktree 启发式。同样 trim 并去重。
   - 为什么可能需要钉死二进制：plugin worker 的环境被 Orca 洗白（只保留 `PATH`/`HOME`/`LANG`
     等 16 项），从 Dock 启动的 Orca 常常没有 homebrew 的 PATH。插件按
     `PATH → /opt/homebrew/bin → /usr/local/bin → ~/.local/bin → ~/bin` 找二进制，
     找不到就在日志里说明并降级。`BIN_DIR`（仓库 e2e 的约定）在文件确实存在时优先于自动发现，
     所以对刚构建的二进制跑冒烟是 `BIN_DIR=target/debug node tools/smoke.mjs`。
   - **`onlyne` 0.6.0（旧 CLI）不认识 `--server-root`/`sessions`**，会以 `cli_surface_mismatch`
     降级；把 `onlyneBin` 指到 v1.0.0 的二进制（通常在 `target/debug/onlyne`）即可。

## 2. 命令（命令面板里搜 “Onlyne Sessions”）

| 命令 | 行为 | 参数 |
| --- | --- | --- |
| `onlyne-sessions.refresh` | 重扫一遍再推送 | 无 |
| `onlyne-sessions.board` | 立即推送一块看板（通知 + 插件日志） | 无 |
| `onlyne-sessions.debug-board` | 把面板内嵌的那份看板 JSON 写到 `/tmp/onlyne-board.json`，再通知一行摘要 | `args.path`：目标路径，默认 `/tmp/onlyne-board.json` |
| `onlyne-sessions.focus` | 唯一命中时 `orca terminal switch` 跳到那个 tab | `args.task`：task id 或 pane 前缀 |
| `onlyne-sessions.copy-agent-context` | 给出 `pane_key`/`handle`/`orca selector` 三件套 | `args.task`：task id 或 pane 前缀 |

**参数边界（实测）**：Orca 的命令面板调用插件命令时**不传参数**（`plugin-command-execution.ts`
只传 `pluginKey`/`commandId`）。所以：

- 不带前缀时，`focus` / `copy-agent-context` 只在**恰好一个活 tab**的情况下生效；
  多个（或没有）命中会推一条通知列出候选，而不是瞎猜。
- 前缀可以是 **task id**（`task8a1b…`）或 **pane 前缀** `<tabId>:<leafId>`；看板打印的
  `tab8:leaf8` 短形也接受——从看板上抄下来的东西能直接用。
- 需要带前缀时用 RPC/IPC 面调用（返回结构化结果）：

  ```json
  { "pluginKey": "onlyne.onlyne-sessions",
    "commandId": "onlyne-sessions.focus",
    "args": { "task": "task8" } }
  ```

- `focus` 会切前台焦点（`orca terminal switch` 的副作用），只在你自己触发时发生。
- `copy-agent-context` 的“复制”是通知形式：pluginApi v1 **没有剪贴板 host 方法**，
  也没有 host 侧的拷贝能力，所以通知里给出三件套文本 + 结构化结果，手工复制即可。
  `orca selector` 取自 tab 行自己的 `worktreeId`；Orca 没报这个字段时，该行不输出。

## 3. 看板长什么样 / 去哪儿看

```
Onlyne sessions · 2 roots · 3 roles · 7 tabs (4 live) · 9 sessions (2 working)
/srv/onlyne-a  (3 roles · 9 sessions · 2 working)
  planner  (online · 2 tasks · 1 live)
    ● task8a1b · working/running · 12s · 45e603f7:b6d067b6
    ○ task9c2d · idle/gone · 无 tab · 3m · —
  builder  (offline · 0 tasks · 0 live)
/srv/onlyne-b  (0 roles · 0 sessions · 0 working)
  ! sessions: cli_error — onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace
  ! roles: cli_error — onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace
未 join 的 tab (2)
  ● tab · title=Pi ready · 5s · 470e41ba:31f6d4b1 · wt 53e59790
  ○ tab · title=zsh · 3m · 794041dc:7c648f42 · wt 2ea2fe23
```

- summary 行：`N roots · N roles · N tabs (M live) · N sessions (K working)`；
  working = `public_lifecycle=working` 或 `agent=running`。
- root 行：该 root 的 role 分节数、session 数与 working 数；某个 root 连不上时是 0 加上它自己的
  `!` 行，而不会把整块看板拖垮。
- role 分节行：该 role 的 presence（`online`/`offline`/`draining`，只有 session 提到它时是
  `no role row`）+ `N tasks · M live`。
- 行：`task 短形 · lifecycle/agent（server 给了 outcome 时带上）· lastOutputAt 相对时间
  （没有就退化为 session 的 updatedAt）· pane_key 短形`；没有 tab 的 task 行多一个 `无 tab`，
  pane 位显示 `—`。
- 未 join 的 tab 行：`tab · title=<原始标题> · lastOutputAt 相对时间 · pane_key 短形 · wt <worktree>`。
- 图例：`●` 活（`connected=true`）· `○` 没连上，或 tab 轴没有列出的行。
- 空态一句话：**既没有 `serverRoots` 也没有 Orca tab**——去配置文件里加 `serverRoots`。

看板出现在四个地方：

1. **面板**（右侧栏 `Onlyne Sessions`）——dev 安装下就是实时看板，见下节。会话终止不干净的行
   （`outcome=fault/cancelled`）标 `✕`；快照超过一个节奏没更新时年龄变灰。
2. **桌面通知**：结构性变化（roots/roles/task/tab 活体、某个 root 掉线或恢复）时推一条，30s 冷却；
3. **Settings → Plugins → 本插件的日志**：每次变化一行摘要，`board` 命令给整块看板，
   另外记下面板文档的写回结果；
4. **命令返回**：`plugins.invokeCommand` 的调用方能拿到结构化看板（JSON），命令面板会丢弃返回值。

### 面板就是看板（数据怎么进去）

Orca 1.4.198 的插件面板是一个 `srcdoc` 沙箱文档：CSP `default-src 'none'; connect-src 'none'`
（不能 fetch），只能调用三个 host 方法（`workspace.readContext` / `terminal.sendText` /
`notifications.show`）——`PLUGIN_PANEL_ACTIONS`（`src/shared/plugins/plugin-host-api.ts:263`），
再由 `plugin-panel-bridge.ts:42` 的 schema refine 和能力门禁各拦一道。宿主往 frame 里只发
watchdog ping 与 action 结果。**worker→panel 没有通道**，v1 也不打算有。

所以看板只能走唯一那条路：**面板文档本身**。worker 把快照渲染进面板入口文件，而 Orca 每次打开
或刷新面板都从插件根目录重读它（`src/main/plugins/plugin-panel-controller.ts:142-148`）。
两个 Orca 行为把「写文件」变成「面板实时」：

- **dev 安装（主路径）**——dev watcher 监听配置的插件路径，变化后 300ms 防抖触发 refresh
  （`plugin-dev-watcher.ts:106-114`），渲染端发现入口 HTML 变了就重挂载 frame
  （`PluginPanel.tsx:143-147`）。这里写文件是被明确允许的：`verifyHashAddressedPluginContent`
  在 `contentHash === null` 时直接返回 ok——*“Dev trees are intentionally mutable; installed
  hash-addressed trees are not”*（`plugin-content-integrity.ts`）。
- **正式安装（降级）**——目录内容寻址（`<plugins>/<key>/<sha256>/`）、每次加载面板都重新校验，
  所以 worker 绝不写那里。安装进去的文档就是安装那一刻的 `panel.html`；本仓库提交的是
  **placeholder** 版本，实时看板退到通知 + 插件日志。

只有看板**结构**或某个会话状态变化时 worker 才重写文档（见 `panelFingerprint`），绝不按秒写：
时间显示是 `data-ts` 属性，由文档自己的脚本每秒跳动，而重写会重挂载面板。快照超过约 15s 没更新，
年龄数字变灰——数字本身始终精确（`now - data-ts`），变灰只是「扫描循环停了」的信号。在 dev 树里 `panel.html` 被改写是正常的
工作区改动——那个文件**就是**面板的数据通道。

## 4. 数据契约（本插件如何读）

### 轴 A —— tab（一次调用，平铺）

`orca terminal list --json`，**不带** `--worktree`：一次调用就答出所有 worktree 的所有 tab，
插件不再逐个遍历 worktree。每行保留 `handle`、`tabId`、`leafId`、`paneKey`、`title`、
`connected`、`writable`、`lastOutputAt`、`worktreeId`。

- 活体就是该行自己的 `connected`，不看别的字段。
- 缺 `handle` 的行直接丢掉（寻址不了）。
- **实测**：Orca 1.4.198 的行里没有 `paneKey` 字段，插件用 `${tabId}:${leafId}` 现拼
  （新版本可能带上，带上时以它为准）。
- `worktreeId` 是插件保留的唯一 selector：它既是 `orca terminal list --worktree` 需要的那个值，
  也是 `copy-agent-context` 输出的 `orca selector`。

#### tab 轴只留一个 swarm：先看 pi 申报，再看 worktree

一个 Orca worktree 里会混着不属于 onlyne 的 tab，所以 tab 轴按下面的顺序过滤：

1. **适配器申报 —— 权威来源。** pi 适配器从它被 spawn 的那个 pane 继承 `ORCA_PANE_KEY`、
   `ORCA_TAB_ID`、`ORCA_TERMINAL_HANDLE`、`ORCA_WORKTREE_ID`（**2026-09-11 实测，Orca
   1.4.198**：`orca terminal create --command …` 会把这四个都导出给命令进程），所以它是唯一
   从进程内部就知道「哪个 pane 是 onlyne 会话」的组件。它把这个绑定写到
   `<workspace>/.onlyne/cache/pi-pane.json`：`<workspace>` 就是 `onlyne client run` 的运行
   目录（client 以 `cwd` = workspace spawn 插件，且一个 workspace 只跑一个 client），所以这个
   文件必然只属于一个 pane。tab 的 `paneKey` 被申报了才在范围内；若某条申报指向 Orca 已不再
   列出的 pane，结果是其余 tab 全被隐藏——过期申报不会把 tab 复活。
2. **worktree 启发式 —— 兜底。** 任何地方都还没有申报时，tab 在自己 `worktreePath` 里含某个配置
   root 才算在范围内（两侧都做 `realpath`）。root 推不到任何 tab 的 worktree 时，该轴不设范围，
   并在说明里写清楚。

两条路径插件都推不出来：workspace 属于 *client*，而 session 轴是按 *server root* 配的，
`welcome.server` 也只有一个 `{connected, cluster, name}`、不含路径——所以申报只从配置里的
**`piWorkspaces`**（见 §1 第 4 步）读，与 `serverRoots` 相互独立。为空是合法状态：没有申报，
只用 worktree 启发式。

`board.scope` 记录是哪一种决定的：`{ derived, source: "adapter" | "worktree" | "none",
worktrees, hidden, claimed? }`，另有 `summary.hiddenTabs`。申报文件缺失、读不到或格式坏掉都是
正常状态，不是错误——只说明那个 workspace 还没申报过，坏掉的文件贡献 0 条申报而不是一次失败。

### 轴 B —— session（每个 root、每个动词各一次）

按配置顺序，对每个 server root：

```
onlyne --server-root <S> sessions --json   -> {ok:true, data:{sessions:[…]}}
onlyne --server-root <S> roles    --json   -> {ok:true, data:{roles:[…]}}
```

- session 行归一化为 `task_id`、`role`、`session_id`、`public_lifecycle`（退化取
  `projection.lifecycle`）、`projection.agent`/`delivery`/`resource`、`outcome`、`updated_at`、`seq`。
  形状由仓库自己的 wire vector
  `crates/onlyne-proto/tests/wire_vectors/res_session_row.json` 钉住。
- role 行归一化为 `name` → `role`、`admin`、`max_sessions` → `maxSessions`、`state` → `presence`
  （`online`/`offline`/`draining`）、`sessions`。由 `res_role_info.json` 钉住。
- role 列表是分节骨架：一个 session 都没有的 role 也会渲染（离线 role 就是这么看出来的），
  而 role 没在 role 列表里的 session 归到 `(unknown role)`。
- **实测 2026-09-11**：用 `target/debug/onlyne` 打一个 socket 不存在的 root → 退出码 3，
  stderr 是 `onlyne: no onlyne socket found; pass --socket, --server-root, or --workspace`，
  stdout 为空；插件把它报成**该 root 自己**的 `cli_error`。socket 存在但拒连时是 JSON 错误体，
  插件报该 body 自带的错误码。

### join（弱信号，仅展示）

tab 的**标题去空白后正好等于** `onlyne:<task_id>` 时，才标注到那一行 task；对不上任何 session 的
tab 落在「未 join 的 tab」一节。除此之外不推断任何东西。

- **这不是身份**。pane 里的任何进程都能写标题（OSC 0/2），所以这个前缀可能被偷走、丢掉，
  或者指向错的 task。权威身份在 adapter / pi 插件协议里；这块看板只是给 supervisor 行方便的，
  需要确定性的时候要读 session 行，而不是读这个标注。
- **2026-09-11 真机实测（Orca 1.4.198，宿主 worktree 里一个活的 `sleep 600` 会话）**：create 时写的
  标题只活约一秒——操作者自己的登录 shell（zsh + 提示符）立刻把它抢走——而 session 行要等 agent 上报
  之后才出现在 server root 上。两者因此基本不会同时成立，实际看板大多就是把两条轴并排显示、标注为空。
  这是「supervisor 视图且不引入第二个发现轴」的既定代价；`joined` 是附赠，不是某一行存在的理由。
- 认领是**一次性**的，按 `serverRoots` 配置顺序和 Orca 行顺序：两个 tab 同标题时第一个 join，
  其余进 stray；两个 root 有同一个 task id 时，配置里靠前的 root 拿走 tab，靠后的那个行保持
  未 join。这样 `live tabs` 不会把同一个物理 tab 数两次；而它真正属于哪个 root，这里无从得知。

### 降级矩阵（都不算失败，会写进通知/日志）

| 情况 | 结果 |
| --- | --- |
| `serverRoots` 缺失/为空 | 合法：只剩 tab 轴，完全不调用 `onlyne` |
| `orca terminal list` 失败（如 `missing_binary`） | `ok:false` + 错误码；session 轴照常渲染，只是全部未 join |
| 某个 root 的 socket 不存在 | 该 root 报 `cli_error` 与那句 no-socket 提示；其他 root 与 tab 轴照常渲染 |
| onlyne CLI 不认识 `--server-root`/`sessions`（如 0.6.0） | 该动词 `cli_surface_mismatch`；一个动词失败不会盖掉另一个 |
| `onlyne` 二进制找不到 | `missing_binary`，同样按动词降级 |
| 答里没有该动词的行 | `unexpected_shape` |
| tab 标题被抢走或本来就不是这个约定 | 不标注：task 行显示 `无 tab`，tab 出现在 stray 一节 |

## 5. 已知边界

- **人工安装**：pluginApi v1 没有 CLI 安装面（`plugins:install` 只有桌面 IPC / serve RPC），
  装/启/授权都得在桌面点。
- **实验 API**：`pluginApi` 尚未冻结；Orca 升级后先跑一次「重新扫描」确认 join 还成立。
- **worker 会被回收**：5s 兜底重扫只在 worker 活着时跑；下一个事件或命令会把 worker 重新拉起。
  因此「事件驱动 + 兜底」不是硬实时保证。
- **不碰生命周期**：spawn/close/rename 属于 onlyne backend（防双主），本插件一律不碰。
- **写文件的范围**：dev 安装下只重写一个文件——自己的 `panel.html`（面板的数据通道，见 §3）；
  正式安装（内容寻址）下一个字节都不写。它绝不碰 onlyne 的文件、cache、socket 侧状态；
  另一个读的文件只是它自己的可选配置，`debug-board` 也只写你指定的路径。
- **单次扫描成本**：一次 `orca` 调用，加每个配置 root 两次 `onlyne` 调用。各 root 互不依赖，
  一个 root 掉线只花它自己的两次失败，不会拖累整块看板。

## 6. 卸载 / 升级

- **禁用**即可：worker 停 → 不扫、不通知、不订阅事件（面板还在，但读不到数据即报错）。
- **卸载**：Settings → Plugins 里卸载，Orca 会删掉 `<userData>/plugins/onlyne.onlyne-sessions`
  （hash 目录 + `current` 指针）。本插件没有在别处留状态（无 storage、无 secrets、无设置写入）。
- **升级**：改源目录 → 重新 Install（新 hash 目录）→ 能力指纹变了才需要重新授权。

## 7. 开发与验证

```
integrations/orca-plugin/
  orca-plugin.json     manifest（pluginApi 1）
  main.mjs             worker 入口：activate/deactivate + 五个命令 + 三个事件
  panel.html           面板文档：提交的是 placeholder（dev 安装会替换它）
  src/
    runner.mjs         可注入 exec、CLI JSON 解析（含 stdout 里的错误体）、配置
    orca-cli.mjs       terminal list（平铺）/ terminal switch / status
    onlyne-cli.mjs     --server-root <S> sessions|roles，行归一化 + 失败归一化
    board.mjs          两条轴 + 弱标题 join + 看板模型
    claims.mjs         pi 适配器的 pane 申报（swarm 归属的权威来源）
    render.mjs         文本渲染（通知/日志共用）
    panel-document.mjs 面板文档：快照渲染、指纹、仅 dev 生效的写回
    commands.mjs       refresh / board / debug-board / focus / copy-agent-context
    board-state.mjs    防抖、5s 兜底、结构指纹、通知冷却
    *.test.mjs         node:test（零依赖）
  tools/smoke.mjs      真机只读冒烟（带突变守卫）
```

```bash
node --test                     # 本包自己的测试命令
BIN_DIR=target/debug node tools/smoke.mjs
                                # 真机只读：orca status / 平铺 tab 列表 / 按 root 的看板 /
                                # join demo（合成 session）/ 面板文档（生成到临时目录，不碰插件树）
```

冒烟脚本的 runner 会拦截任何 `terminal switch|create|close|rename|send` 与
`worktree create|rm`，一旦出现就退出码 1 —— 也就是说「跑一次冒烟」本身证明不了会动你的 tab。

## 8. 给 onlyne backend 的契约核对

按实测到的面实现，以下是实现时才暴露出来、需要 backend 确认的点（不改契约，只报告）：

1. **标题约定不是契约**。插件按 `onlyne:<task_id>` join，因为 tab 侧只有这一个线索；
   但 OSC 写标题随时能把它换掉。如果 backend 开始依赖这个前缀，就必须另有一条途径公布
   pane/handle 绑定——权威在 adapter / pi 插件协议，这块看板只是镜像。
2. **插件不读 backend 的任何文件**。session 只来自 `sessions`，role 只来自 `roles`，仅此而已。
   supervisor 需要的东西必须能通过这两个动词答出来。
3. **`sessions` 必须在没有 role 工作区时也能答**。既然不再有 per-role worktree 注册，
   某个 role 的 tab 在 Orca 侧与其他 tab 无从区分，`task_id` ↔ tab 的绑定改由 pi 适配器恢复
   （`pi-pane.json`，见 §2 轴 A）：适配器继承自己所处 pane 的 `ORCA_PANE_KEY`，所以线上量到的
   `pane_key` 就是 backend 为被 spawn 会话记录的那个值。
4. **用到的 session 字段**：`task_id`（身份）、`session_id`（展示）、`role`（分节）、
   `public_lifecycle`/`projection.lifecycle`、`projection.agent`、`projection.outcome`、
   `updated_at`、`seq`。其余字段忽略。
5. **用到的 role 字段**：`name`、`admin`、`max_sessions`、`state`、`sessions`。presence 词表就是
   server 自己的（`online`/`offline`/`draining`），原样渲染。
6. **按 root 失败是常态**。某个 root 没有活 server 对看板而言是正常状态；插件按动词报错并继续，
   而且**不合并 root**：两个 root 上相同的 task id 就是两行。
7. **插件从不传 `--quiet` 或 `--socket`**。它读整个回答体（`{ok, data:{…}}`），所以这个 envelope
   形状的变化对插件是破坏性变更。
