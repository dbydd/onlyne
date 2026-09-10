# 验证场景（TEST，修订一 2026-09-06）

射后不理模型：任务 = session = 一跳。每跳 out 即 done 并退出，无等待、无回调、
无记账。测试树统一建在临时目录（非本仓库），root 下
`.agents/.schedule/{a,b}/template.workspace.jsonc`。

> 实测状态：五场景已用 headless stub agent（`SWARM_STUB_AGENT=1` + python 驱动
> `swarm_ready` / history 轮询 / `send_message` 写 out）全跑通。stub 脚本见本仓库
> `e2e/` 目录，一键复跑 `./e2e/run_e2e.sh all`。
> 旧 `reply_to` 头按普通消息处理，不建 session。

## 1. 单链单跳（submit → ready → 投递 → out → 回收）

1. `onlyne-swarm run` 启动，`sync` 生成 `.ws/a`，daemon 上线。
2. `onlyne-swarm submit --to a --payload task1.md` 返回 `task_id`。
3. 断言：a 的 terminal 建立，`swarm_ready` 到达后 loopback/in 收到带 swarm 头的任务；
   stub 写出 out 后任务状态 `done` → `closed`，terminal 回收（orca 侧无残留），
   ledger 追加一行 done。
4. `onlyne-swarm list` 全 `closed`，`status` 无 orphan/dangling。

## 2. 一发多收扇出（零记账）

1. root 提交父任务到 a，a 激发 b 与 c 各一个子任务后立即 out 退出。
2. 断言：父 out 落地即 done，与子女状态无关；三个任务各自 `closed`；
   ledger 三行（父 done + 两子 done），`transfer_send_to` 指向父 task。
3. 并发版（race，子任务零延迟同时 out）同样全 closed；无写锁、无合并丢失概念。

## 3. 回边自激发循环（环路 + 手动终结）

1. a 与 b 的 `back_edges` 互指，对方收到任务即激发新任务（载荷递增计数器）后 out 退出。
2. 断言：任务数线性增长，terminal 随起随收，无钉死 session；
   TUI 任务表滚动，族深度计数告警标红但运行不受干扰。
3. `onlyne-swarm cancel <族task_id>` 后：全族 cancelled，无新任务产生，
   TUI 无残留 running，ledger 追加 cancelled 行。

## 4. 同 workspace 并行

1. 向同一 workspace 提交两个任务。
2. 断言：两 terminal 并存，各自 out 后 closed，互不阻塞。

## 5. 通用断言（每场景）

- `task_id` 唯一，无重复投递建双 session。
- 无 swarm 头的消息不建 session（普通 loopback 行为不变）。
- 带旧 `reply_to` 头的消息按普通消息处理。
- 调度器重启后 running 任务记 failed 台账行，任务不重放。
- 普通模式 session 看不到 swarm_* 工具；swarm 模式 session 看不到通用收发工具。
