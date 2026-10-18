# IM Gateway & Chat Protocol / 聊天网关与多平台接入

`onlyne-im` connects external chat platforms (Telegram, Feishu/Lark, QQ, Slack, Linear) to an Onlyne cluster. It is an optional, standalone binary—the core `onlyne-server` and `onlyne-client` compile zero chat SDKs.

---

## 1. v2.3 Chat Addressing Protocol (`<chat:KEY>`) / 聊天键寻址模型

- **One gateway root = one cluster role**: A gateway root directory (`<gateway-root>/.onlyne/config.toml`) registers a single `[[client]]` role in the cluster (default name `im` or operator-chosen `--role`) and holds a single TLS role link to `onlyne-server`.
- **Conversations are identified by `<chat:KEY>`**: Each conversation workspace lives under `<gateway-root>/conversations/<slug>/` alongside `.onlyne/chat.key`. The directory `<slug>` is a local operator label with no wire meaning; the chat key (e.g. `<chat:a1b2c3d4e5f6>`) placed on the first line of the delivery text is the conversation's identity.
- **Agents reply by preserving `<chat:KEY>`**: When a role replies or hands off back to the gateway role with `<chat:KEY>` on the first line, `onlyne-im` routes the outbound message to the owning platform backend and conversation.

---

## 2. v2.4 Reply Threads (`<msg:PREFIX>` & `<ref:PREFIX>`) / 回复线程与引用

A conversation is a message list stored in `<gateway-root>/conversations/<slug>/chat.jsonl`.
- **Inbound messages** are stamped with a short message ID prefix in the delivery text (e.g. `<msg:7f3a9c>`).
- **Explicit reply (`<msg:PREFIX>`)**: Including `<msg:7f3a9c>` on the header line after `<chat:KEY>` threads the outbound message as a native platform reply to that message.
  - *Strict resolution guard*: `onlyne-im` resolves `PREFIX` against the conversation's recorded history window. Ambiguous prefixes or unknown prefixes in a non-empty window are refused and recorded as an `im_unplaced_delivery` fault rather than silently leaking `<msg:...>` text to end users.
- **Cross-reference (`<ref:PREFIX>`)**: Cites another message without moving the reply thread anchor.

---

## 3. Supported Platform Backends / 支持的五大平台后端

| Backend Binary | Platform | Inbound Transport | Credential Format (`onlyne-im login`) |
|---|---|---|---|
| `onlyne-im-backend-telegram` | Telegram Bot | Long polling (`getUpdates`) | `<bot_token>` (or `ONLYNE_TELEGRAM_TOKEN`) |
| `onlyne-im-backend-feishu` | Feishu / Lark | `openlark` WebSocket long connection | `<app_id>:<app_secret>` |
| `onlyne-im-backend-qqbot` | QQ Open Platform | WebSocket event gateway | `<app_id>:<app_secret>` |
| `onlyne-im-backend-slack` | Slack | Socket Mode WebSocket | `<xapp-...>:<xoxb-...>` |
| `onlyne-im-backend-linear` | Linear | GraphQL comment polling (`ONLYNE_LINEAR_POLL_SECS`) | `<linear_api_key>` |
| `onlyne-im-backend-fake` | Local Test / Smoke | Scripted stdio frames | None |

---

## 4. Setting Up an IM Gateway / 配置与启动流程

### Guided Interactive Setup

```bash
onlyne-im setup --root ./im --server-root ./cluster
```

### Step-by-Step CLI Setup

```bash
# 1. Initialize the gateway root
onlyne-im init --root ./im --server-root ./cluster --role im

# 2. Store credentials (--interactive verifies against the live platform first)
onlyne-im login --root ./im --gateway telegram --token "$TELEGRAM_TOKEN" --interactive

# 3. Mint a conversation workspace and register the gateway role on the server
onlyne-im generate \
  --root ./im \
  --gateway telegram \
  --conversation -100123456789 \
  --slug tg-dev-group \
  --server-root ./cluster \
  --peer _supervisor --peer coder

# 4. Verify configuration and credentials
onlyne-im check --root ./im

# 5. Run the gateway (or pass --im-root ./im to `onlyne up`)
onlyne-im run --root ./im
```
