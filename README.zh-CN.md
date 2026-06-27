# Onlyne

Onlyne 是一个小型 Rust 本地 IM channel daemon / broker。它让本地 agent 可以通过 workspace-local 的方式收发消息、订阅事件、查看历史，并接入多个聊天平台。

English README: [README.md](README.md).

## 它是什么

- 工作区本地：默认使用从当前目录向上找到的最近 `.onlyne/` 作为工作区；每个工作区都有自己的配置、状态、socket、日志和缓存。
- CLI 优先：可以前台运行，也可以交给 systemd/launchd 等外部 supervisor 包装，还可以用 stdio 模式被其他进程拉起。
- 本地 IPC：Unix socket 或 stdio 上的 newline-delimited JSON。
- 多 channel：Telegram、飞书/Lark、QQ Bot、微信 ilink。
- 轻量历史：用本地 SQLite 保存状态和消息历史。
- 事件流：本地客户端可以订阅入站、出站和适配器状态事件。

Onlyne 不是 agent runtime、模型运行器、调度器、Web 管理后台，也不是 prompt/memory 系统。

## 安装

```bash
cargo build --release
```

构建产物在 `target/release/onlyne`。开发时也可以使用 `cargo run --`。

## 快速开始

```bash
onlyne init
# 可选：把 Onlyne agent skill 导出/更新到当前工作区
onlyne export-skill
onlyne run
```

在同一个工作区树下的另一个终端运行：

```bash
onlyne client '{"id":"1","op":"ping"}'
onlyne client '{"id":"2","op":"status"}'
onlyne client '{"id":"wake","op":"loopback","text":"后台任务需要处理","raw_text":true}'
```

`loopback` 会写入本地 `loopback` channel 的入站消息，订阅中的 agent 可以用后台脚本唤醒自己，不需要外部 IM adapter。

stdio 模式使用同一套请求格式：

```bash
echo '{"id":"1","op":"ping"}' | onlyne stdio
```

## 工作区目录

默认情况下，Onlyne 会从当前目录开始向上查找最近的 `.onlyne/`，找到后把它所在目录作为工作区。如果没有找到 `.onlyne/`，则使用当前目录，因此 `onlyne init` 会初始化执行命令时所在的目录。也可以用 `--workspace <dir>` 显式指定工作区根目录。

`onlyne init` 会在选定工作区下创建：

```text
.onlyne/
  config.toml
  .env
  state.db
  run/onlyne.sock
  logs/daemon.log
  cache/media/
  adapters/
```

Onlyne 的工作区数据不会默认写到全局可变目录。

`onlyne export-skill` 会在选定工作区下写入或更新本地 agent skill：`.agents/skills/onlyne/SKILL.md`。这是工作区本地导出，不会写入 `~/.agents/skills`。

## Channel 配置

每个启用的 channel 都是单会话路由：可以配置一个 `bind_conversation_id`，也可以留空并在 adapter 连通后从目标会话发送 `/handshake` 自动绑定。agent 发送时只传 `channel_id`（`telegram`、`feishu`、`qqbot`、`wechat`）。未绑定 channel 收到非 `/handshake` 消息时会提示发送 `/handshake`。

| Channel | 配置方式 |
| --- | --- |
| Telegram | 在 `.onlyne/.env` 写入 `TELEGRAM_BOT_TOKEN`，启用 `[adapters.telegram]`，然后设置 `bind_conversation_id` 或发送 `/handshake`。 |
| 飞书/Lark | 运行 `onlyne auth feishu`，启用 `[adapters.feishu]`，然后设置 `bind_conversation_id` 或发送 `/handshake`。 |
| QQ Bot | 运行 `onlyne auth qqbot --app-id <id> --app-secret <secret>`；沙箱凭证加 `--sandbox`；设置 `bind_conversation_id` 或发送 `/handshake`。 |
| 微信 ilink | 运行 `onlyne auth wechat`，启用 `[adapters.wechat]`，然后设置 `bind_conversation_id` 或发送 `/handshake`。 |

认证命令只会写入选定工作区的 `.onlyne/`。

Adapter SDK：飞书使用 `openlark`，Telegram 使用 `teloxide`，微信 ilink 使用 `wechat-ilink`。QQ Bot 暂时保留轻量直接 API/gateway adapter，因为当前 Rust 社区 crate 对本项目路径还不够成熟。

## 常用命令

```bash
onlyne [--workspace <dir>] init
onlyne [--workspace <dir>] export-skill
onlyne [--workspace <dir>] run [--debug]
onlyne stdio
onlyne client '<json-request>'
onlyne config-check
onlyne auth feishu [--app-id <id> --app-secret <secret>]
onlyne auth qqbot --app-id <id> --app-secret <secret> [--sandbox]
onlyne auth wechat [--token <token>]
onlyne shell-completions zsh
onlyne shell-completions fish
```

`onlyne run --debug` 会在收到入站消息后，向同平台同会话回复脱敏后的 channel/conversation/thread 元数据。它只适合用来查 conversation id 或平台 thread 字段。

## 示例

- `examples/telegram/`
- `examples/feishu/`
- `examples/qqbot/`
- `examples/wechat/`
- `examples/broadcast/`
- `examples/multicast/`
- `examples/multi-channel/`

这些示例都是纯 CLI 工作流。建议在 `examples/` 下运行 `onlyne init`，让所有子目录共用被 git 忽略的 `examples/.onlyne/` 工作区；如果要隔离，也可以用 `--workspace <dir>` 或 `ONLYNE_WORKSPACE`。

## IPC

Onlyne 接收 newline-delimited JSON 请求。操作详情见 [docs/IPC.md](docs/IPC.md)。

最小请求：

```json
{"id":"1","op":"ping"}
```

最小响应：

```json
{"id":"1","ok":true,"data":{"pong":true}}
```

## 项目状态

当前实现说明和验证记录见 [docs/STATUS.md](docs/STATUS.md)。
