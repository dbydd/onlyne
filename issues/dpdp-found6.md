# dpdp 第六轮：Feishu IM 资源节点全功能路径测试

2026-10-27。测试节点 `daily-workbench/dpdp`，资源 `urn:onlyne:gateway:im` / `lark-oc_example`，平台 Lark，会话 `oc_example`。测试账号为用户提供的 Feishu 测试环境，界面侧通过 Computer Use 操作飞书客户端，资源侧通过 `xd://msg` 调用。未读源码、数据库、密钥或平台凭证。

资源公布了八个操作：`read`、`send`、`list_events`、`describe`、`handover`、`update_card`、`recall`、`react`。本轮全部调用过。文本、卡片、更新卡片、表情、撤回、事件读取、引用回复在飞书界面逐项可见。文件与图片只能单向进入事件表，且只带平台句柄。跨节点 `handover` 三次均返回 `aborted / unreachable`，绑定未改变。

## 环境与操作面

| 项 | 值 |
|---|---|
| 节点 endpoint | `http://127.0.0.1:62923/onlyne/v3` |
| 网关 endpoint | `http://127.0.0.1:3991/onlyne/v3`，`handshake: exchanged` |
| 会话绑定 | `handler: urn:onlyne:agent:dpdp`，`cursor: 0`，`revision` 由 `rev-1` 变为 `ae9989d2-6983-48ce-80d2-08bef303d72a` |
| 事件保留 | `retention_ms: 3600000` |
| 日志读取方 | `urn:onlyne:gateway:im` |
| 界面 | 飞书窗口 id 49496，测试会话「智能助手」 |

界面侧使用 `ui-control` 技能：焦点守卫逐次捕获与恢复，用户的 Tern 窗口始终在前。`computer.control.acquire` 被拒绝，全程使用后台窗口输入与 AX，未接管键鼠。截图是真实客户端的窗口画面，含个人会话名与侧栏条目，因此不随仓库发布，由运行者本地保存。

操作矩阵里的拒绝文案与返回字段来自本轮调用返回，未另存原始返回体。本轮笔记与截图都不含这些返回体，因此矩阵内容无法由第三方独立核验。截图只证明界面呈现，不证明事件层字段；事件层结论全部来自 `list_events`、`read`、`describe` 的返回。

## 操作矩阵

| 操作 | 调用形状 | 结果 | 界面证据 |
|---|---|---|---|
| `describe` | 仅 `resource` | committed，返回八操作、绑定、revision、`event_seq` | — |
| `read` | 缺 `event_id` | 拒绝：`read names the event_id to return.` | — |
| `read` | `data.event_id: "1"` | committed，正文 `111`，含 `platform`/`conversation`/`from`/`attachments` | — |
| `list_events` | 缺游标 | committed，返回 `event_id` 1，并给 `next_cursor` | — |
| `list_events` | 自造 `cursor: "1"` | 拒绝：`Cursor belongs to another query.` | — |
| `list_events` | 使用返回的游标 | committed，逐页前进到 `next_cursor: null` | — |
| `send` | `data.text` 中英文混合 | committed，返回 `message_id` | 文本原样显示 |
| `send` | `data.kind: "interactive"` + 卡片 JSON | committed，返回 `message_id` | 卡片标题与正文渲染 |
| `update_card` | `data.message_id` + 新卡片 JSON | committed，`data` 为空 | CARD-V2 可见（V1 仅有调用结果） |
| `react` | `data.message_id` + `emoji: "THUMBSUP"` | committed，`data` 为空 | 消息下方出现点赞 |
| `recall` | `data.message_id`，两次 | committed，`data` 为空 | 每次留下一条撤回占位 |
| `send` | `data.kind: "file"` + `file_key` | 拒绝：`The send data carries a text or a card.` | — |
| `handover` | 缺 `target` | 拒绝：`A handover names its target.` | — |
| `handover` | 缺 `handover_deadline` | 拒绝：`A handover carries a handover_deadline.` | — |
| `handover` | `target: caerulea` + 截止时间 | `state: "aborted"`，`data.reason: "unreachable"` | — |
| `handover` | `target: lazuli` | 同上 `aborted / unreachable` | — |
| `handover` | `target: dpdp`（自身） | committed，绑定与 revision 更新 | — |

游标看起来与会话或查询绑定，`"1"` 不是 `event_id` 的别名。具体绑定到哪个维度（绑定、revision、还是查询本身）本轮没有查清。

## 入站路径

飞书界面发出的消息会进入资源事件表，字段包含 `platform`、`conversation`、`kind`、`from`、`text`、`attachments`、`at`。

| 场景 | 事件 | 观察 |
|---|---|---|
| 纯文本 | 2 | 正文完整，`attachments: []` |
| 多行文本 | 5 | `text` 保留 `\n`，界面与事件一致 |
| 界面引用回复 | 6 | 额外出现 `reply.parent` 与 `reply.root`，指向被引用消息的 `source_id` |
| 本地文件 | 3 | `text` 是 `<file key=… name=…/>` 占位；`attachments` 只有 `kind: "file"`、`platform_handle`、`alt` |
| 本地图片 | 4 | `text` 是 `![image](img_v3_…)`；`attachments` 只有 `kind: "image"`、`platform_handle` |

事件里没有本地路径、内容地址或可下载引用。本轮在 Onlyne 面没有找到取内容的路径，平台 API 绕行不计为通过。

事件 3 与事件 4 的字段来自 `list_events` 条目。`read` 只对事件 1 执行过，未对这两个附件事件单独读取。

第一轮测试曾把一条纯文本的新行当成丢失，实际是输入方式把换行折叠了。改用 `shift+enter` 后换行完整保留，事件与界面一致。

## 资源侧发送与关联

`send` 只接受文本与卡片。传入 `reply_to` 或 `reply_in_thread` 时调用仍 committed，返回 `om_example_1`（`ONLYNE-IM-REPLY-02`）与 `om_example_2`（`ONLYNE-IM-REPLY-MODE-03`）。两条在界面都是普通消息，没有引用装饰；事件表里也没有 `reply` 字段，而界面发出的引用回复事件 6 明确带 `reply.parent` 与 `reply.root`。资源侧发送目前不表达引用关系。

子会话草稿路径可用，但要求消息已分配给该子会话的上下文。`onlyne-c-1` 读到 `111` 并起草回复 `c0901da8-7b81-4bb4-beb3-a4dfeed0dc88`，主会话以 `{draft_id}` 发送后落成 `c161fb6d-edab-444c-8562-7b38636d2d33`，线程 `im-lark-…` seq 2，`in_reply_to` 指向原通知，回执 `guarantee` 为 `binding-defined`。界面显示为对 `111` 的引用回复。

起草时若目标节点没有可读的执行上下文目录，会报 `The target's published contexts cannot be read: The host provides no execution context directory`，需要显式补 `meta.context`。

两条前提要分开看。第五轮已确认文档写明了「只能为分配给本上下文的源消息起草」；本轮新发现的是「当目标没有可读上下文目录时，必须显式给 `meta.context`」，后者未写进 `xd://msg` 的草稿示例。两次起草失败分别对应这两条，报错文案不同。

## 动态移交

三次跨节点 `handover` 全部 `aborted / unreachable`，目标分别是 caerulea（重启前后各一次）与 lazuli。每次调用前目标节点都在邻居表里 `verified` 且 `handshake: exchanged`，普通消息也能送达并收到回复。caerulea 在自己一侧查了 inbox 与 threads，没有任何资源或移交记录。

同一操作对自身 `target: dpdp` 成功提交，绑定与 revision 更新，说明操作本身可用，失败发生在跨节点这一段。本轮没有为此调整拓扑或配对，只记录现象。

同一张邻居表里 `urn:onlyne:gateway:mcp` 是 `no_exchange`，本轮没有把它作为移交目标，因此 `unreachable` 与 `no_exchange` 是否同义仍未区分。自移交只用于证明参数与状态机可走通，其业务语义未验证。

## 界面侧限制

- 飞书客户端主窗口的聊天区是自绘表面，AX 只暴露输入框与按钮，消息气泡必须靠截图判断。读取消息内容依赖资源事件表，这一点反而更可靠。
- 一次自动化输入被飞书弹出的推广浮层挡住，浮层没有可用的 AX 关闭控件，需要人工关闭。
- 界面侧未观察到第二个会话产生资源事件。多会话隔离需要再建一个绑定到 IM 网关的会话，属于运维侧操作，本轮未做。

## 结论

八类路径全部调用过：文本发送、卡片发送、更新卡片、表情回应、撤回、事件读取、事件分页、界面引用回复。文本、卡片、更新卡片、表情、撤回及界面引用回复在客户端可见；事件读取与分页由资源调用结果确认。出站操作（`send`、`update_card`、`react`、`recall`）是否同时回写事件表，本轮没有单独核对。

文件与图片能进入事件层；本轮 `list_events` 返回平台句柄，未取得内容或可下载引用。附件事件未单独执行 `read`，内容读取能力尚未验证。本轮文件出站尝试被拒，拒绝文案称仅接受文本或卡片；图片出站未测。资源侧 `send` 不能表达引用。跨节点 `handover` 在本环境不可用。

已修一处此前的错误结论：网关消息确实会派生上下文并起回合，只是耗时约 5 分钟，先前读到的是中间态。

收尾复核：`describe` 返回 `handler: urn:onlyne:agent:dpdp`、`cursor: "0"`、`revision: ae9989d2-6983-48ce-80d2-08bef303d72a`、`event_seq: "6"`。两次 `recall` 各在界面留下一条「撤回了一条消息」占位，共两条，与两次 committed 结果一致。

## 未验证与边界

- 图片与文件的出站发送：资源明确只接受文本与卡片，图片出站未测。
- 多会话隔离：缺少第二个绑定到网关的会话。
- 跨节点 `handover` 的成功路径：三个目标全部 `unreachable`，未查明原因。
- 资源消息的自动回复策略：本轮入站均未触发自动发话，是否需要回复由任务决定。
- 事件保留期 1 小时后过期后的读取行为未测。
