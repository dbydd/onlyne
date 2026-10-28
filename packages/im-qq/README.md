# @onlyne/im-qq

The QQ connector for the Onlyne IM gateway. It implements translation for the official QQ Bot
OpenAPI v2: it authenticates with an app id and secret, receives events on the platform's
WebSocket gateway (or on the signed callback), normalizes inbound messages, and sends text,
Markdown, and quote replies.

## Credential

The constructor accepts the credential in this order: an explicit `app_id:app_secret` value, a
credential file, then the pair `QQ_BOT_APP_ID` and `QQ_BOT_APP_SECRET`.

## Transport

`transport: 'websocket'` is the default: QQ's own receive path. The connector fetches the access
token, asks `GET /gateway/bot` for the shard count and the session-start budget, and opens one
socket per shard. Each socket runs Hello → Identify → Ready, heartbeats on the negotiated interval
and treats a missing `op 11` acknowledgement as a dead connection, and resumes with `op 6` from the
last sequence number a listener accepted — a delivery that failed is replayed, never skipped.
Reconnects back off with jitter, and `session_start_limit` gates how many sessions start at once
and waits out `reset_after` instead of hammering the gateway.

`transport: 'webhook'` serves the signed HTTP callback instead: `handleWebhook` answers the
platform's challenge, verifies `X-Signature-Ed25519` over the timestamp and raw body, and
acknowledges accepted event batches. The gateway serves it at `/im/webhook` by default.

Outbound calls take the OpenAPI HTTP path under either transport.

## Supported

| Direction | Capability |
|---|---|
| Inbound | Text, quoted text, voice transcript, ARK prompt, attachment metadata |
| Outbound | Text, Markdown, passive reply, quote reply, retract, typing in direct messages |

Group typing is refused by the platform. Media upload, cards, and guild events are not
implemented. The connector declares only `recall` and `typing` beyond `send`, so another
operation is refused before an operation id is claimed.

## Example

```ts
import { createQqConnector } from '@onlyne/im-qq';

const connector = createQqConnector({ credentialFile: './credentials/qq' });
await connector.start();
```

The credential file may reference `$NAME` variables. A missing credential leaves the connector
in onboarding state and reports the variables it needs.

## Sources

- QQ Bot OpenAPI: <https://bot.q.qq.com/wiki/develop/api-v2/>
- Webhook signatures: <https://bot.q.qq.com/wiki/develop/api-v2/dev-prepare/interface-framework/sign.html>

## License

MIT.
