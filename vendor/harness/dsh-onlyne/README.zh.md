# dsh-onlyne

**通过 [Onlyne](https://github.com/dbydd/onlyne) 为 DeepSeek Harness agent 提供真正的 IM 收件箱/发件箱。**

`dsh-onlyne` 是 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)(`dsh`)的 Onlyne 插件。Onlyne 是一个工作区本地的 IM 频道守护进程。它提供面向模型的工具和一个 watch 循环,让 dsh agent 可以接收来自 IM 频道(Telegram、飞书/Lark、QQ 机器人、微信)的消息并发送回复——不需要把聊天平台伪装成终端或工作流引擎。

它是 [pi-onlyne](https://github.com/dbydd/pi-onlyne) 的 dsh 对应实现,并共享同一个项目级配置文件(`.pi/onlyne.json`),因此同一个工作区可以由任一 harness 桥接,无需重新配置。

## 安装

Onlyne 是单个二进制;请参阅 [Onlyne README](https://github.com/dbydd/onlyne) 安装并初始化一个工作区:

```bash
onlyne init
```

然后将插件安装到 dsh profile 并挂载:

```bash
dsh plugin --profile web add dsh-onlyne
```

在 profile 的 `cordis.patch.yml`(`$DSH_HOME/profiles/<name>/cordis.patch.yml`)中加入一行:

```yaml
- insert:
    - id: onlyne
      name: dsh-onlyne
```

插件会从 dsh 进程的调用目录向上逐级查找最近的 `.onlyne/` 目录,以此定位工作区。

## 工具

```text
onlyne_daemon_start()
onlyne_daemon_stop()
onlyne_daemon_restart()
onlyne_reply({ text })
onlyne_send({ channelId, text, rawText? })
onlyne_broadcast({ targets, text, rawText? })
onlyne_loopback({ text, rawText? })
onlyne_mark_no_reply({ reason? })
```

### 发送一条消息

```
onlyne_send({
  channelId: "telegram",
  text: "# Build report\n\nAll checks passed."
})
```

设置 `rawText: true` 可发送字面文本而非 Markdown。

### 广播

```
onlyne_broadcast({
  targets: [{ channelId: "telegram" }, { channelId: "feishu" }],
  text: "# Release shipped"
})
```

### Loopback 唤醒

从任意本地脚本向运行中的守护进程注入一条入站消息:

```bash
onlyne client '{"id":"wake","op":"loopback","text":"background job finished","raw_text":true}'
```

`loopback` 频道仅用于唤醒:它会在会话中产生一条 follow-up,但不期望 `onlyne_reply`。

## 命令

```text
/onlyne status
/onlyne watch on
/onlyne watch off
/onlyne daemon start
/onlyne daemon stop
/onlyne daemon restart
/onlyne config auto-start
```

`watch on` 订阅守护进程的事件流;入站消息会以用户 follow-up 的形式进入当前 dsh 会话,agent 用 `onlyne_reply` 回复,或用 `onlyne_mark_no_reply` 表示无需回复。`/handshake` 等控制消息会被静默消费。`config auto-start` 切换是否在会话启动时自动开启 watch。

## 配置

与 pi-onlyne 共享 `.pi/onlyne.json`:

```json
{
  "watch": { "autoStart": false },
  "inbound": {
    "defaultMode": "auto-handle",
    "rules": [{ "channel": "telegram", "mode": "queue-only" }]
  },
  "outbound": {
    "defaultReplyMode": "guarded-explicit",
    "retry": { "attempts": 2, "concurrency": 8 }
  }
}
```

## 开发

```bash
npm install
npm run check     # 构建 + 测试
npm pack          # 构建可发布的 tarball
```

## 链接

- Onlyne 主仓库:https://github.com/dbydd/onlyne
- dsh-onlyne 包:https://www.npmjs.com/package/dsh-onlyne
