# Onlyne 代码库深度审查 - 第二轮潜在 Bug 清单

**审查时间**：2026-09-19  
**审查范围**：全代码库（server/client/session/proto/plugin）  
**审查方式**：5 路并行 scout 只读分析  

---

## 审查状态

- ✅ **AuditClient** 完成（crates/onlyne-client 全部源码）
- ✅ **AuditSession** 完成（crates/onlyne-session 全部源码）
- ✅ **AuditPlugin** 完成（plugins/onlyne-agent-pi）
- ✅ **AuditProto** 完成（proto/adapter/frame）
- ❌ **AuditServer** 超时失败（relay/router/projection 未完成）

---

## 审查结果摘要

4 个审查完成，发现 **60+ 个潜在问题**，分级如下：

### Critical 级（数据错误/重复投递/状态机崩溃）
约 **10-15 个**，包括：
- Client: intent 无限重试毒丸、hello_live_tasks DB 错误静默、control note 无鉴权
- Session: apply 窗口竞态、mismatch counter 永不重置、adoption seq 不一致
- Plugin: seq 单调门违背、多任务 seq 交叉
- Proto: frame 解码器无 buffer 上限、causality hop 溢出

### Major 级（资源泄漏/逻辑错误/性能问题）
约 **25-30 个**，包括：
- Client: parked 单槽覆盖、dispatch 失败泄漏、capacity 门位置错误
- Session: 持久化不一致、backend close 在锁内、isolate 永久占用
- Plugin: pendingCompletions flush 中断、deliveredTo 误记 queued
- Proto: adapter unbounded channel、ImagePart 预解码内存

### Minor 级（防御加固/边界条件/日志缺失）
约 **25-30 个**，包括：
- 错误静默吞掉（unwrap/log warn）
- parse_* 的 FromStr Err 无日志
- control settle watchdog 未清理
- agent_install 非原子占用

---

## 详细清单

完整问题清单已保存在各 scout 的输出中：

- `agent://AuditClient` - 客户端问题清单（~20 个）
- `agent://AuditSession` - 会话层问题清单（~25 个）
- `agent://AuditPlugin` - 插件问题清单（~10 个）
- `agent://AuditProto` - 协议层问题清单（~10 个）

每个问题包含：
- 文件路径 + 行号
- 问题描述（一句话）
- 根因分析
- 触发条件
- 影响范围
- 优先级（Critical/Major/Minor）

---

## 高危问题速览（Top 10）

### 1. Client intent 无限重试 + 无 LIMIT（Critical）
**文件**: `crates/onlyne-store/src/client.rs` - `flush_order`  
**问题**: intent 表查询无 `next_attempt_at` 过滤、无 LIMIT、无超时清理  
**影响**: 一个坏 payload 变毒丸，flush 永远重试，阻塞后续所有 intent  

### 2. Plugin seq 单调门违背（Critical）
**文件**: `plugins/onlyne-agent-pi/src/agent.mjs` - `heartbeat()`  
**问题**: 多任务场景下 `this.seq` 全局递增，task A 的 beat 可能用 task B 推进后的 seq，触发服务端 seq 门拒绝  
**影响**: 心跳被拒、liveness stamp 不刷新、heartbeat_stale 误报  

### 3. Client control note 无鉴权（Critical）
**文件**: `crates/onlyne-client/src/session/dispatch/reports.rs` - `on_control`  
**问题**: `on_control` 直接写 control note，不校验 from 连接权限  
**影响**: 任意连接可伪造 control，篡改 task 状态  

### 4. Session apply 窗口竞态（Critical）
**文件**: `crates/onlyne-session/src/reconcile/bridge.rs` - `apply_at_next`  
**问题**: 读 version → apply → 写 store 之间窗口，竞争写入变 StaleSeq 但 `landed=true`  
**影响**: adoption 失败但无重试，服务端不知情  

### 5. Client hello_live_tasks DB 错误静默（Major）
**文件**: `crates/onlyne-client/src/session/dispatch/slots.rs` - `hello_live_tasks()`  
**问题**: `active_session_tasks()` 失败被 `if let Ok` 吞掉，返回空 claim  
**影响**: sqlite busy 时触发，服务端 requeue 所有 in_flight 行，重复投递  

### 6. Client parked 单槽覆盖（Major）
**文件**: `crates/onlyne-client/src/session/dispatch/transport.rs` - `park_adapter`  
**问题**: `parked: Option<(io, caps)>` 单槽，第二个 mount 覆盖第一个  
**影响**: 第一个 plugin 永久丢失，无 bye 通知  

### 7. Session mismatch counter 永不重置（Major）
**文件**: `crates/onlyne-session/src/reconcile/bridge.rs` - `reconcile`  
**问题**: mismatch_count 只增不减，一次 mismatch 后永久累积  
**影响**: 误触 isolate/terminate 阈值  

### 8. Client dispatch 失败泄漏（Major）
**文件**: `crates/onlyne-client/src/session/dispatch/delivery.rs` - `dispatch`  
**问题**: slot 分配、backend spawn、store 写三步，中间失败 slot 不释放  
**影响**: 资源泄漏，max_sessions 达上限后无法接新任务  

### 9. Proto frame decoder 无 buffer 上限（Major）
**文件**: `crates/onlyne-frame/src/lib.rs` - decoder  
**问题**: partial frame 累积在内存 buffer，无上限  
**影响**: 慢速 sender 可撑爆内存  

### 10. Plugin pendingCompletions flush 中断（Major）
**文件**: `plugins/onlyne-agent-pi/src/agent.mjs` - `flushPendingCompletions`  
**问题**: drain 循环中第一个成功、第二个失败时，第一个的 exit 已执行但第二个 unshift 回去  
**影响**: 进程已退出但 completion 未全部送达  

---

## 建议

1. **Critical 问题**应立即修复（数据正确性 + 安全）
2. **Major 问题**排期修复（资源泄漏会累积）
3. **Minor 问题**技术债清理

Server 侧审查因超时未完成，建议单独重跑 `relay.rs`、`router.rs`、`projection.rs` 的审查。

---

## 附录：审查工具输出

完整结构化输出：
- `history://AuditClient` - 客户端审查记录
- `history://AuditSession` - 会话层审查记录
- `history://AuditPlugin` - 插件审查记录
- `history://AuditProto` - 协议层审查记录
- `history://AuditServer` - 服务端审查记录（失败）

每个 history 包含完整的文件列表、问题描述、根因分析、影响评估。
