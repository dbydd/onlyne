# @onlyne/lib

Onlyne v3 的库：thread、持久接纳的消息、关注义务、回复关联、带显式交接的资源，以及 Fetch 形式的 HTTP 绑定。

协议草案见 [`docs/rfcs`](../../docs/rfcs)。RFC 层形式化在 [`proof/proof_rfc`](../../proof/proof_rfc)，实现层形式化在 `proof/proof_lib`。

## 运行

包导出 TypeScript 源码，由 Bun 等支持 TypeScript 的宿主加载。

```sh
bun install
bun run check
bun run test
```

`bun run test` 通过公开接口运行验收脚本，并启动回环 HTTP 节点。在仓库根目录执行 `bun run check` 与 `bun run test` 会转发到本包。

## 库入口

宿主提供节点身份、服务地址和插件。库提供 Fetch handler，宿主负责监听端口和关闭服务。

```ts
import { OnlyneNode, type NodeOptions } from '@onlyne/lib';

export function serveOnlyne(options: NodeOptions, port: number) {
  const node = new OnlyneNode(options);
  const server = Bun.serve({
    hostname: '127.0.0.1',
    port,
    fetch: node.fetch,
  });
  return { node, server };
}
```

`options.base_url` 包含 `/onlyne/v3`。`FetchTransport` 允许回环 HTTP，远端地址必须使用 HTTPS。解析插件提供远端身份、地址和认证请求头，认证插件把入站请求映射为已验证的主体。

模型调用 `node.client.msg({ delivery_kind, msg, meta })`。`decide` 插件返回一个带类型的发送、读取、thread、资源或域动作。判定不可用时返回 `decision_unavailable`。宿主也可以直接调用 `threads`、`messages`、`resources`、`domains` 和 `client.send` 接口。

计划的 `response` 为 `await` 时，`client.send` 会等到回复到达或等待结束。结果用 `wait` 表示 `answered`、`timed_out` 或 `cancelled`；有答复时还带 `reply` 与 `reply_body`。await 工具结果与自动关注片段共用同一条 `(context, ThreadRef, seq)` 消费记录。该上下文已确认过的正文以 `reply_body: 'withheld'` 返回，只给到达事实与原 MessageRef；本次工具结果展示过的正文会关闭该记录，自动片段不再重复展示。`runtime.confirmToolResult` 供宿主报告它已在该上下文中观察到工具结果。没有任何路径确认的记录保持未确认，会被重放。

## 域

域是一组节点的命名划分。它没有 owner、中心成员表，也不隐含 thread 或资源权限。一个节点同一时间只属于一个域，加入另一个域前必须先退出当前域。thread 可以跨域；资源访问仍走资源 owner 或已有的 ingress。每个节点只记录自己的成员关系：

```ts
const created = await node.domains.create(node.auth, mutation);
await node.domains.join(node.auth, "urn:onlyne:domain:research", mutation);
const members = await node.domains.discover(node.auth, "urn:onlyne:domain:research");
```

`create`、`join`、`leave` 只修改本节点记录，调用主体必须是本节点。已有 active 域时再创建或加入另一个域返回 `conflict`。`describe` 返回本节点当前记录，包含已退出的墓碑；`list` 至多返回一条 active 关系。`discover` 从宿主的 `domainCandidates(domain_id)` 取得候选项，逐个查询候选的 `/domains/{domain_id}`，只返回当前声明 active 的节点。查询失败的节点进入 `unavailable`，缓存或对端目录不被当作成员依据。

使用同一存储重建节点后成员关系仍在。带当前 revision 的重复 `join` 幂等；过期 `expected_revision` 被拒。重投旧幂等键返回原结果，不会复活之后的退出。HTTP 路径为 `POST /domains`、`GET /domains`、`GET /domains/{domain_id}`、`POST /domains/{domain_id}/join`、`POST /domains/{domain_id}/leave`、`GET /domains/{domain_id}/members`。已认证的服务描述用 `domain` 返回唯一的 active 标识；未认证响应既不返回 `domain` 也不返回 `domains`。

## 资源

操作结果为 `committed`、`pending`、`aborted` 或 `unknown`。`aborted` 表示 endpoint 明确拒绝或本地中止，且已确证没有产生外部效果；无法排除已产生副作用的失败记为 `unknown`，在 `details` 中保留原始错误，并由 `node.resources.recover()` 按 `operation_id` 向插件查询结局。调用方停止等待时只结束本地等待，外部调用继续运行，之后插件给出的结局可以修正该记录。重投的操作只查询结局，不会再次执行。

事件同时带投递确认与保留期。接收者确认与保留期过期各自把同一投递义务从待清空积压中幂等移除。过期结束本地投递责任，内容仍按内容保留策略处理。过期之后到达的 `ack` 保留接收证据，不复活义务、不重复移除，也不恢复本地投递责任。交接提交检查的正是这个积压。

资源交接先返回 `pending`，指定目标通过 `decideHandover` 接受或拒绝。提交检查当前 revision、权限、`decision_deadline` 和积压为空；到期未提交的准备记为 `aborted`，之后不得凭已记录的接受补交。平台 SDK、平台凭据和产物读取由资源插件负责。

## 插件边界

库不执行文件系统操作。持久化、配置加载、凭据、附件读取、运行时接入和平台操作由宿主插件负责。消息 parts 和资源 data 只携带 JSON 引用，库不打开引用里的路径。

| 插件 | 宿主负责的行为 |
|---|---|
| `storage` | 提供原子、可串行化的事务并声明接纳保证；事务回调失败时回滚全部写入。 |
| `authenticate` | 验证请求凭据，返回主体及明确授权的转发身份。 |
| `resolve` | 把节点身份解析为服务地址和认证请求头。 |
| `decide` | 把消息意图解释为一个可执行动作。 |
| `runtime` | 检查投递能力、注入宿主上下文、按需建立运行时待办，并报告宿主已观察到的 await 工具结果。 |
| `resources` | 校验资源参数、执行平台操作、查询未知结果。 |
| `mayAdmit`、`mayHandover`、`mention`、`directory` | 提供接纳策略、交接策略、thread 别名和宿主目录。 |
| `domainCandidates` | 提供可能声明某个域的节点身份。缺少它时拒绝发现，不猜成员。 |
| `transport` | 按需用其他网络绑定替换内置 Fetch transport。 |

`MemoryStorage` 只保存在当前进程中，回执保证为 `binding-defined`。持久化插件在事务可靠提交消息、回执和义务后返回 `durable`。库按插件声明的保证生成回执。

宿主调用 `node.client.maintain()` 重投授权通知、对账关注义务并注入队列中的条目。宿主调用 `node.client.recover()` 恢复出站请求，调用 `node.resources.recover()` 推进资源恢复。调用时机由宿主决定，重建节点时继续使用原存储插件。运行时报告消息已被消费后，库才记录注入完成。

库不保存任何任务状态。把一条消息变成运行时待办是宿主的事：待办快照与消息引用的关联写入同一个可恢复提交，宿主投影由该提交派生，恢复时先重放该提交再决定是否新建条目。消费记录完成后才调用 `runtime.todo`，待办内容和提醒由宿主负责。
