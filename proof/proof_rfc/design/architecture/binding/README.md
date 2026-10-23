# 绑定

本模块负责资源的当前 assignee、版本围栏，以及每条已接收事件的首次归属。它不负责平台发送，也不负责 thread。

## 依据

资源 endpoint 保存自己的绑定表，agent 缓存只作展示（来源：`docs/rfcs/0006-resources-and-handover.md:136-151`）。交接提交是生效点，提交前的事件属于原 assignee，提交后的事件属于新 assignee（来源：`docs/rfcs/0006-resources-and-handover.md:181-183`）。积压重放沿用首次记录的归属（来源：`docs/rfcs/0012-im-resource-node.md:174`）。发送同时校验当前 assignee、调用者和版本（来源：`docs/rfcs/0006-resources-and-handover.md:195-199`）。

## 为什么

绑定、版本与事件归属放在同一模块，因为它们共同回答「谁有权处理这条事件」。把首次归属与当前绑定分开存放，会让推送路径和读取路径各自选择一侧。

## 语义

``Binding`Receive`` 与随后可能发生的交接提交之间的顺序由 endpoint 排列，模型不引入时钟。``Binding`Recipient`` 的调用方必须已经持有一条已接收事件的引用；模型不保证每条事件都存在获准读取它的节点。

节点可以崩溃并恢复。恢复时，尚未终结的准备由 endpoint 自己接管，不依赖新的请求。

保留期到达时，endpoint 必须调用 ``Binding`Expire``；保留期的推进由外部环境承担。``Binding`Ack`` 的身份参数来自认证连接。确认与过期都清除投递义务，两种证据可以并存；现行 RFC 没有规定展示时的终态标签优先级。
