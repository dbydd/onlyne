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

模型调用 `node.client.msg({ delivery_kind, msg, meta })`。`decide` 插件返回一个带类型的发送、读取、thread 或资源动作。判定不可用时返回 `decision_unavailable`。宿主也可以直接调用 `threads`、`messages`、`resources` 和 `client.send` 接口。

## 插件边界

库不执行文件系统操作。持久化、配置加载、凭据、附件读取、运行时接入和平台操作由宿主插件负责。消息 parts 和资源 data 只携带 JSON 引用，库不打开引用里的路径。

| 插件 | 宿主负责的行为 |
|---|---|
| `storage` | 提供原子、可串行化的事务并声明接纳保证；事务回调失败时回滚全部写入。 |
| `authenticate` | 验证请求凭据，返回主体及明确授权的转发身份。 |
| `resolve` | 把节点身份解析为服务地址和认证请求头。 |
| `decide` | 把消息意图解释为一个可执行动作。 |
| `runtime` | 检查投递能力、注入宿主上下文，并按需建立运行时待办。 |
| `resources` | 校验资源参数、执行平台操作、查询未知结果。 |
| `mayAdmit`、`mayHandover`、`mention`、`directory` | 提供接纳策略、交接策略、thread 别名和宿主目录。 |
| `transport` | 按需用其他网络绑定替换内置 Fetch transport。 |

`MemoryStorage` 只保存在当前进程中，回执保证为 `binding-defined`。持久化插件在事务可靠提交消息、回执和义务后返回 `durable`。库按插件声明的保证生成回执。

宿主调用 `node.client.maintain()` 重投授权通知、对账关注义务并注入队列中的条目。宿主调用 `node.client.recover()` 恢复出站请求，调用 `node.resources.recover()` 推进资源恢复。调用时机由宿主决定，重建节点时继续使用原存储插件。运行时报告消息已被消费后，库才记录注入完成。

资源交接先返回 `pending`，指定目标通过 `decideHandover` 接受或拒绝。提交会检查当前 revision、权限、期限和未确认投递。平台 SDK、平台凭据和产物读取由资源插件负责。
