# IM gateway demo — 最小集群 + 三个平台后端

把 v2.3 的聊天寻址协议（`docs/v2.3-CHAT-PROTOCOL.md`）跑在一个**真的**集群上：
一台服务器、一个 agent 角色、一个 IM 网关根，网关根里写着 Telegram、飞书、QQ
三个平台后端，外加一个不需要凭据的 `fake` 后端。

整个搭建只走产品自己的面：`onlyne-server init` 起集群，`onlyne-client init` 种
角色，`onlyne-im init|login` 铺网关根、写凭据。例子不发明任何机制。

## 一句话的架构

```text
IM 会话（Telegram 群 / 飞书群 / QQ 群）
   │  平台事件
   ▼
onlyne-im-backend-<platform>（后端进程，网关按 [[im.backend]] 拉起）
   │  标准协议帧（stdin/stdout）
   ▼
onlyne-im run（网关，集群里是 role `im`）
   │  触发脚本 im-in.sh：投递文本第一行 = 该会话的 chat key
   ▼
_supervisor（本例消息的处理者，没有 agent、没有 client）
   │  complete / send，收件人 `im`
   ▼
网关按第一行令牌查注册表 → 交给对应后端 → 渲染回那个平台会话
```

关键约定：**core 不认识会话**。一个 IM 会话的身份是它工作区里那把 32 位 hex 的
chat key；入站由脚本写在投递第一行，出站由网关的这条 link 提取、查
`conversations/<slug>/`、渲染。

## 这个例子怎么处理消息

例子不为 agent 进程花一分钟：入站投给 `_supervisor`，**它没有 client**，行停在
`queued`（挂起）——这是 §12 的口径，处理者是操作员本人。回复由人手工发：

```text
target/debug/onlyne --server-root /tmp/onlyne-im-demo send --as admin \
  --from _supervisor --to im --text '<chat:<KEY>>

<回复正文>' --force --yes-i-am-supervisor-not-other-role
```

`_supervisor` 不报终态（它没有会话投影可结算），信息交换成功就够——这正是
`_supervisor` 的定位。要看入站，`IM_DEMO_NOTIFY_TO=<pi 会话 id>` 会让触发脚本把
每条消息顺带推给那个 pi 会话（走 pi-intercom），这台机器上的观察窗，不是产品
机制。`run.py smoke` 就是替操作员回一条，同一形状。

## 不需要预先知道 chat id

chat id 只有机器人**已经在**那个会话里之后才读得到，而机器人是为了服务会话才起
的。网关根的 `[im.listen]` 缺省全监听：网关听到一条没人注册过的会话的事件，就**当
场 adopt**——铸一把 chat key、写一份 `conversations/<slug>/.onlyne/` 工作区、日志一行
说明。重启后照旧服务同一个会话（key 在工作区里，不换个新的）。

`[im.listen]` 是可选的收紧：

```toml
[im.listen]
allow = ["-1001234567890"]      # 只服务这些 id；空 = 全收（缺省）
deny  = ["-1009999"]            # 这些一律拒绝，allow 优先让位
```

## 用法

```bash
cargo build --workspace
cp examples/im-gateway/.env.example examples/im-gateway/.env   # 填凭据
python3 examples/im-gateway/run.py up        # 起集群 + 写网关根 + 写凭据
target/debug/onlyne-im run --root /tmp/onlyne-im-demo/gw \
  --socket <up 打印的 socket>                # 前台跑网关
# 到任何平台 @ 你的机器人说一句话 → 它 adopt 会话并回话
python3 examples/im-gateway/run.py status    # roles / 网关 check / 每个会话的 key
python3 examples/im-gateway/run.py stop      # 收摊
```

`smoke` 是不要凭据的一条往返：起带 `ONLYNE_FAKE_BACKEND_EVENTS` 的网关，fake 后端吐
一条入站，走通 网关 → 触发脚本 → `onlyne send` → planner 的 fake agent 完成 →
网关按令牌渲染 → fake 后端收到 send 帧。断言是聊天日志里两个方向各一行。

## 凭据与扫描

| 变量 | 哪家 | 内容 |
| --- | --- | --- |
| `ONLYNE_TELEGRAM_TOKEN` | Telegram | @BotFather 的 token |
| `FEISHU_CREDENTIAL` | 飞书 | `cli_xxx:secret`（或 `FEISHU_APP_ID`+`FEISHU_APP_SECRET`） |
| `QQBOT_CREDENTIAL` | QQ | `appid:secret`（或 `QQBOT_APP_ID`+`QQBOT_APP_SECRET`） |
| `*_CHAT_ID` | 三家 | **可留空**；留空交给 adopt |

## 各家平台的坑（实测）

| 平台 | 状态 | 坑 |
|---|---|---|
| Telegram | 通 | 无。本机曾遇 TLS reset 到 `api.telegram.org`，是网络侧，非代码 |
| 飞书 | 通 | ① 自建应用**必须在租户内发布版本**，否则 `im/v1/chats` 返回 `232034 The app is unavailable or inactivate in the tenant`，数据面和事件面一起不通（WS 连上但零事件）。② 事件要走**长连接**订阅 `im.message.receive_v1`。③ 私聊的 `conversation` 是用户的 `ou_` open_id，发送路径已按 `ou_` 判 `receive_id_type=open_id`。④ 发消息不需要 chats 列表，`chat_id` 只从事件来 |
| QQ | 通（群场景已修） | ① 后端**必须自己按 HELLO 间隔发 `op 1` 心跳**——服务器不来要，静默 60 秒后直接 reset。② reconnect 收到 `op 9 d=false` 表示旧会话死了，必须重新 IDENTIFY，不能 RESUME。③ 群消息的 `conversation` 必须带 `group:` 前缀：裸 `group_openid` 会让发送路径推断成 channel scene，POST `/channels/{id}/messages` → QQ 400 |
| QQ | 通 | ① 后端**必须自己按 HELLO 间隔发 `op 1` 心跳**——服务器不来要，静默 60 秒后直接 reset。② reconnect 收到 `op 9 d=false` 表示旧会话死了，必须重新 IDENTIFY，不能 RESUME。③ `conversation` 要带 scene 前缀（`:group:` / `:c2c:` 等），裸 id 会推断成 channel scene 打错端点。④ **`Authorization` 只能是 `QQBot <token>`**：`bearer_auth()` 会覆盖同一个 header，`Bearer` 换来的是 401 `请求头Authorization参数格式错误`（code 11241）。活体对拍：`QQBot` → 200 带消息 id，`Bearer` → 401 |

### 回复一条消息（被动消息）

投递正文第一行还是 `<chat:KEY>`；**第二行可选** `<msg:EVENT_ID>`，指定这条回复回答哪
条平台消息。不给的时候，默认指向该会话**最后一条入站事件**（`chat.jsonl` 里本来
就记着它）——也就是"回复你刚收到的那条"。

QQ 要这个 id：不带 `msg_id` 的群发送是**主动消息**，需要群主在群设置里打开「机器
人主动在群聊内发言」，否则 `40034105 主动消息失败, 无权限`。带上是被动回复，@ 机器
人即可，5 分钟内最多 5 次。飞书和 Telegram 用不到它，忽略这一行。

凭据写进网关根的 `.onlyne/credentials/<gateway>.token`（0600），后端进程从
`ONLYNE_IM_CREDENTIAL` 拿**路径**而不是秘密本身。`.env` 被 .gitignore 忽略。

## 目录

```text
examples/im-gateway/
├── .env.example     填好后复制成 .env
├── run.py           驱动（python3 标准库）
└── README.md
```

运行时树（不进仓库）：

```text
/tmp/onlyne-im-demo/
├── .onlyne/              server root：spec、state.db、key
├── ws/planner/           agent 角色的工作区
├── keymint/              给网关 role 铸 key 的临时工作区（key 已搬走）
├── im-in.sh              入站触发脚本（[[im.push]] 指向它）
└── gw/                   网关根
    ├── .onlyne/
    │   ├── config.toml   [gateway] role=im + [server] + 四行 [[im.backend]]（+可选 [im.listen]）
    │   ├── keys/role.key 网关 role 的 ed25519 私钥（0600）
    │   └── credentials/  每个后端一份凭据文件（0600，login 写的）
    └── conversations/    每个 IM 会话一个：坐标 + chat.key + chat.jsonl + media/
```

四家后端各自的 crate：`crates/onlyne-im-backend-{telegram,feishu,qqbot,fake}/`。微
信没有——它的物联网协议空闲后自己断联要重握手，代价不成比例。