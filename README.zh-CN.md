# Onlyne

Onlyne 是一个小型 Rust 本地 IM channel daemon / broker。它让本地 agent 可以通过 workspace-local 的方式收发消息、订阅事件、查看历史，并接入多个聊天平台。

English README: [README.md](README.md).

## 它是什么

- 工作区本地：每个目录都有自己的 `.onlyne/` 配置、状态、socket、日志和缓存。
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
onlyne run
```

在同一个工作区的另一个终端运行：

```bash
onlyne client '{"id":"1","op":"ping"}'
onlyne client '{"id":"2","op":"status"}'
```

stdio 模式使用同一套请求格式：

```bash
echo '{"id":"1","op":"ping"}' | onlyne stdio
```

## 工作区目录

`onlyne init` 会在当前目录创建：

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

## Channel 配置

| Channel | 配置方式 |
| --- | --- |
| Telegram | 在 `.onlyne/.env` 写入 `TELEGRAM_BOT_TOKEN`，并启用 `[adapters.telegram]`。 |
| 飞书/Lark | 运行 `onlyne auth feishu` 扫码，或用 `--app-id` 和 `--app-secret` 绑定。 |
| QQ Bot | 在 `.onlyne/.env` 写入 `QQBOT_APP_ID` 和 `QQBOT_APP_SECRET`，并启用 `[adapters.qqbot]`。 |
| 微信 ilink | 运行 `onlyne auth weixin` 扫码，或用 `--token` 绑定。 |

认证命令只会写入当前工作区的 `.onlyne/`。

## 常用命令

```bash
onlyne init
onlyne run [--debug]
onlyne stdio
onlyne client '<json-request>'
onlyne config-check
onlyne auth feishu [--app-id <id> --app-secret <secret>]
onlyne auth weixin [--token <token>]
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

这些示例都是纯 CLI 工作流。secret 和运行态数据保存在对应示例目录的 `.onlyne/` 中，并被 git 忽略。

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
