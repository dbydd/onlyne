# SqliteStorage 串行事务队列单链死锁导致 GET /description 超时与终端卡死

日期：2026-11-01
报告人：urn:onlyne:agent:lazuli（research-chamber）
相关组件：`omp-onlyne`（`SqliteStorage`）、`@onlyne/lib`（`http.ts`）、`Tern`（TSP 协议）

---

## 现象描述

在节点持续运行与跨节点交互过程中，`lazuli`（端口 57804）的 HTTP 端点出现严重级联故障：

1. **路由分化挂死**：
   - 纯内存静态接口（如 `GET /onlyne/v3/keys`）响应极快（HTTP 200，< 2ms）；
   - 凡涉及 `SqliteStorage` 存储访问的接口（尤其是 `GET /onlyne/v3/description`）被永久卡死，直到触碰 `Bun.serve()` 的 10 秒空闲超时限制被强行切断（`curl: (52) Empty reply from server`）。
2. **对端级联互卷死循环**：
   - 对端节点 `remd-uv-divergence`（端口 56583）因持有 13 条属主为 `lazuli` 的 `known` 线程，每 24 秒执行一次周期性 reconcile；
   - 每次 reconcile 解析 `lazuli` 的 endpoint 时，都会调用 `discovery.resolve()` 发起 `GET http://127.0.0.1:57804/onlyne/v3/description`；
   - 导致对端每 24 秒便产生 13 次并发请求挂死 10 秒后报 `code: unavailable, message: The target could not be reached.`；
3. **反噬 Tern 协议帧，会话全盘卡滞**：
   - 密集的挂死套接字、连接堆积以及日志刷屏反噬了宿主环境 Tern 的终端协议（TSP），导致宿主日志中频繁出现 `TSP: terminal stopped acknowledging frames; resuming without credits`，终端停帧确认长达 165 秒（`stalledMs: 165421`），整个交互界面严重卡死。

---

## 根因分析

### 1. `SqliteStorage` 的 `#tail` 单 Promise 链排队模型缺陷
在 `packages/omp-onlyne/src/store.ts` 中：
```typescript
export class SqliteStorage implements Storage {
  readonly guarantee = "durable" as const;
  readonly #db: Database;
  #tail: Promise<unknown> = Promise.resolve();
  ...
  transaction<T>(work: (tx: Transaction) => Promise<T>): Promise<T> {
    const run = () => this.#execute(work);
    const result = this.#tail.then(run, run);
    this.#tail = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }
}
```
所有传入 `storage.transaction` 的操作均被单链挂在 `#tail` 后面串行执行。一旦有任何一个 `work` 闭包因内部等待、隐式未决 Promise、外部网络 I/O 或循环嵌套导致未完成（或卡住），后续所有加入队列的事务操作将被永久阻塞。

### 2. `GET /description` 强耦合 `storage.transaction`
在 `packages/lib/src/http.ts` 与 `domains.ts` 中：
```typescript
// http.ts
if (path.length === 1 && path[0] === 'description') {
  expect(request, 'GET');
  return respond(await description(ctx, domains, discovery !== undefined));
}

// http.ts description()
async function description(ctx: Context, domains: DomainService, discovered: boolean): Promise<Json> {
  const active = (await domains.list({ actor: ctx.node }, { page_size: 100 })).items.find(membership => membership.state === 'active');
  ...
}

// domains.ts list()
async list(auth: Auth, options: PageOptions = {}) {
  this.authenticate(auth);
  return this.#ctx.storage.transaction(async tx => {
    const current = await activeMembership(tx, this.#ctx.node);
    ...
  });
}
```
`/description` 作为基础服务探活与地址发现路由，其核心职责本应是无状态、高可用地返回当前节点的基本元数据。但它强制同步调用了 `domains.list`，进而必须等待进入 `storage.transaction`。当 `#tail` 发生死锁时，甚至连握手探针和自身状态读取都被连带卡死。

---

## 修复建议

1. **解耦基础探针读取（Read Cache / In-Memory State）**：
   - 当前节点的活跃 `domain_id` 与 `membership` 是低频变更配置，启动或加入 domain 时完全可以在内存中缓存当前活跃成员信息，`description()` 查询时优先读取内存缓存，避免在公共健康探活路径上强行进入 SQLite 排队事务；
2. **事务超时熔断（Transaction Timeout）**：
   - `SqliteStorage.transaction()` 对排队和执行应增加超时保护（如 5000ms 超时），超时自动抛出 `storage_timeout` 错误并打断队列，严防单一事务无限挂死整个队列；
3. **禁止事务内嵌套与外部调用**：
   - 审查所有 `storage.transaction(async tx => { ... })` 内部逻辑，严禁在事务作用域内进行外部 HTTP fetch、等待外部事件触发，或直接/间接再调用可能触发 `storage.transaction` 的方法。
