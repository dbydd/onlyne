# Onlyne v3

Onlyne v3 是面向 agent 消息与资源协作的去中心化 TypeScript 系统。agent 通过 `msg({ delivery_kind, msg, meta })` 表达投递意图。各参与者保存自己经手的记录。系统没有集中式 Onlyne server、全局账本或任务状态投影。

仓库包含协议草案、运行时包，以及两份形式化验证记录。

## 包

| 包 | 内容 |
|---|---|
| [`packages/lib`](packages/lib/README.zh-CN.md) | `@onlyne/lib`：thread、持久接纳的消息、关注义务、回复关联、无 owner 的域、带显式交接的资源、HTTP 绑定与插件接口 |
| [`packages/node-host`](packages/node-host/README.md) | `@onlyne/node-host`：身份密钥、DNS-SD 发现、网关信任终端与消息目录 |
| [`packages/im-core`](packages/im-core/README.md) | `@onlyne/im-core`：IM 连接器契约与驱动桥 |
| [`packages/im-fake`](packages/im-fake/README.md) | `@onlyne/im-fake`：供测试使用的脚本化连接器 |
| [`packages/im-lark`](packages/im-lark/README.md) | `@onlyne/im-lark`：飞书连接器 |
| [`packages/im-qq`](packages/im-qq/README.md) | `@onlyne/im-qq`：QQ 连接器 |
| [`packages/im-wechat`](packages/im-wechat/README.md) | `@onlyne/im-wechat`：微信连接器 |
| [`packages/im-gateway`](packages/im-gateway/README.md) | `@onlyne/im-gateway`：每个 IM 通道独立运行节点，托管会话资源并提供信任终端 |
| [`packages/mcp-core`](packages/mcp-core/README.md) | `@onlyne/mcp-core`：MCP 会话与资源驱动 |
| [`packages/mcp-gateway`](packages/mcp-gateway/README.md) | `@onlyne/mcp-gateway`：MCP 资源、持久定时任务与 controller 结果推送 |
| [`packages/omp-onlyne`](packages/omp-onlyne/README.md) | OMP 接入：单一 `msg` 工具、judge 选择投递、入站注入 |

`packages/` 只放可发布的运行时代码。后续包与 `lib` 并列。

每个包发布 TypeScript 源码，运行时要求写在 `engines.bun`。带作用域的包使用 `access: public` 发布。

## 发布

包之间有依赖，按依赖顺序发布：

```sh
for pkg in lib node-host im-core im-fake im-lark im-qq im-wechat im-gateway mcp-core mcp-gateway omp-onlyne; do
  npm publish "packages/$pkg"
done
```

`npm pack --dry-run packages/<name>` 列出单个包实际发布的文件。各清单已写 `files`、`repository.directory` 与许可证，压缩包只含 `src`、README 与 LICENSE。

## 形式化验证

| 目录 | 建模范畴 |
|---|---|
| [`proof/proof_rfc`](proof/proof_rfc/SSOT.md) | RFC 层形式化：接纳、关注与绑定，对草案规则做有界状态搜索 |
| `proof/proof_lib` | 实现层形式化：库实际运行的语义 |

`proof_rfc` 检查草案，`proof_lib` 检查交付的实现。两份记录各自写明边界；有界搜索能在一个边界内否证性质，不能证明性质。

## RFC 集

| RFC | 内容 |
|---|---|
| [0000 总览](docs/rfcs/0000-overview.md) | 目标、对象归属、不变量 |
| [0001 公共契约](docs/rfcs/0001-core.md) | 身份、引用、HTTP、错误、重投 |
| [0002 Thread](docs/rfcs/0002-threads.md) | Thread 生命周期、成员与访问控制 |
| [0003 消息](docs/rfcs/0003-messages.md) | 发送、持久接纳、回执与回复 |
| [0004 视图](docs/rfcs/0004-events-and-views.md) | 消息列表、索引、事件续传 |
| [0005 投递](docs/rfcs/0005-delivery-and-runtime.md) | SystemOne、库等待、运行时 todo |
| [0006 资源](docs/rfcs/0006-resources-and-handover.md) | 资源、路由绑定与动态交接 |
| [0007 绑定](docs/rfcs/0007-bindings.md) | A2A、MCP、HTTP 与 OMP 映射 |
| [0008 OMP 界面](docs/rfcs/0008-omp-surface.md) | 模型工具、消息视图、上下文注入与运行时 todo |
| [0009 A2A 参考](docs/references/a2a.md) | 上游协议、网络操作与映射边界 |
| [0010 ANP 参考](docs/references/anp.md) | 上游身份、发现与网络协议 |
| [0011 发现与域](docs/rfcs/0011-discovery-and-domains.md) | 节点发现、无 owner 的域与服务描述 |
| [0012 IM 资源节点](docs/rfcs/0012-im-resource-node.md) | IM 会话：事件、绑定、交接与恢复 |
| [0013 计算作业节点](docs/rfcs/0013-compute-job-resource.md) | 计算作业：三项授权分离、产物与恢复 |
| [0014 数据源节点](docs/rfcs/0014-data-source-resource.md) | 数据源：分级授权、订阅与恢复 |
| [0015 MCP 资源节点](docs/rfcs/0015-mcp-resource-node.md) | MCP server 作为无状态资源节点接入 |

- [v1/v2 实现与历史文档](archive/README.zh-CN.md)
- [English](README.md)

RFC 仍是草案。库实现共享的消息、HTTP、运行时和资源契约。平台适配以及 OMP、A2A、ANP、MCP 接入需要宿主插件。
