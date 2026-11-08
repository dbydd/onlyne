# worker 没有协议级身份：跨线程 review 重开工单，UI 只能扫聊天 DOM

日期：2026-11-07
报告人：dsh-onlyne 集成（omd-issue-runner + DS-Bot 前端）
路径说明：omd-issue-runner 已迁入 `onlyne/external-integrations/dsh-onlyne/`，迁移前位于 `oh-my-dsh/plugins/`；`omd-context-tavern` 仍留在 `oh-my-dsh/plugins/`。行号以当前树为准。

## 现象

1. Bot 在另一个 chat 线程里发 `review`，runner 找不到原 job，Bot 改发 `issue`，同一份工作出现第二台 VM。commit `f9f7820` 的提交说明记录了这个现场："the Bot filed a second issue, which forked a second virtual machine for the same piece of work"。
2. worker 不出现在 dsh 的 subagent 目录里，UI 只能从聊天气泡文本里找 `onlyne://runner/issue/<id>`，再包成 chip。
3. 机器名被当成 worker 身份的反向来源：UI 用机器名最后一段还原 worker 地址。

## 根因分析与证据

1. 线程是唯一的关联键：`byThread` 只比较 thread 的 owner 与 id（`external-integrations/dsh-onlyne/omd-issue-runner/src/jobs.ts:321-326`），线程不同就 miss。`f9f7820` 补了 `meta.data.worker` 查找（`src/jobs.ts:334-342`），修的是调用方的记忆。信封本身仍然没有工作单元字段：lib 的 `meta` 只有 thread、in_reply_to、relation、mentions、parts、data（`packages/lib/src/types.ts:43-56`）。
2. `acceptIssue` 无条件建 job 并入队（`external-integrations/dsh-onlyne/omd-issue-runner/src/jobs.ts:439-459`），没有幂等键。同一个人重复投递同一条 issue，就是两个 job、两台机器。
3. worker 由 `ctx.agents.create` 创建（`external-integrations/dsh-onlyne/omd-issue-runner/src/backends/dsh-agent.ts:125-143`）。`ctx.subagents` 因沙箱策略拒绝外部 worktree 的 cwd，理由写在同一个文件的开头注释（`src/backends/dsh-agent.ts:6-18`）与 README（`external-integrations/dsh-onlyne/omd-issue-runner/README.md:188-203`）。这种子会话不进 subagent 目录，也没有 `subagent/descriptor`，README 把这条写成已知限制（`README.md:268-270`）。
4. UI 侧因此只能靠 DOM：`WORKER_MENTION` 正则扫 `.data-chat-flow`（`oh-my-dsh/plugins/omd-context-tavern/src/client/index.js:3336-3342`），`MutationObserver` 监听整棵聊天树（:3469-3480）；`workerOfMachine` 从机器名 `...-issue-<id>` 反推地址（:2338-2345）。

## 影响

1. "一台 VM 干一件活"没有协议保证，重复工单直接烧双份算力与端口。
2. worker 的进度与生命周期对用户不可枚举，只能靠聊天记录里出现过的那行文本；新开窗口、换会话就看不到。
3. 机器名一改，UI 的 worker 链接就断。机器名是 provider 侧标识，不该承担身份。
4. 同一份工作出现两台机器后，`review` 只能命中其中一台，另一台靠 sweep 回收。

## 建议修法

1. 信封加一级工作单元字段：`meta.work = { unit, parent?, attempt }`（写进 `meta.data` 也行，但要在 `Message` 里有一等字段，别让每个调用方自己编）。`issue`/`review`/`close` 都带；runner 以 work id 做幂等键，重复 `issue` 返回已有 job 的 worker 地址，不新建机器。
2. runner 把自己持有的 worker 作为可枚举资源发布（`onlyne://runner/workers` 或资源节点），字段含 worker 地址、machine、thread、state、startedAt。UI 读这个，不扫 DOM，不从机器名反推。
3. dsh 侧给这类运行时子会话一个可见目录条目（只读投影即可），让 subagent 目录能列出它；做不到时至少在 `ctx.onlyne` 的 registry 视图里标明"由 issue-runner 持有的 worker"。
4. 机器名与 worker id 解耦：机器名只给 provider 用，worker id 独立生成（uuid 或 issue id），两者映射关系在 runner 内维护。

## 验收判据

- 从与 issue 不同的线程发 review 且不带 worker：返回明确错误并列出 live workers；带 worker 时续用同一台机器，不新建。
- 同一 work id 的 issue 重复投递两次，只存在一个 job、一台机器。
- worker 列表由接口返回；关闭聊天渲染或换窗口后仍能看到全部在跑 worker。
- 人为改动机器名后，UI 显示的 worker 地址不变，`review` 仍能命中。
