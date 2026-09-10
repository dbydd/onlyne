# 不打断在途跳的换代配方（R4 / 0.7.0）

适用：把 `onlyne-swarm` 从 R4 之前的构建换到 0.7.0（hop 状态机），
**不打断在途 hop**，并带 DB 迁移失败的回滚路径。

R4 换代必须**三件同时换**：

1. daemon `onlyne 0.6.0`：处理 `swarm_busy` / `swarm_idle` IPC op；
2. scheduler `onlyne-swarm 0.7.0`：路由 busy/idle、维护 hop_state 与超时；
3. plugin `pi-onlyne 0.9.1`：在 `agent_start` / `agent_end` 上报 busy/idle，
   且将 `ONLYNE_SWARM_TASK` 作为 history replay 硬绑定；

只换 scheduler 会让 0.9.1 插件发出的 op 落到不识别它的 0.5.2 daemon，请求
返回 unknown op，scheduler 收不到迁移事件；此时 hop 仍按旧 running 路径运行。
因此三件任一不齐都不要开换代窗口。

判据来源全部是代码事实，不靠记忆：

- `cargo install --path` 只替换磁盘上的二进制文件；在跑的 scheduler 用旧
  inode 的进程镜像继续跑。替换文件本身不动进程。
- 只有 `ipc::serve` 打开 DB（`src/ipc.rs` 的 `Db::open`）；所有 CLI 读命令
  （`status`/`list`/`attach`）走 socket。迁移**只在 scheduler 启动时发生一次**。
- 迁移是加法且幂等：`ALTER TABLE tasks ADD COLUMN hop_state TEXT NOT NULL
  DEFAULT ''`，前面有 `pragma_table_info('tasks')` 存在性检查（`src/db.rs`）。
- R4 之前的二进制 SELECT 都写显式列名，多出的 `hop_state` 列被忽略。
  **回滚不需要动 DB。**

## 0. 前提取证（scheduler 仍在跑，不重启）

```bash
ROOT=/path/to/swarm/root
cd "$ROOT"
sqlite3 .onlyne/swarm.db "PRAGMA table_info(tasks);" > /tmp/tasks-before.txt
stat -f %Sm "$(command -v onlyne)" > /tmp/onlyne-bin-before.txt
stat -f %Sm "$(command -v onlyne-swarm)" > /tmp/swarm-bin-before.txt
# Linux: stat -c %y
onlyne --version                  # 当前旧值期望 0.5.2
onlyne-swarm --version            # 当前旧值期望 0.6.1
onlyne-swarm status | jq '.data | {tasks_by_state, hops, alerts}' > /tmp/status-before.json
onlyne-swarm list --state running
onlyne-swarm list --state pending
```

记下在途 task id。`status` 在 scheduler 活着时退出 0；**没有 scheduler 时它
退出非零**（0.7.0 起），别把非零误读成故障。

## 1. 装三件新二进制（不重启，不影响在途）

```bash
cd /path/to/onlyne
cp "$(command -v onlyne)" /tmp/onlyne-0.5.2.bak
cp "$(command -v onlyne-swarm)" /tmp/onlyne-swarm-0.6.1.bak
npm --prefix harness/pi-onlyne ci   # 或按现有本地安装方式装 pi-onlyne
cargo install --path . --locked --offline
cargo install --path harness/onlyne-swarm --locked --offline

onlyne --version                    # 期望 0.6.0
onlyne-swarm --version              # 期望 0.7.0
node -e 'const v=require("./harness/pi-onlyne/package.json").version; if(v!=="0.9.1")process.exit(1); console.log("pi-onlyne",v)'
```

此步之后在跑的 scheduler 仍是旧构建；`--version` 反映磁盘，不反映进程。
三件版本全部命中后才能进入第 2 节。

## 2. 选窗口

**不打断在途跳的唯一硬条件是：此刻没有 `pending`/`running` task。**
第 0 步的两个 list 都为空时才是真静默窗口。

若不想等：

- 0.7.0 的 R5 收养路径会在重启时探活并收养仍在跑的会话
  （`state=running` + `ledger_state=adopted`），不会把它们判死。
- 代价是：收养会把该 hop 的 dwell 时钟重置为重启时刻，TUI 上带
  `(adopted)` 标记；DB 里多一行 `adopted` ledger。业务不丢，但现场变了。
- 因此生产环建议等静默窗口；不接受 dwell 重置就不要在途重启。

## 3. 停旧、起新

前台（独占 pane）：

```bash
# 在 scheduler 所在 pane 按 Ctrl-C；等它打印
# "shutting down; stopping managed daemons" 后返回
cd "$ROOT" && onlyne-swarm run
```

detached（0.7.0 才有）：

```bash
onlyne-swarm stop          # SIGTERM → 最多等 5s → SIGKILL；清 pid + sock
cd "$ROOT" && onlyne-swarm run --detach
tail -f .onlyne/logs/scheduler.log
```

**不要同时留两个 scheduler。** 新 scheduler 启动时 `serve()` 会先探 socket：
有活 scheduler 应答就直接报 `already running` 退出。DB 迁移只在新进程启动时
执行，此时旧进程已退，无 SQLite 锁竞争。

## 4. 验收

```bash
sqlite3 .onlyne/swarm.db "PRAGMA table_info(tasks);" | grep hop_state   # 迁移生效
onlyne --version                                                         # 必须 0.6.0
onlyne-swarm --version                                                   # 必须 0.7.0
node -e 'const v=require("/path/to/onlyne/harness/pi-onlyne/package.json").version; if(v!=="0.9.1")process.exit(1); console.log("pi-onlyne",v)'
onlyne-swarm status | jq '.data | {tasks_by_state, hops, alerts}'       # hops 出现
onlyne-swarm status | jq '.data.not_swarm_ready'                        # 应为 []
# 语义 smoke：scratch root 提交一个正文首行 `> hop-failed:` 的 out，
# 期望 DB state/ledger_state 都是 failed 且 out_head 保留该正文；
# 后文才出现 marker 的正常 out 仍应是 done/closed。
```

对照第 0 步：在途 task 若走了收养，`ledger_state=adopted`、状态仍 `running`；
若静默窗口重启，状态计数不变。三件版本任一不符即停止验收，先恢复三件旧产物。

## 5. 回滚

```bash
onlyne-swarm stop    # 或 Ctrl-C 前台 pane
cp /tmp/onlyne-swarm-0.6.1.bak "$(command -v onlyne-swarm)"
cp /tmp/onlyne-0.5.2.bak "$(command -v onlyne)"
# 插件按安装方式恢复 0.8.1；若用仓库 checkout，回到 72e5fa6
onlyne --version             # 回到 0.5.2
onlyne-swarm --version       # 回到 0.6.1
cd "$ROOT" && onlyne-swarm run
```

回滚同样三件同时回。0.9.1 插件可以接旧 daemon，只是 busy/idle op 被拒；旧
插件不能接 0.7.0 scheduler 的新事件假设。因此插件、daemon、scheduler 的
验收必须成组处理。**不需要回滚 DB。** 旧二进制忽略 `hop_state` 列。不要执行
`ALTER TABLE tasks DROP COLUMN hop_state`：无必要，且删列会重写表、在
有 scheduler 时加锁。

## 6. 护栏

- 换代验证只在 scratch root 做；**不要对生产 root 发 SIGINT / 做收养演练**。
- 不要在生产环在跑时执行 `e2e/run_e2e.sh`：它的 cleanup 已按物理路径只杀
  测试树下的 scheduler（`kill_tree_schedulers`），但 `build` 步骤重，且
  多场景会反复起停，没必要冒险。
- `onlyne-swarm status` 的退出码是探活判据；无 scheduler 时非零。
- 换代验收固定三行：`onlyne --version` = `0.6.0`、
  `onlyne-swarm --version` = `0.7.0`、pi-onlyne `package.json.version` = `0.9.1`。
- `pi-onlyne 0.9.1` 额外 smoke：新 pane 的 `ONLYNE_SWARM_TASK` 与
  第一条 custom message 的 swarm header task_id 必须相同；任务尚未入 history 时
  session 应等待 live delivery，绝不回放 workspace 内旧 payload。
- 记录换代前后的 `pragma table_info(tasks)`、`onlyne` 二进制 mtime 与
  `onlyne-swarm` 二进制 mtime；三件版本号共同构成换代指纹。
