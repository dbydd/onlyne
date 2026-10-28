# @onlyne/im-wechat

The WeChat connector for the Onlyne IM gateway. It uses the official iLink (ClawBot) bot API:
long-poll for inbound messages, send text with the conversation context token, and report typing.

## Credential

The constructor accepts a `WECHAT_ILINK_BOT_TOKEN` value from explicit configuration, a
credential file, or the environment. `WECHAT_ILINK_BASE_URL` may override the platform base URL.
The token comes from the ClawBot QR login flow, which the node does not drive itself.

## Transport

The connector long-polls `getupdates` and holds the platform continuation cursor in memory for
the life of the poll loop. It advances that cursor only after the gateway stored every message in
the batch. A listener failure leaves the cursor unchanged, and the platform replay is safe
because resource delivery deduplicates by platform message id.

## Supported

| Direction | Capability |
|---|---|
| Inbound | Text, voice transcript, image, file, video, and audio handles |
| Outbound | Text with the inbound context token, typing |

Quoting, attachments, recall, and group administration are not implemented. A send without an
inbound context token is refused by the platform, and that refusal is reported verbatim. An
expired session (`ret = -14`) moves the connector to a paused state with an onboarding note and
keeps retrying.

## Example

```ts
import { createWechatConnector } from '@onlyne/im-wechat';

const connector = createWechatConnector({ credentialFile: './credentials/wechat' });
await connector.start();
```

## Sources

- WeChat iLink openapi: <https://developers.weixin.qq.com/doc/aispeech/knowledge/openapi/Clawbotrelated.html>
- Wire protocol: <https://github.com/Tencent/openclaw-weixin/blob/main/docs/protocol.md>

## License

MIT.
