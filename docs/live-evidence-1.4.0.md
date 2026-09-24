# 1.4.0 live acceptance evidence

One live deployment's acceptance record, kept here because the CHANGELOG quotes its readings. The
cluster is the Alexandria workspace, driven by a peer operator agent that runs the product's real
processes on real panes and records what it reads. It is the source of every live reading in the
1.4.0 entries above, and it covers the swap acceptance rounds of 2026-09-23 and 2026-09-24: the
refusal gate, the family wire, the admin control addressing, the re-dispatch generation, the
control-settle fallback, the retirement publish, the ghost sweep, and the three defects those
rounds found — the sweep refusing a completion receipt, a replayed verdict holding a slot forever,
and a queued delivery whose recipient has no client never reaching a terminal state.

The record is its author's, verbatim, with this provenance paragraph added by `onlyne-dev`; the
author's own copy is the working record it keeps extending. This repository's own gates live in
`docs/STATUS.md` and the release scripts in `packaging/README.md`.

---

# round4 — 1.4.0 换机验收现场（2026-09-23 23:20 起 CST）

集群：真身 `/Users/dbydd/Library/CloudStorage/OneDrive-个人/new_document/Alexandria`
新二进制：onlyne / onlyne-server / onlyne-client / onlyne-gateway / onlyne-tui 均 1.4.0（23:19）

## 换机动作与结果

| 步 | 动作 | 结果 |
|---|---|---|
| 0 | 备份 | `.onlyne/state.db{,-wal,-shm}` → `/tmp/onlyne-bug-141/swap-backup/` |
| 1 | 停六 client | `orca terminal send --interrupt` ×6，无残留进程 |
| 2 | 停旧 server | `onlyne server stop --root .` → stopped pid 71622 |
| 3 | 起新 server | **首次失败**：`onlyne: unsupported schema; v1.0.0 does not migrate`（活库是 marker 3，dev 的「marker 仍是 4、不用挪库」是相对上一批而言）→ 旧库三件套挪 `swap-backup/schema3/` → 重启成功 `started pid 46640`，version 1.4.0，空库 `ghost_sweeps: []` |
| 4 | 六 client 重开 | pids 47082 scriber / 47126 librarian / 47154 astrologer / 47197 master / 47240 socrates / 47267 scraper，`connected_roles 6` |
| 5 | skill export | `onlyne skill export --set role --set supervisor --force` → 写 role / supervisor / role-payload-v2 三份（git diff: role +49、supervisor +51 行） |
| 6 | canon 同步 | `.onlyne/AGENTS.md` 工具面：续家族 `onlyne_handoff {to, text}`、开新家族 `onlyne_send`；bash 七动词门禁条保留 |

## 门禁负例（新二进制实测）

缺 flag 一律 exit 2，且在解析 socket 之前拒：

```
onlyne: send requires --force and --yes-i-am-supervisor-not-other-role: a role inside a session sends
with its plugin's own tool, onlyne_send (to, text, kind, image), where kind="task" starts a new task
family at hop 0, kind="note" leaves free text, and onlyne_handoff continues the family this session
was handed; this verb is a supervisor maintenance command for an operator or a supervisor driving a
role from outside
```

`handoff` 文案点名 `onlyne_handoff (task_id, to, text, image)` 并说明携带 hop budget / origin /
deadline / labels；`control` 文案说明插件代答。只读动词（`ledger` 等）不受影响。

## 验收单

### 单 1：跑通路径（绿）
task `fd54f1ba` → scriber 写 `tools/scratch/swap-acceptance-2325.md`（12 行）→ pi 调
`onlyne_complete{outcome: done}` → task `acked`、completion head「验收单已落 …（12 行…）」、
投影行 `exited`、client `retire: retiring idle session resource … reason=Completed`（15:25:40Z）。

### 单 2：handoff + kill client（暴露一条新缺陷）
- task `18bab282` → scriber 写 `tools/scratch/handoff-acceptance-2326.md` 6 行 → 用
  **`onlyne_handoff`**（插件工具，head 原文「已 onlyne_handoff 给 li…」）交 librarian。
- 23:27:47 `SIGTERM` librarian client（pid 47126，cmdline 核对过）。
- 23:27:57 起 task 行回到 `queued`（客户端死在中途 → requeue），session 行留 `working/running`；
  23:29:17（+90s）`heartbeat_stale=true`；**`ghost_sweeps` 全程 0 条** —— 符合 sweep 的设计条件：
  `onlyne-server/src/ghosts.rs` 的 `sweep_row` 先读任务 ledger 行，非终态（queued/in_flight）
  直接返回 `None`，因此「账还开着的行不收，只记 stale_working」。
- 23:31/23:32 重开 librarian client → 同一 task 被重投，新会话干完了活（文件末尾追加
  「librarian appended this line to close the handoff test」），**但账最终落
  `rejected: session_dead`（23:33:29）**。
- 原因（librarian client 日志原文）：
  ```
  15:32:24.970 WARN onlyne_store::client: session ledger alert alert=session fault settle_without_turn on 3f2812db
  15:32:24.970 WARN onlyne_client::session::dispatch::settle: a completion arrived for a session that
               never ran a turn; the task stays open task=3f2812db-…
  15:33:28.992 INFO onlyne_client::session::dispatch::retire: retiring idle session resource task=3f2812db-…
  15:33:29.183 INFO onlyne_client::runtime::runloop::sessions: session retired past its window
               session=3f2812db-… arm="reconnect_g…
  15:33:29.297 WARN onlyne_session::reconcile::bridge: lifecycle event rejected; ledger left as-is
               task=3f2812db-… reason=Undefine…
  15:33:29.297 WARN onlyne_session::reconcile::feed: resource attach refused for a dispatched session
               task=3f2812db-… verdict=Reje…
  15:33:50.687 WARN onlyne_client::session::dispatch::settle: a completion arrived for a session that
               never ran a turn; the task stays open task=3f2812db-…
  ```
  pi 侧的 completion 是 `{outcome: done, head: "Handoff acceptance already satisfied: …"}`（15:33:50），
  被 settle 门拒两次；工作成果在磁盘上，账上却是 `session_dead` 拒绝。

### 单 3：control 关闭（未单独走通，两次都被快速任务抢先完成）
- 单 `2fc2f01d`：**cancel 调用形态错**——`control` 在 admin 面还要 `--from _supervisor`，缺它 exit 2
  （输出在 stderr，首轮脚本只收 stdout，看起来像"空输出"）。任务在 cancel 前已正常完成（acked、
  `tools/scratch/cancel-acceptance.md` 162 B）。
- 单 `5965f874`：完整形态 `onlyne control cancel --task … --reason … --from _supervisor --force
  --yes-i-am-supervisor-not-other-role` → exit 0，control 行入账（15:46:01）；但该单只有 6 行活，
  23:46:35 正常完成（task acked + completion 行 + 行翻 `exited`，走 completion 臂）。
  **control 驱动的活体关闭未单独隔离**（需分钟级任务才压得住时序）。
- 可佐证的旁例：孤儿单 `8c2e5024`（kill 测试遗留、queued、无活会话）被同形态 cancel 处理，
  client 重连后 6 秒内 `retire: retiring idle session resource task=8c2e5024`（15:46:13）。

### 单 4：admin 面 control 的寻址缺陷（决定性）
`control cancel|probe` 不带 `--to` 时，账上落到 `from _supervisor → to _supervisor`、`state queued`、
`acked_at` 空，client 侧零日志——六次（5×cancel + 1×probe）全一样。而 `_supervisor` 按设计不起
client 进程，于是没人取件、op 静默失效。给 `--to scriber` 后同一条 cancel **0.4 秒**被应用：

```
16:03:46.151739Z INFO onlyne_client::runtime::runloop::run: control command applied op="cancel"
                 task=e230f443-0084-4a71-8f48-454df73891d6 held=true
```

control 行 3 秒内 `acked`。源码指针：`crates/onlyne-cli/src/verbs.rs::control_inner`（845-883）在
Admin 面构造 `AdminOp::Control(AdminControl { from, op, to: args.to })`，`to` 直接取可选 `--to`。
dev 已认领，按「缺 `--to` 时按 task 反查属主 role、无属主则明确拒收」修。

### 单 5：cancel 应用后任务与镜像行都不落定（新发现）
同一条已应用的 cancel（落在会话开跑约 8 秒时）之后观察 3.5 分钟：task 仍 `in_flight`、镜像行仍
`working`、心跳 stale、server 开 `heartbeat_missing` fault、**无 retire 行**，且该任务的会话
jsonl **从未生成**。最终靠 `onlyne repair fail --task … --reason …` 收尾（task rejected、行翻
`exited`、fault 关闭）。已问 dev：cancel 是「只停会话」还是「自己结账」——后者缺一步 settle。

### sweep 活体样本：两条路都造不出
- kill client：任务行退回 `queued`（非终态），`sweep_row` 第一条件不满足，设计如此。
- `repair fail`：**自己就把镜像行翻了**（00:08:10 执行 → 00:08:15 读到行 `exited`），
  `ghost_sweeps` 始终 0 条。新 client + repair 面下结账者总顺手搬行，sweep 窗口从外部够不到。
- 结论：按「`onlyne-server/src/ghosts.rs::sweep_row` 源码 + store 侧 marker-4 分支单测」记为已验，
  不再花集群时间。

### 临时配置实验（已还原）
`ghost_sweep_secs = 2` 写入 → `onlyne reload` 报 `change server: ghost_sweep_secs`（热生效路径通）
→ 用完删行 + reload → `onlyne spec_diff` 回报 `spec: no changes`，配置干净；
`heartbeat_grace_secs` / `heartbeat_timeout_ms` 按 dev 提醒未动。

## 结论状态
- 投影行翻面实测路径：completion 臂（单 1/单 3/单 4 路由后）、重连臂 `arm="reconnect_grace"`（单 2）、
  admin cancel 臂（单 4/单 5，行 final `exited`）均翻 `exited`。
- 另两条待 dev 修：重连后 completion 被 settle 门拒（单 2）；cancel 不落定（单 5）。
- 文档缺口：`.agents/skills/onlyne-supervisor/SKILL.md` 的 `control` 示例缺 `--from _supervisor`
  （已由 dev 修，随下次装 onlyne 生效，需再 `--force` 重导）。

## 修复后复验（2026-09-24 00:33–00:37，新 onlyne + onlyne-client；六 client 已重启换映像）

### 复验一：kill 形状（客户端重投世代修复 `8cfdf40`）——绿
单 `7fb1a5c5`（scriber 写 5 行 → `onlyne_handoff` 给 librarian → librarian 追加一行后 complete）：
scriber 交接落 `753f9568`（in_flight）→ `SIGTERM librarian client`(94526) → **5 秒内 task 回 `queued`**
→ 重开 client → 重投并完成：`completion acked` + `task acked` + 投影行 `exited`，
ledger head「收单完成：tools/scratch/kill-verify.md 由五行加到六行…」。
librarian client 日志出现修复本身的痕迹：

```
16:36:12.414170Z INFO onlyne_client::session::dispatch::delivery: the row a re-dispatched session is
                 born onto was rebased onto a new generation task=753f9568-1bcf-4600-bae2-a386cdb89649
```

**`settle_without_turn` 零条**（tab 里仅剩 15:32 那次旧事故的两行），无 `rejected: session_dead`。

### 复验二：control 不带 `--to`（寻址修复 `7174a93`）——绿
- 负例：任务无属主会话 → `exit 4`，stderr 逐字
  `onlyne: no session owns task 00000000-0000-4000-8000-000000000000; pass --to <role> to say where the control goes`。
- 正例：`control probe`(16:33:27) 与 `control cancel`(16:33:32) 均不带 `--to`，两条 control 行都在
  数秒内 `acked`，client 日志两条 `control command applied op="probe"|"cancel" … held=true`。
- 该单的收尾按 interim 规则走 `repair close`（cancel 兜底未装），落 task rejected + 行 `exited`。

### 复验三：cancel 未作答兜底（`a3d20e6`）——半绿（日志与 30 秒判据对，落账缺一步）
单 `becbd03c`（投出后 2 秒、该任务的 jsonl 尚未生成时 cancel，不带 `--to`）：

```
01:06:19.52 INFO …run: control command applied op="cancel" task=becbd03c-… held=true
01:06:49.49 INFO …sessions: a task was settled on an operator's word no plugin answered
             task=becbd03c-… outcome=Cancelled waited_secs=30
01:06:49.77 INFO …delivery: the row a re-dispatched session is born onto was rebased onto a new
             generation task=becbd03c-… generation=2
01:07:57.62 WARN …settle: a second verdict arrived for a settled task; the first one stands outcome=Done
01:09:05.82 INFO …sessions: session retired past its window arm="reconnect_grace" quiet_secs=74
01:09:05.94 INFO …delivery: … rebased onto a new generation … generation=3
01:09:21.32 WARN …settle: a second verdict arrived for a settled task; the first one stands outcome=Done
01:09:47     （server）ghost sweep：seq_before 1007 → seq_after 1008，evidence=task_settled:rejected，outcome=failed
01:10:23.65 INFO …run: control command applied op="cancel" task=becbd03c-… held=true（at-least-once 重投）
01:10:53.68 WARN …slots: an unanswered control command's task was already settled; the first verdict stands
```

**绿的部分**：寻址（control 行 `from _supervisor → to scriber`、秒级 acked）；兜底日志与其 30 秒判据（`CONTROL_SETTLE_BOUND`）；重复 control 投递惰性（`held=false` 仍结行）。
**不绿的部分**：兜底只写了 **session 镜像**的 outcome（`cancelled`），**任务账行**没被它落定（一直 `in_flight`，直到 01:09:4x 由 `session_dead` 收口）。账不终态 → 投递行有资格被反复重投 → 每约 2 分钟起一代新会话重跑整个任务（`tools/scratch/fallback-verify.md` 被写了 60 行、jsonl 长到 380KB），完成的 Done 又被 settle 门拒掉。**修点**：兜底宣布 settled 的同一拍就该按 `Cancelled` 落任务账（reason 用 `operator cancel`，而不是被后来的 `session_dead` 覆盖）。

### sweep 活体样本：**跑通了**（我先前判「外部造不出」是错的）
`onlyne ghosts` 实录：

```
{id:1, task_id/session_id:becbd03c-…, role:scriber, generation:3,
 seq_before:1007, seq_after:1008, outcome:failed, evidence:"task_settled:rejected", swept_at:1790183387}
```

形状正是「任务账终态 + 镜像仍 working + 属主在线」；outcome 按账本行推（rejected → failed）与设计一致。**止住上面那台循环的也是它**（01:09:47），我随后 01:10:23 的 `repair close` 只是重复了一次已完成的动作。样本文件已清（`tools/scratch/` 只剩 qwen 族遗留件）。

### 收尾状态
13 行 sessions 全部 `exited`；open faults 0；六 client = `a3d20e6` 映像；supervisor 手册已重导（含 `operator cancel` / `operator recycle` 段）。三条修复的复验结论：重投世代绿、control 寻址绿、cancel 兜底半绿（等 dev 补落账）。

## 复验五：关闭即答投递行（`200c88d`，server+client 同批）——**五条全绿**
现场：server 重启为 pid 56145、六 client 02:11 映像、`ghosts` 基线 1 条（becbd03c 旧账）。
单 `3f0cb6bb`：投出后 2 秒（该任务 jsonl 尚未生成）`control cancel`，不带 `--to`。

```
02:13:56.557 INFO …run: control command applied op="cancel" task=3f0cb6bb-… held=true
02:14:01     账：task rejected / reason=operator cancel        ← +5 秒，不再 session_dead
02:14:26     行：exited / agent=gone / outcome=cancelled       ← 未被 sweep 改写
             generation 始终 1；tab 内 3f0cb6bb 名下零条 rebased onto a new generation
             onlyne ghosts 仍 1 条（becbd03c / task_settled:rejected / failed），本任务无新行
             tools/scratch/v5-cancel.md 未生成（会话死在干活前，无第二遍重跑）
             全程未使用 repair close
```

对照上一条循环（becbd03c：兜底 30 秒 + 两代重投 + sweep 01:09:47 才收口），这条形状在 client 侧 +5 秒收口，快一个量级，重投路径从源头消失。

## 1.4.0 验收清单（我方全部过）
七动词门禁正负例 · 只读面不受影响 · `onlyne_handoff` 续家族在真实链上跑通 · 重投世代（复验一）· control 寻址（复验二）· cancel 兜底与关闭即答（复验三/五）· ghost sweep 活体一次（含它作为唯一兜底的那次）· skill export 一致性与手册新段落 · `ghost_sweep_secs` 热生效 · `spec_diff`/marker 4 不挪库 · `repair` 族与两 flag 边界。
待人定或未结：「从未跑过的任务被判 done 是否再加闸」·「页2 `reason` 列」（若已随批落地，TUI 核一眼再结）。

## 复验六：recycle 形状（`7b0f21de`）——三条中、两条偏差（dev 认账并补修复）

```
02:19:18     control recycle 发出（不带 --to）
02:19:19.088 INFO …run: control command applied op="recycle" task=7b0f21de-… held=true
02:19:23     投递行 rejected + reason=operator recycle        ← 不是 session_dead ✓
02:19:38     镜像 exited / outcome=failed / generation 仍 1 ✓ ｜ 产物 v6-recycle.md 未生成 ✓
02:19:38     ✗ onlyne ghosts 1→2：新行 {7b0f21de, task_settled:rejected, failed}
02:19:48.881 ✗ 兜底仍开火：a task was settled on an operator's word no plugin answered
                task=7b0f21de-… outcome=Failed waited_secs=30
```

**我的读法（对一半）**：recycle 的关闭路径答了投递行却**没顺手发布镜像**，于是「账已终态 + 镜像仍 working」活了约 15 秒，被 sweep 的 60 秒周期捞走。对照 cancel（v5）：那 30 秒里 sweep 没轮到，所以无新行——差别只是抢没抢到，与两个动词的语义无关。
**dev 纠正的部分**：sweep **不结算任务**，只搬镜像；任务判定只有 30 秒兜底那一个门写。所以不存在"三方抢同一次收口"，兜底的开火是对的，只是**晚**——根因是 close 少了一次发布。
**修复**（client 一处、`Recycle`/`Cancel` 两臂各一次）：关闭当场发布本机已写好的投影（lifecycle `exited`，**不带结局**——结局留给先落地的判定，可能是插件自己那份 `done`）。补完后预期：投递行 +5 秒 `operator recycle`、镜像 **+5 秒 exited（outcome 仍空）**、+30 秒兜底补 `failed`、`ghosts` 对该任务无新行、无第二遍。
**页2 `reason` 列的核法**（dev 给）：别啃渲染帧，直接 `onlyne-tui --server-root <root> --once --page 2` 落一帧纯文本再找 `reason=`。orca 对该 tab 的 `--screen` 不给屏幕，`tail` 里是 diff 后的重绘，grep 会漏。
**可观测性挂账**（双方一致，交人拍板）：done 判定不再加第二道闸；要往前只加证据（例如「该任务无心跳记录」写成 faults kind 或只读列）。

## 复验六b：recycle + close 补发布（`58807ea`）——1/2/4 中，3 半中
六 client 换 02:41 映像，server 保持 56145。单 `4fcbcd0a`（行 working +2 秒、jsonl 尚未生成时 recycle，不带 `--to`）：

```
18:44:13.641 INFO control command applied op="recycle" task=4fcbcd0a-… held=true
18:44:13.844 INFO control command applied op="recycle" task=4fcbcd0a-… held=true   ← 203ms 内重复应用
+2.1s        账：task rejected / reason=operator recycle                    ✓ 判据1
             镜像：exited / agent=gone / outcome=（空）/ gen=1              ✓ 判据2（上一轮 +20s 仍 working）
18:44:43.443 INFO a task was settled on an operator's word no plugin answered
                        task=4fcbcd0a-… outcome=Failed waited_secs=30
收尾         镜像 outcome 仍为空（未变 failed）                            ✗ 判据3 后半
             产物 v6b-recycle.md 未生成、gen 始终 1、无第二遍               ✓ 判据3 前半
             onlyne ghosts 仍 2 条旧账、本任务无新行                        ✓ 判据4
```

**读法**：close 在 +2.1 秒已把投递行答掉、任务即刻终态，+30 秒兜底那次写在 `settled_at IS NULL` 守卫下惰性——连镜像 outcome 也没补。
**交 dev 定的两点**：① 兜底日志现在会在什么都没结算时开火（措辞误导，应终态即整条不发）；② 镜像结局一致性——v5 的 cancel 最终 `cancelled`，本轮 recycle 最终空；要么 close 带结局（与「怕与插件自己那份 done 冲突」相冲），要么让兜底负责补镜像结局。

### 三角定位（dev 要的两条 + 一条附加，未重跑）
- **(a)** `onlyne sessions --task 4fcbcd0a`：`generation 1`、`row.seq 1004`、`observed.version {gen 1, seq 1004}`、`agent gone`、**outcome 空**、`lifecycle exited` → close 的发布把行推到 1004，**兜底之后 seq 未再前进**。
- **(b)** client 日志（scriber tab，缓冲 18:42:40→现在，66 行）：**无** `a settled task's exit was not published`，也无任何 publish/版本被拒的 WARN/ERROR；兜底只留下它自己那行 `outcome=Failed waited_secs=30`。
- **(c)** `server.log`（14 行）：最后一条是 `18:19:36 ghost sessions swept count=1`（上一轮的 7b0f21de）；**18:44 之后零条**——没 swept、没 stale、没写入被拒。
- ~~推断：`sync_session` 只发会话字段、投影与已存无差~~ **已被 dev 证伪**：`projection.rs:164-193` 在同一把锁下同时读会话行与 `stored_task_state`，`projection_of` 确实带上 `task_outcome_of` —— 发布是带结局的。
  **真正的拦截点是服务端版本闸** `onlyne-server/src/projection.rs:236-242`：`(generation, seq) <= watermark → skipped()`，静默、不写、不记日志。会话 tuple 在关闭那刻已终态，而任务判定住在 `task` 表（本仓刻意把任务结果移出会话 tuple），判定落地不动行版本 → +30 秒那次发布带的是 `(1,1004)`，与镜像水位相同 → 判重复丢弃。三个角全吻合：发送成功（无 `exit was not published`）、跳过静默（server.log 零条）、seq 不前进（停在 1004）。v5 的 `cancelled` 能落上只因那一版 close 不发布，兜底那次是该行第一次带 exited 出门。
  **修法（服务端一处）**：`write` 开一个窄口子——版本不更新、**只差 `outcome` 一个字段**、且已存行**没有** outcome 的发布，只把该 outcome 写进去（沿用已存 gen/seq、水位不动、不许其它维度混入）；此后更旧的重复发布一律跳过。客户端无需改动。
  **复压判据精确化**：`ghosts` 对该任务无新行；镜像 **seq 停在关闭那一刻的值不前进**；**outcome 在 +30 秒补齐**（recycle→`failed`、cancel→`cancelled`）；外加第二次 applied 归零、镜像 +2 秒 `exited`。
- **我对 dev 的一处让步**：他第 1 条反驳成立——`rejected: operator recycle` 是**投递行**的 state，任务判定住在 `task` 表（投影里是 `outcome`），两个对象各写各的；那条日志按他给的代码路径（`sessions.rs:277-299` 只在 `settle_unanswered_control` 返回 true 时才打，而 true 的唯一来源是 `store.settle_task` 的 `Ok(true)`）就是「这次真写了任务判定」的证据，措辞不动。

## 复压七：ack 先于发布 `42fc685` + 服务端 outcome 窄口子 `b31b5b3`——**两条形状五条读数全中**
换机：`onlyne-server`/`onlyne-client` 03:25 新装；备份 `round7-pre/state.db*`；六 client 与 server 全重启（marker 仍 4、库未挪）→ server 新 pid、`connected_roles 6/7`。两条都用缺省寻址（不带 `--to`），投给 scriber 后 2 秒下手。

**recycle · `c2eac369`**
```
19:30:41.738 control command applied op="recycle" held=true        ← 全缓冲仅此一条，第二次 applied = 0 ✓判据1
+1.6s        镜像 exited / agent gone / outcome None / gen 1 / seq 1003→1004 ✓判据2
+1.6s        投递行 rejected reason=operator recycle；control 行 acked
19:31:11.536 a task was settled on an operator's word no plugin answered outcome=Failed waited_secs=30
+29.8s→+57s  镜像 outcome=failed 已补齐；seq 仍 1004（未前进）✓判据3+4
             generation 始终 1；ghosts 无本任务新行；v7-recycle.md 未生成 ✓判据5
```
**cancel · `18fb2ea6`**
```
19:32:25.755 control cancel 发出 → 19:32:26.113 applied（一条，第二次 = 0）✓判据1
+3.5s        镜像 exited / agent gone / outcome None / gen 1 / seq 1004 ✓判据2；行 rejected reason=operator cancel
19:32:55.923 no plugin answered outcome=Cancelled waited_secs=30
+52.7s       镜像 outcome=cancelled 已补齐；seq 仍 1004 ✓判据3+4
             gen 1；ghosts 无新行；v7-cancel.md 未生成 ✓判据5
```
**水位这条是特意要的证**：关闭那次发布把 seq 从 1003 推到 1004，兜底的判定写入**沿用 1004 不动**，`ghosts` 两条形状都无新行——服务端那个「只差 outcome 且已存行无 outcome」的窄口子按设计生效。
**顺带纠我上轮的一处推断错**：`sync_session` 确实带 outcome（`projection.rs:164-193` 同锁读 `stored_task_state`），拦它的是版本闸；我的「映射缺失」说被证伪，修法在服务端。
**文档同步**：`.supervisor/AGENTS.md` 终结条改写——`--to` 可省（老毛病已修）、收口时序（2 秒答行 + exited 无结局 + 30 秒补结局且水位不动 + ghosts 无新行）、`repair close` 只留给「目标会话不存在」那一类。
**收尾读数**：18 条 sessions 全 exited、open faults 0、`ghosts` 仍 2 条旧账（7b0f21de / becbd03c）、`spec_diff` no changes、验收产物零残留（`tools/scratch` 只剩 qwen 族件）。

## 复压七·附加：done-then-recycle（`0294930b`）——dev 五条判据全中
配方：任务 8 秒干活 + `complete` 不 handoff；脚本轮询到**镜像 outcome=done 落地那一拍**立刻 `control recycle`（间隔 0.0s）。

```
+22.8s       镜像 outcome=done、任务账 acked（msg de681398）           ✓判据1
19:37:15.911 WARN redelivery of a finished task settled without running it   ← 重投守卫先响
19:37:16.113 control command applied op="recycle" held=true           ✓判据2（第二次 applied = 0）
19:37:16.221 retiring idle session resource backend=orca             ｜control 行 acked、镜像 exited
             投递行**不**出现 rejected: operator recycle（已被插件自己 ack）← 与 dev 措辞差一处，语义正确
19:37:46.277 WARN slots: an unanswered control command's task was already settled; the first verdict stands
             该任务名下 `no plugin answered` **0 条**                    ✓判据3（note 被 take_controlled_settle 消费）
+26.8/60.8/82.8s  outcome 始终 done，未变 failed                        ✓判据4
             gen 始终 1、ghosts 无新行、无第二遍                        ✓判据5
```
**水位与 dev 预期不同（硬数）**：done 那一拍 seq 已推到 **1010**，recycle 之后**仍是 1010**——镜像在 recycle 前已 `exited`，关闭那步只 retire 资源、投影无新字节，故水位不动。与「判定不推水位」同一类惰性，我判无需改。

### 观察 A：正常完成路径的 `observed.agent` 永远停在 `running`（全库 19 条计数）
```
(exited, running, done)      ×10   ← 插件自己报完成的会话
(exited, running, failed)    ×2
(exited, running, cancelled) ×2
(exited, gone,   cancelled)  ×2   ← 我的 cancel 探针（操作者关闭路径）
(exited, gone,   failed)     ×2   ← 我的 recycle 探针
(exited, gone,   None)       ×1   ← 上一版遗留 4fcbcd0a
```
`agent=gone` **只从操作者关闭那条发布路径产生**；插件报 done/失败的会话在镜像上读作「已退出、agent 还在跑」。活样本 0294930b：`19:37:16.221` 资源已 retire，字段仍 running。已交 dev 定：要不要让 retire/completion 那次发布把 agent 一起带走。
### 观察 B：`completion` 账行 10 条里 9 条停在 `queued`
唯一 `acked` 的是 `753f9568`（librarian，也是唯一 generation 走到 2 的那条）。假设：`complete` 且无 handoff 时报告没有后继收件人，ack 路径天然不发生；有接力的一跳被下游 ack。副作用是这类行会成为重投来源（本轮 redelivery WARN 就在 done 后 0.2 秒）。交 dev 判：收口时就地结掉无后继的 completion 行，或维持现状并在文档写明「报告类行不被 ack 是正常态」。
**探针清场**：`v7-done.md` 已移出工作区（`/tmp/onlyne-bug-141/round7/v7-done.md.probe`），scriber ws 内 v7 残留为零。

## 复压八：观察 A 的修复（`3704b00`，04:17 client）——**未通过，根因改判**
六 client 10:20（02:20Z）已换 04:17 映像；server 为 03:25 二进制（dev 以为仍是 56145，实际我 03:29 重启过，同一批代码）。

**探针 `cf48fed4`（普通完成、不 handoff）**
```
02:23:32.818 INFO retire: retiring idle session resource task=cf48fed4 … reason=Completed   ← 新分支命中，feed 跑过
             （无 “agent-gone projection failed” 告警）
本机 client.db sessions 行: seq=1019  agent_state=gone   resource_state=closed    generation_live=false
服务端镜像 同一任务:        seq=1018  agent=running      resource=attached        generation_live=true
                                                  outcome=done  lifecycle=exited
```
→ **本机事实写了、版本号推了，那份 1019 从未上线。**

**普查（scriber ws 本机行 vs 服务端镜像，10/10 一致）**：每条 done 会话 **本机 seq = 镜像 server_seq + 1**，本机一律 `(gone, closed)`、镜像一律 `(running, attached)`：
`fd54f1ba 1014|1013`、`18bab282 1020|1019`、`2fc2f01d 1015|1014`、`8c2e5024 1016|1015`、`5965f874 1016|1015`、`3bd0e71c 1014|1013`、`ce4f59aa 1017|1016`、`7fb1a5c5 1024|1023`、`0294930b 1011|1010`、`cf48fed4 1019|1018`（末两条之别只在映像新旧）。

**改判**：前 9 条是**旧 build 退休的**，本机行同样读 `gone` ⇒ `feed_agent_gone` 在完成路径上本就有人喂（另一观测点），缺的是**退休之后那一次发布**。`3704b00` 补的是一次不改变对外读数的本机写；它的两条测试断言也在本机 stored row 上 ⇒ 测试全绿而镜像不动。**修法建议**：`retire_idle_locked` 在最后一次 feed 之后、`untrack_live` 之前显式发布一次（与操作者关闭 `c589876` 同一手法——那条能上线正因发布在摘 slot 之前），并补一条**断服务端镜像**的测试。

### 观察 B 定稿（替换我先前那两版推断）
dev 的两版假设：① 「地址形状/收件人记着退休 session」——**被代码否证**（`relay.rs:515-521` 的 `queued_for(role,8)` 与 `in_flight_for(role)` 从不读 `to.session`）；② 「收件人 role 当时无活 client」——**被我的账本独立证实且更宽**：当前 16 条 queued 行 `to` **全是 `_supervisor`**（10 completion + **6 control 回执**），唯一 `acked` 的 completion `to=scriber`（在线）。
**本拓扑事实**：`_supervisor` 无 ws、`session_command = []`、`allowed_senders = []`，supervisor 是人机界面而非插件 client ⇒ **寄给 `_supervisor` 的 origin 回执永不被消费**，16 行是产物、不是故障。dev 提的「起该 role 的 client 看它取走」在此仓不可做（要 `generate` 一个设计外的 supervisor ws）。若要把「收件 role 无 client 时就地结掉」变成行为，那是设计题，交人拍板。

## 复压九：退休之后补发布 `51c23d9`（12:16 client）——**两条形状全中，观察 A 结案**
六 client 换 12:16 映像（server 未动，03:25 二进制）。tab↔role 实测登记：scriber `b54f9994`、librarian `1efa68d1`、astrologer `09098506`、master `f41cf013`、socrates `93262969`、scraper `ab0c128d`（off-by-one 那笔账清掉）。读数取法固定为**两侧同取**：服务端 `observed.version.seq` 对 scriber ws `client.db` 的 `sessions.seq`。

**形状一 · 普通完成 `bac4c003`**
```
12:22:21 done 落地   镜像 exited / running / attached / ver.seq 1008 ｜ 本机 1008 ｜ 差 0
12:22:25 retire      retiring idle session resource（同行 reason=Completed）
+8s                  镜像 exited / **gone** / **closed** / done / ver.seq **1009** ｜ 本机 1009 ｜ 差 0
+20s                 不再动
账行 task acked ｜ completion queued（收件人 _supervisor 无 client = 正常态）
ghosts 无新行 ｜ gen 1 ｜ 产物 r9-done.md 恰好一份
```
三条判据全到位：`agent` running→gone、**差值从恒为 1 变 0**、发布帧带 `resource=closed + lifecycle=exited + outcome=done`。退休那一拍确实又推一格水位（1008→1009），与「两扇门各自发布」一致。

**形状二 · recycle `a9e87adc`**
```
12:22:39.571 applied op="recycle" held=true   ← 全缓冲仅一条（第二次 applied = 0）
+2s  镜像 exited / gone / closed / outcome 空 / ver.seq 1004 ｜ 本机 1004 ｜ 差 0
+2s  task rejected reason=operator recycle、control acked
12:23:09.364 no plugin answered outcome=Failed waited_secs=30
+30s 镜像 outcome=failed 补齐、ver.seq 仍 1004（水位不动）｜ 本机仍 1004 ｜ 差 0
     ghosts 无新行 ｜ gen 1 ｜ 产物未生成（无第二遍）
```

**结论**：A 的修法在两扇门口都验到；复压七五条不回退；新增项（agent=gone / seq 对齐）两形都成立。**operator 面与镜像一致性结案，我方无保留意见。**

## 复压十：TUI 目视位与快照时钟（`onlyne`/`onlyne-tui` 12:27，`550bfd0`+`1edf815`）——两点通过、一点是我方判据措辞要改
```
① --state all    → 帧内命中 reason=session_dead 1 行；同形不带旗标 0 命中           ✓ 目视位达成
② 不带旗标 vs --state active → 逐字节一致（连页脚都同）                             ✓ 今日 dump 不破坏
③ --state all 隔 9 秒两帧 → 36 行只差第 36 行、差异自第 101 列起 = 页脚 refreshed 时刻
                             去掉页脚，其余 35 行逐字节相同                          ✓ 年龄锚定生效
```
**dev 判定：我提的「同一命令两次跑出逐字节相同」这条判据本身是错的**——两次 `--once` 是两次 pull、两个快照，页脚那格读的正是各自快照的 `refreshed_at`（`ui.rs:738-741`，缺时钟显示 `--:--:--`），差异是正确行为。正确断言是**同一快照渲染两次相同**，仓内用例即如此写（渲染 → 睡 1.1 秒 → 再渲染 → 整帧比较），我这次实测的「除页脚外逐字节相同」就是它的外部版本。
**cosmetic 一条**（120 列下 `reason=` 尾巴挤在图形框文本之后）：dev 判**不改**，理由是那格的既定预算（按可用格裁切、hop 每格保住自己、无 reason 的行逐字节同旧版），改它要动预算规则。记档，不再追。

## dev 侧 e2e 复跑（他交来的逐条结果，我方记档）
- 第一轮 19 条串行：16 过 3 挂（`acp-payload-v2`、`pi-live`、`running-lights`）；用 `7174a93` 临时 worktree 对照，三条都不在本批窗口改动内。
- 第二轮（`4eab276` 修掉两条用例自身的不确定性后）：**18 过 1 挂**，只剩 `acp-payload-v2` —— 而这条**是他这批引入的真回归**：鬼影清扫/共用结清助手把该任务名下**未结清的完成回执**一并拒了，回执行变 `rejected: ghost sweep: the task's ledger row reads acked`，而结局住在这行的 `out_head` 上 ⇒ 读账的人看不见结局。修法：**完成回执原地不动**（它是结算本身的记录；收件人 client 回来就 ack，无 client 的 role 继续排队 = 与我方观察 B 定稿同一口径），加服务端用例钉住「回执保持 queued 且 `out_head` 可读」。
- **与我方观察 B 的关系**：B 的定稿（`queued` = 收件 role 离线的正常排队、记档不改）**不受影响**，这批补的是「清扫不得顺手拒回执」这一条新事实。
- dev 自查纠正一处证据口径：先前那次 `acp-payload-v2`「通过」用的是旧二进制，不算证据；他随后用重建后的二进制复跑全套，最终逐条表见下一节。

### 全套 e2e 最终逐条表（dev 交来，19 条串行；二进制含 `083fc2b` 回执修复与 `4eab276` 两条 case 修复）
门：`cargo test --workspace` 69 目标 / 1087 通过 / 0 失败 / 1 ignored；`clippy --workspace --all-targets -D warnings` rc=0；`cargo fmt --all --check` 清。

| case | 结果 |
|---|---|
| acl-reject · acp-session · exec-headless · gateway-mount · generate-relocate · heartbeat-watch · idempotency · legacy-layout · local-task · reconnect-requeue · requeue-claim · socket-path-length · two-cluster | ok |
| herdr-live · orca-live | ok（SKIP 语义，无可达会话） |
| pi-live | ok（`4eab276` 修后稳定，真跑 pi 凭据与模型回合） |
| running-lights | ok（`4eab276` 修后稳定，十二跳全 acked） |
| **acp-payload-v2** | **FAIL** —— 阻塞子场景的竞态：回执被抢后未 ack → 服务端再投 → 新一代重跑 → 第二 verdict 被拒 → 60 秒预算耗尽。**与清扫无关**（清扫那条回归已由服务端用例结案） |

合计 **18 通过 / 1 失败**。

### 出处（按 dev 纠正口径写）
`acp-payload-v2` 这条竞态怎么处理，**是 dev 的建议、尚未定**：他建议「记后发、如实记为已知未修竞态」。选另一条路（再修一轮）或改主意都由**人**拍板；人若另判，本档案以人的判语为准并留下这是人定的痕迹。1.4.0 的账上这条记 **FAIL**，不记通过。

### 两拍的处置（2026-09-24 下午）
- **发版：人说「不发」**。`packaging/release.sh` 与 `publish.sh` 继续不碰，tag、GitHub release、Homebrew tap、install.sh 一律不动；本仓继续 0 提交 0 push。
- **竞态那拍：人给了口径「别拿去问人，反馈和疑惑都给 dev」** ⇒ 处置回到 dev 手上，他已在改 case（见下），我不再把这条挂成人的选择题。
- **dev 的 case 侧改动（脚本，非产品）**：`wait_root_acked` 从「要求 completion 行 `state=acked`」改成「任务行 `acked` 且该 completion 行 `out_head` 读得出判定」，回执自身 state 打成一行 note。契约取向正确：该 case 承诺的是「合法 `hop-done:` 报告结清本轮并造出子任务」，回执投递本身是 at-least-once。他正连跑 5 次验稳定。
- **claim 语义**（确定性 ack，或退回队列时带「别用同一 verdict 再投」标记）：dev 判它是行为面取舍（今天的语义允许同一次工作跑两遍、账上保住第一个结局），**列为待人拍板的设计项**，不夹进本次修复。我按此挂账一条，不催。
- **我方回给 dev 的一处疑点（待他答）**：放宽后的断言会放过**卡死的认领**——有活 client 的收件方把行抢成 `in_flight` 后永不作答也能通过。`acp-payload-v2` 的收件 role 是起了 client 的，同一句「停在 `queued`/`in_flight`」在我这拓扑（17 行回执的收件人 `_supervisor` 无 client）是常态，在他那里是缺陷信号。建议按**收件方活性**分档：有活 client 时要求回执在 N 秒内到 `acked`，否则 FAIL 并打 state；无活 client 时走宽松形。**我的前提待核**：若 `in_flight` 在实现里可以是服务端自占位（无人认领），这条建议不成立。
- **两处措辞分离（dev 提，我认）**：「每个任务至多一次再投」与「不构成循环」是两条断言——前者由单发 WARN 与复验一证，后者由「第二 verdict 被拒、第一个结局 stands」证。档案与他那条 issue 用同一措辞，免得后人把一次再投读成又一轮循环。

### 我这处疑点的下场：**已证，关闭**（2026-09-24 下午）
- **前提成立**：`in_flight` **恒等于有当前持票的认领方**——`pull` 交出行的那一刻才 `mark_in_flight` 并 arm `DeliveryTicket`（带拉取方 role 与 session）；三条退回路径（`apply_automatic_requeue`、`release_exited_delivery`、重连 `hello` 处理无主票）都把行退回 `queued`。**不存在服务端自占位的 `in_flight`。**
- **dev 据此撤回放宽**：`acp-payload-v2` 的断言退回严格形态（要求回执到 `acked`），只保留失败文本增强（`task=` / `receipt=` / trace / client 日志），并在代码里写明两种形态为何不可互换。
- **那条 case 卡住的机理（dev 读出，我方认可）**：单槽 client 满载时拉取走 `control_only` ⇒ **`completion` 行不被取走** ⇒ 严格等待必然超时。满载的源头是阻塞子场景的迟到重投占掉 planner 唯一槽位（client 侧复现：第五次派活收到了、没开 ACP 会话）。**这条失败是「容量把回执挡在拉取之外」，不是 at-least-once 语义问题**；在无 client 拓扑里不可能发生。
- **处置（dev 自定，经人放行「反馈和疑惑都给你」）**：`acp-payload-v2` 选 **修**（放宽断言不做，修容量那条链）；claim 语义仍挂**待人拍板的设计项**；发版按人判「不发」继续按兵不动。
- **我回给 dev 的两件**：① 综合口径——「回执停在 `queued`」有两个方向相反的成因（无 client ⇒ 消费者对投递不可见，常态；有 client 但满载 ⇒ `control_only` 主动排除 completion，缺陷信号），故分档断言要按「有无活 client × 是否满载」两维定；② 判别实验——把该 role 的 `max_sessions` 从 1 抬到 2、断言保持严格，若直接变绿即证成因在容量，若仍红则卡在票证生命周期或回执投递语义。附一条我方旁证：复验三（`becbd03c`）单槽满员时 `probe`/`cancel` 确实落到占最后槽的那一代、该族的活整段排队等槽，与 `control_only` 那句文档逐字对得上。

## 复压十二：`39408cb` 两态与 `ffa6d1a` 槽位释放（15:14 三进制；dev 报全套 19/19 绿、1100 通过）
**出处（dev 给原句，我未在场）**：人对无 client 的排队投递另下过一句判语——「无client时阻塞在server等重试，超过宽限次数后判失败并写日志。」与 A/B 那两句、「不发」、「反馈和疑惑都给 dev」同批给出。**档案标注：人裁定，dev 转述，该 peer 未在场。**
**调和口径（两句同时为真）**：默认 `requeue_ttl_secs = 0` 时我们的观察 B 定稿逐字不变（`queued` = 收件 role 无 client 的正常排队，常驻）；配了预算（非 0）才补上终态：行变 `expired` + `reason=requeue_ttl` + server 一行日志。定稿没有被推翻，是常态那一句多了「有预算时」的另一半。

**① `requeue_ttl_secs` 两态：全中（`88d0f5cd` 一轮现场）**
```
默认（未设=0）  全账：completion 14 queued + 1 acked ｜ control 6 queued + 14 acked ｜ task 15 acked + 10 rejected
               queued 回执合计 20 行，expired 0 行，faults 0 条          ✓ 与定稿逐字一致
设 20 秒 + reload（插在 spec 第 14 行的 [server] 之后，避开注释区）
   +30 秒      completion 14 queued → **14 expired** ｜ control 6 queued → **6 expired**
               acked 的 1 + 14 行不动                                  ✓ 只有无认领方的排队行被判失败
   faults 含 requeue 的：**0 条**（dev 预告：投递行自身的结局不落 faults 表，那是会话级发现的面）✓ 属设计
   server.log 命中 21 行：`onlyne_server::relay: queued delivery expired after its recipient role
                stayed offline msg_id=… task=… role=_supervisor age_secs=…`  ✓ 形态与他给的逐字相同
复原           删掉该行 → reload ok → `spec_diff` 回 no changes          ✓
```
（`state.db` 跑前已备份进 `/tmp/onlyne-bug-141/round12/state.db.pre`；那 20 行现状为 `expired`，是这次实验的产物，非事故。）

**② 槽位释放（`ffa6d1a`）：两条入口都关着，改交等效判据（不记失败）**
- 入口一 `repair retry`：`conflict: task is settled; the frozen ledger transition table has no edge back to queued` ⇒ 造不出第二份 completion。
- 入口二 让插件补报：任务书写明「对同一任务连调两次 complete」，`4ba33af9` 实际只报一次，07:21:56 即 `retiring idle session resource`，此后 15 个采样点无 `a second verdict arrived for a settled task` ⇒ pi 侧 complete 落地即收会话，**没有第二次上报的机会**。
- **历史对照（解释为何这是不可达分支）**：`a second verdict arrived for a settled task; the first one stands` 我在旧 build 上抓到过两次活体（`becbd03c` 17:07:57、17:09:21），那是重投被放行、真跑出新一代时才有。新 build 当场结掉重投（`redelivery … settled without running it`），第二代不存在 ⇒ **该分支在 orca/pi 真机不可达**，活体证据只能在 dev 自己的 acp 夹具里（5/5 过、planner 单槽）。
- **等效判据（功能面：不永久占槽）**：第一单退休后该任务只剩 1 行 `exited / gone / closed / ver 1008`、全库 `working` 会话数 **0**、账行 `task acked` + `completion queued`；**紧接着投的第二单在 +4.0 秒起出新会话**（`working / running / attached`、gen 1），并在 15:26:59 完成（`exited / done`、ver 1006）。症状若还在，第二单起不来。
- **重投收敛的旁证（合「一次再投 ≠ 循环」）**：`88d0f5cd` 的 `redelivery of a finished task settled without running it`（07:17:42，msg `655761a1`）一次即止。
- **交回 dev 两条**：① `repair retry --help` 那句「Put the task's still in flight…」按设计不给 settled 回退就得补完文案；②「操作者能否强制重投已结算任务」记为**待人拍板的设计项**，与 claim 语义同批，不夹进 1.4.0。
- **收场**：server 与 scriber 已停、探针出栏、spec 复原、`state.db` 跑前有备份（`round12/state.db.pre`）。
- **dev 的落点（复压十二之后）**：`4f4826e` 把我这段历史对照与等效判据写进 CHANGELOG（第一单退休 → 镜像 `exited/gone/closed` → 全库 working 0 → 下一单 **+4.0 秒** 起；并写明「这条支需要能重跑已结算任务的后端，重投闸已在第二代之前结掉」，指向 e2e 那条 5/5 单槽覆盖）；`bc0a5df` 落了 `repair retry` 的 help（转换表不动 —— 终态无回边就是账本的意义；help 里写明拒收与「重跑已完成的工作等于发新任务」）。
- **档案地位（他明示）**：本文件是这轮所有活体证据的来源（复压五~十二），他 CHANGELOG 引的读数与措辞都出自我这里、未另立一套。⇒ **本档案是承重件**：后续任何形状仍由他转读数给我、由我入档，避免两份档案分叉。
- 「操作者能否强制重投已结算任务」他补了一条依据并同意挂**待人拍板的设计项**：现在没有任何入口能造出第二份 verdict（`repair retry` 被转换表关死、插件路径被重投闸挡、新 build 连第二代都不跑），所以「强制重投」若要，是一条**新增的、有意的能力**，不是缺陷修补。

## 复压十一：清扫与完成回执那一形（`083fc2b`）——**谓词在 orca/pi 后端造不出来，两次现场**
dev 的配方第 3 步要求「插件完成后仍报心跳」，让镜像滑回 `working`（`projection.rs:85-101` 裸心跳合成 `working/running/attached`），凑齐「镜像 working + 任务行终态 + 属主在线」三件事，清扫才开火。

**第一发 `b91a43f4`（同一会话补一句话）**
```
12:52:08 投出 ｜ 12:52:26 outcome=done（ver 1007，exited/running/attached）
+0s 往会话 tab term_699eeebc 发一句话 ｜ +8s 镜像翻 gone/closed、ver 1008（51c23d9 的退休补发布再次显形）
始终 exited，从未滑回 working；ghosts 无新行；completion 保持 queued、out_head 可读
```
**第二发 `6fb566a6`（complete 之后继续干 100 秒活）**
```
12:56:06 投出 ｜ 12:56:16 outcome=done（ver 1004）
+0…+26s  镜像 exited / running / attached、ver 钉在 1004 —— 会话确实活着，retire 被 attached transport 挡住
+30s     退休发布 gone/closed、ver 1005
全程零心跳上达服务端 ⇒ 1004→1005 之间无帧；从未 working；ghosts 无新行
停服后以只读库复核：task acked ｜ completion queued、out_head=「r11b 已按要求先交完成」
```
**结论（dev 判 1+3 合并，我方一句自我更正）**：我先前写的「活体证据缺失」**要收窄成「缺的是修复后的同一形状」**——那个合取在真实进程上出现过，地点是他的 e2e：修复前跑 `acp-payload-v2`，其阻塞子场景天然形成「镜像 `working` + 任务行已终态 + 属主在线」，清扫开火并拒了回执（`rejected: ghost sweep: the task's ledger row reads acked`，而结局住在那行 `out_head` 上）。

三块证据按此口径入账：
1. **合取的「前」= 真进程活体**：修复前 `acp-payload-v2` 那一次（真实进程、真实清扫、真实回执）。
2. **合取的「后」= 服务端用例确定性钉住**：`the_sweep_leaves_a_queued_completion_receipt_alone`（一条用例同时断镜像被搬 + 回执仍 `queued` 且 `out_head` 可读）。这条是主证据。
3. **我方两发补上两件活体事实**：真实链路上回执确实以 `queued` + 可读 `out_head` 留存；`51c23d9` 的退休补发布又显形两次（+8s 与 +30s，`gone`+`closed`、ver 各推一格）。复验六那条 `seq_before 1003 → seq_after 1004` 继续作为「谓词成立时清扫仍开火」的独立活体证据。
4. **同一条 case 的失败形态换了一次**（复压十一的第四块旁证）：修复前那行是 `rejected: ghost sweep: …`（清扫拒回执），修复后是 `in_flight`（回执被抢未 ack）。同一形状、同一条 case，行的 state 从 `rejected` 变成 `in_flight` ⇒ 清扫不再碰它，这是 `083fc2b` 生效的现场反证；剩下的失败换成了另一条竞态（未 ack → 再投 → 新一代重跑 → 第二 verdict 被拒 → 60 秒耗尽），与清扫无关。

**机制答复（dev 给，我第二发是实测依据）**：服务端把**不带投影的裸心跳**合成成 `working/running/attached`（`projection.rs:85-101`），所以只有结算后仍有裸心跳上行的后端能把镜像从 `exited` 滑回 `working`。pi + orca 造不出：完成路径发布一次（`Done + Accepted → exited`）、退休再发布一次（`agent → gone`、资源 `closed`），插件的会话在完成被 ack 后就结束，**没有第三个心跳源**。我那 26 秒里「ver 钉在 1004 + 会话活着 + 退休被 attached transport 挡住」三条同时成立，正说明**活着的是 agent 进程，不是心跳源**。⇒ 本仓要出这形状得把某个 role 换成 acp 后端，dev 判不值得（换来的只是复述他已有的活体），同一事实他已写进 `docs/operations.md` 的清扫一节。该形在 Alexandria 拓扑**不可复现**，定档。

**本轮自伤两处（记档防复发）**
- `ghost_sweep_secs` 我用 `str.replace('[server]', …)` 插旗标，命中的是**注释行里的 `[server].cert_pin` 字样**（spec 第 8 行），`reload` 报 `spec.toml:9: invalid floating-point number`。复原脚本反转了同一替换，`git diff` 只剩本来要改的三处，事后已核。教训：**改配置文件要按行定位段落标题，别按字符串首现替换**。
- 两发的收尾（停 client、停 server、探针产物出栏）都跑到了；`pgrep` 复核 server/client 均为无。

### 本轮踩坑（可复用）
- `orca terminal read` 的缓冲会留着同一 tab **前一个实例**的 `server link ready role=` 行 ⇒ 按它判 role 会把两个 tab 认成同一个 role。定 role 要用「最近一次启动命令行」，或直接用 `ps --workspace` 认进程。重启脚本因此加了守卫（role 集合不齐就不动手），一次真的拦住了误杀。
- 同机 dev 跑 e2e 会短时起落别的 `onlyne-client` 进程 ⇒ `ps` 计数会飘（一次读到 8 行）。过滤条件必须含本仓 ws 路径。
- `onlyne send` 无 `--reason`（属 `control`/`repair` 族）；`--state` 取值只有 `active`/`all`，`any` 会被 clap 拒。
- 脚本一律 `write` 成文件 + `python3 -m py_compile` 再跑（`x.get(k) or` 漏 `{}` 手误此前连中三次）。
