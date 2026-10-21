# Onlyne 现场运维问题报告（2026-10-21）

本报告汇总今天在 `research-chamber` 与 `daily-workbench` 跨集群联调实机运维中抓出来的 5 个关键问题，附实测复现路径、数据表证据与代码定位。供 Onlyne 开发者今晚精准修 bug。

---

## 问题 1：跨集群同名角色引发的 Socket 解析退路失效（二义性拒绝）

### 1. 现象
- 在 `ResearchChamber` 目录下运行 `omp` 会话，通过 `onlyne_send` 插件工具向本集群角色发消息，返回 `sent to <role>`，但本集群的 `state.db` 账本和 `events` 表中**查不到任何发送记录**，甚至全盘扫描两个集群的数据库都不见痕迹。
- 角色完全收不到消息。

### 2. 根因链条（代码级验证）
- 位于 `plugins/onlyne-agent-pi/src/socket.mjs:236` 的 `resolveResidentSocketPath`：
  - 机器级运行目录 `/tmp/onlyne-501/` 下注册着两个不同集群的客户端：
    1. `/tmp/onlyne-501/8a5a6010026da04e.json`：`root = .../daily-workbench/lazuli`（PID 57794）
    2. `/tmp/onlyne-501/7dff5ee50d5db9be.json`：`root = .../ResearchChamber/lazuli`（PID 40661）
  - 代码第 250-255 行规定：
    ```javascript
    if (role) {
      const byRole = candidates.filter((entry) => entry.role === role);
      if (byRole.length === 1) {
        return join(dir, `${workspaceDigest(byRole[0].root, options)}${SOCKET_SUFFIX}`);
      }
    }
    ```
  - 当同名角色 `lazuli` 在联邦中多于 1 个客户端时，`byRole.length === 1` 直接判定为 `false`！
  - 随后退入第 258 行 `resolveSocketPath`，因为当前执行目录为 `ResearchChamber`（上级目录而非具体的 `lazuli` 目录），第 287 行 `covers(entry.root, here)` 判定子目录不覆盖父目录，再次落空；
  - 最终代码回退到了第 260 行 `socketPath(workspaceRoot)` 生成的**未被任何守护进程监听的孤儿 Socket**：`/tmp/onlyne-501/e0c34f5fc2e9cd50.sock`（该 socket 实际被 server 作为 admin 监听，或者无进程监听），导致插件发出去的包成了黑洞。

### 3. 修复建议
- 在 `resolveResidentSocketPath` 中，当 `byRole.length > 1` 时，应当进一步比对候选者的 `root` 与调用者 `cwd` 是否属于**同一个祖先集群目录（即向上查找 `.onlyne/spec.toml` 相同的集群）**，优先绑定同一集群下的同名角色，而不是机械地直接丢弃选择。

---

## 问题 2：离线角色的未消费消息产生“全频段阻塞”

### 1. 现象
- 整个集群的角色轮流报 `heartbeat_missing` 与 `stalled`，在线角色无法接收任何新派发的任务，仿佛集群发生了全面崩溃。

### 2. 根因分析
- 在 `spec.toml` 中，`_supervisor` 与 `lazuli` 被定义为管理节点，但此前未在后台为其启动独立的客户端进程（`sessions = 0, state = offline`）。
- 各科研角色（如 `seaice-flow-matching`, `eddy-causal-dag`）在执行完毕后，默认会将 `completion` 和 `note` 回传至 `_supervisor` 或 `lazuli`。
- 服务端队列（`queued`）不断堆积达到 38 条。由于 Onlyne 的投递调度器在处理阻塞队列时，对已连接信道的事件轮转受到排队头阻碍，反向拖垮了所有在线客户端的信道调度。
- 随后在线角色的 `session` 因等待前序事务推进超时，被服务端触发 `turn_end_without_complete` 并最终打上 `stalled` 故障标记。

### 3. 修复建议
- 服务端调度器必须建立**离线死信丢弃/隔离队列**机制：对于目标角色处于 `offline` 且达到重试上限的非关键消息（尤其是 `note` 与无后续依赖的 `completion`），不得阻塞整个服务端的调度分发循环；
- 当管理节点离线时，钩子（如 `supervisor-inbox.py`）应当接管消息落地，服务端标记其为 `delivered_to_hook` 而不是永久占用 `queued`。

---

## 问题 3：对离线角色的消息执行 `reject` 会遭遇 Socket 拒连死锁

### 1. 现象
- 运维人员试图通过 CLI 清理阻塞在离线角色（如已被封存的 `meltpond-optical-decoupling`）上的残留控制消息时：
  ```bash
  onlyne reject --msg-id <ID> --reason "..." --workspace artifacts/meltpond-optical-decoupling --force --yes-i-am-supervisor-not-other-role
  ```
  直接报错：`Connection refused (os error 61)`，指令被拒绝执行。

### 2. 根因机制
- CLI 的 `reject` 逻辑强依赖一个存活的客户端 Socket 来完成协议帧发送；
- 但如果目标角色本身已经死亡/封存，其对应的客户端 Socket（`/tmp/onlyne-501/<digest>.sock`）早已关闭；
- 此时用户陷入死锁：**因为角色死了所以消息卡在队列，而要清理消息又必须连接已死的角色**。
- **实测解法与漏洞确认**：我们今天通过换用一个**依然活着的兄弟角色工作区**（`--workspace lazuli`）发送针对该消息 ID 的 reject 帧，才成功将服务端的死锁记录解开。

### 3. 修复建议
- 管理员运维命令（携带 `--yes-i-am-supervisor-not-other-role`）应当允许直接通过服务端的 Admin Socket（`--server-root`）执行强行清理，跳过对特定 Client Socket 的依赖。

---

## 问题 4：CLI `onlyne send` 无法发送轻量级 `note`（强制要求 task 语义）

### 1. 现象
- 在日常管理中，主管向项目群发广播通知（如“已放宽静音模式”、“数据已到位提醒”）时，希望使用非阻塞的 `note`。
- 但使用 CLI 执行：
  ```bash
  onlyne send --to <role> --text "..."
  ```
  该命令在底层**只能生成 `kind: task`**。
- 这导致群发的每一条通知都被接收角色的 Client 认定为新任务，强制分配新的 `task_id` 并占用其 `max_sessions = 1` 的排他槽位；
- 导致真正的科研任务排在后面迟迟无法下发，或者角色的旧 task 未 complete 前新 task 报 `rejected`。

### 2. 修复建议
- `onlyne send` CLI 亟需开放 `--kind [task|note]` 参数支持，避免运维提醒滥用 task 队列生命周期。

---

## 问题 5：`pi-onlyne` 插件在 Session Resume 时未重新触发 Role Prose 注入

### 1. 现象
- 当角色的 omp 会话因为重启或异常退出，使用 `omp --resume <session_id>` 恢复时，终端中打出高危警告：
  ```
  [pi-onlyne] before_agent_start carries no prompt sections; the role prose reached no instruction layer
  ```
- 此时模型开始执行任务，但其上下文的 System Prompt 中**完全丢失了角色的系统提示词（Role Prose）**。角色忘记了自己的身份规范与纪律约束（如科研禁用词、实验方法要求），直接蜕化为通用无约束状态。

### 2. 根因定位
- 查 `plugins/onlyne-agent-pi/src/index.ts:426` 与 `pi-surface.mjs:265`：
  - `roleProseText` 只在 Client 与 Plugin 完成首次 `welcome` 握手时由客户端下发填充；
  - 但在 `--resume` 路径下，插件并未重新向客户端申请获取 Role Prose，导致 `roleProseText` 保持空字符串；
  - 进而 `applyRoleProse` 在检测到空字符串后判定失败，未将角色定义写入 `systemPromptOptions.sections`。

### 3. 修复建议
- 在 session resume 握手或重连时，必须主动从本地 `.onlyne/config.toml` 或向客户端发送状态同步请求，重新拉取并注入完整的 Role Prose。
