# RFC 0014：数据源资源节点

状态：Draft。依赖：RFC 0001、0006、0011、0015。

## 1. 定位

数据源是数据驱动型资源节点。它按需提供读取，可选地支持订阅变更。数据源不维护处理者关系，也没有发送权。

| 场景 | 形态 |
|---|---|
| 知识库、文档集 | 只读，按查询返回 |
| 文件树、对象存储 | 只读，按路径返回 |
| 数据库视图 | 只读，按查询返回 |
| 可订阅指标 | 读取加变更订阅 |

数据源与 MCP 资源的差别：MCP 是无状态工具调用，数据源持有可寻址的数据集合，读取需要数据级授权。两者都无 assignee，都靠 RFC 0006 的通用操作形状。

## 2. 资源模型

数据源是一个资源，数据项是资源内的可寻址对象。

```json
{
  "resource_ref": {"owner": "urn:onlyne:data:lab", "id": "seaice-archive"},
  "kind": "dataset",
  "operations": ["query", "read", "describe", "list_events"],
  "requires": ["dataset_read"],
  "subscription": {"supported": true}
}
```

数据项引用：

```json
{
  "item_ref": {"resource": {"owner": "urn:onlyne:data:lab", "id": "seaice-archive"}, "id": "obs-2026-10-01"}
}
```

数据项 id 在资源内唯一，不复用。删除后该 id 不分配给其他数据项。

数据源的 `operations` 不含 `send` 和 `handover`。它没有处理者可以切换，也没有对外发送能力。需要推送场景用 RFC 0013 的作业或 RFC 0012 的 IM 资源。

## 3. 查询与读取

查询：

```json
{
  "operation_id": "op-01",
  "retry_until": "2026-10-23T04:00:00Z",
  "operation": "query",
  "data": {"filter": {"region": "arctic"}, "page_size": 50}
}
```

- `operation` 取自 `operations` 清单。
- `data` 匹配查询 schema。不匹配返回 `invalid_request`。
- 查询结果分页，`next_cursor` 为 null 表示没有下一页。
- 查询不改变数据，可用相同参数重放。

读取：

```json
{
  "operation_id": "op-02",
  "retry_until": "2026-10-23T04:00:00Z",
  "operation": "read",
  "data": {"item_ref": {"resource": {"owner": "urn:onlyne:data:lab", "id": "seaice-archive"}, "id": "obs-2026-10-01"}}
}
```

读取返回数据内容或内容引用：

```json
{
  "state": "committed",
  "data": {
    "item_ref": {"resource": {"owner": "urn:onlyne:data:lab", "id": "seaice-archive"}, "id": "obs-2026-10-01"},
    "mime_type": "application/parquet",
    "size": 1048576,
    "uri": "https://lab.example/data/seaice-archive/obs-2026-10-01"
  }
}
```

URI 是数据位置，不授予访问权。读取授权在每次操作时检查。大对象经 URI 直接取，不穿过 Onlyne 的消息路径。

## 4. 授权

数据源的授权按数据范围授予，不是全有或全无：

```json
{
  "reader": "urn:onlyne:agent:lazuli",
  "scope": {
    "collections": ["arctic"],
    "since": "2026-01-01T00:00:00Z",
    "fields": ["temperature", "extent"]
  },
  "revision": "rev-a"
}
```

- 查询和读取都按当前授权过滤。超出范围的数据项返回 `not_found`，不暴露其存在。
- 授权范围可以是集合、时间窗和字段三个维度。
- 授权由数据源 owner 变更，变更产生新 revision。
- agent 缓存已见授权仅作展示，实际操作时数据源重读自己的表。

数据源不设 `assignee`，因此没有交接流程。授权变更是 owner 的单方动作，不需要目标接受。

## 5. 订阅

支持订阅的数据源可以注册变更通知：

```json
{
  "operation_id": "op-03",
  "retry_until": "2026-10-23T04:00:00Z",
  "operation": "subscribe",
  "data": {"scope": {"collections": ["arctic"]}}
}
```

- 订阅按授权范围过滤。超出已有授权的范围返回 `forbidden`，不扩大权限。
- 订阅是本地注册，不代表数据源把数据推给任意节点。
- 取消订阅用同一 `operation` 的逆操作，幂等。

变更通知只报引用：

```text
id: 903
event: data_event
data: {"resource_ref":{"owner":"urn:onlyne:data:lab","id":"seaice-archive"},"event_id":"88","item_ref":{"resource":{"owner":"urn:onlyne:data:lab","id":"seaice-archive"},"id":"obs-2026-10-02"},"change":"added"}
```

`change` 取 `added`、`updated`、`removed`。通知不带内容，读取用 `read` 操作。

## 6. 恢复

### 数据源重启

重启后恢复授权表、数据目录和事件游标。数据项 id 与内容的映射必须持久。重启不得把已删除的数据项 id 分配给新内容。

### 订阅恢复

订阅不保证重放。断开期间的实时变更通知会丢失，重连后不补发实时通知。

- 重连后 Onlyne 重新订阅，断开期间标记为未知。
- 恢复手段是按已见 `event_id` 之后的范围重新查询，比较本地记录。
- 无法确认断开期间的变化时如实报告 `unknown`。

数据源声明自己的事件保留窗口。窗口内可以按游标查询到断开期间的事件，这不等于实时通知会补发；窗口外的事件明确报告缺失。窗口为 0 表示只能实时通知，不提供历史查询。

### 读取恢复

读取是幂等的，可重复执行。URI 过期或数据被删时返回 `not_found`，不把同一 URI 分给其他数据项。

### 未确认投递

数据源与集群链路之间的未确认通知在新连接建立时重新入队。重放产生重复通知，下游按 `event_id` 去重。

## 7. 与 MCP 资源的区别

| 维度 | MCP 资源（0015） | 数据源（0014） |
|---|---|---|
| 状态 | 无状态工具调用 | 持有可寻址数据集合 |
| 授权 | server 级加工具名 | 按集合、时间窗、字段分级 |
| 订阅 | `resources/subscribe` 可选 | 内建，按授权范围过滤 |
| 发现 | `tools/list` | `query` 加数据目录 |
| 恢复 | 无游标，靠重读 | 有 `event_id` 游标和保留窗口 |

数据源可以同时以 MCP server 形态暴露。此时 MCP 是传输和工具形状，数据级授权仍由 Onlyne 的资源层持有，不依赖 MCP 的能力声明。

## 8. 验收

- 查询和读取都按当前授权过滤，范围外数据项返回 `not_found`。
- 订阅范围超出已有授权时返回 `forbidden`，不扩大权限。
- 数据项 id 不复用，删除后不分配给新内容。
- 大对象经 URI 直接取，不穿过 Onlyne 消息路径。
- URI 过期返回 `not_found`，不把同一 URI 分给其他数据项。
- 数据源重启后授权表和数据目录从持久记录恢复。
- 订阅断开后重新订阅，断开期间的变化标记未知，窗口内可按游标补齐。
- 重复通知按 `event_id` 去重。
- 授权变更产生新 revision，agent 缓存不替代数据源重读。
