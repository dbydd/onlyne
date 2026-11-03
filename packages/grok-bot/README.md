# @onlyne/grok-bot

An Onlyne node that wakes [Grok Bot](https://x.com) whenever a message is delivered to it.
Each inbound admission is recorded, then POSTed as JSON to a webhook URL Grok Bot owns. The
webhook URL and the sender key it requires come from configuration and environment variables;
they are never hard-coded and must not be committed.

The process is an ordinary Onlyne node: identity keys, DNS-SD discovery, and the trust terminal
are the same pieces every gateway uses. It does not call a model. Inbound delivery is the
webhook POST. Outbound replies are a small authenticated HTTP path so Grok Bot can send back
through this node's identity after it wakes.

## Run

```sh
bunx onlyne-grok-bot ./grok-bot.json
```

```json
{
  "node": { "id": "urn:onlyne:gateway:grok-bot" },
  "server": { "bind": "127.0.0.1", "port": 3995 },
  "storage": { "path": "./var/grok-bot/store.json" },
  "webhook": {
    "url": "$GROK_BOT_WEBHOOK_URL",
    "key": "$GROK_BOT_WEBHOOK_KEY",
    "header": "Authorization",
    "scheme": "Bearer"
  }
}
```

`$NAME` and `${NAME}` expand from the environment. `$$` is a literal `$`. A missing variable
fails startup instead of posting with an empty secret.

### Environment

| Variable | Purpose |
|---|---|
| `GROK_BOT_WEBHOOK_URL` | Typical name for `webhook.url` via `$GROK_BOT_WEBHOOK_URL` |
| `GROK_BOT_WEBHOOK_KEY` | Typical name for `webhook.key` via `$GROK_BOT_WEBHOOK_KEY` |
| `ONLYNE_GROK_WEBHOOK_URL` | Overrides `webhook.url` after file expansion |
| `ONLYNE_GROK_WEBHOOK_KEY` | Overrides `webhook.key` after file expansion |
| `ONLYNE_GROK_WEBHOOK_HEADER` | Overrides the header name (default `Authorization`) |
| `ONLYNE_GROK_WEBHOOK_SCHEME` | Overrides the header scheme (default `Bearer`; empty sends the raw key) |
| `ONLYNE_GROK_WEBHOOK_TIMEOUT_MS` | Per-attempt timeout (default `10000`) |
| `ONLYNE_GROK_WEBHOOK_MAX_ATTEMPTS` | Bounded retries (default `8`) |
| `ONLYNE_GROK_WEBHOOK_RETRY_BASE_MS` | Backoff base (default `500`; doubles each attempt, capped at 60s) |
| `ONLYNE_NODE_ID` | Overrides `node.id` |
| `ONLYNE_BIND` / `ONLYNE_PORT` | Overrides the listener |
| `ONLYNE_NO_TUI=1` | JSON log on stdout instead of the trust terminal |

The default header is `Authorization: Bearer <key>`. Set `webhook.scheme` to `""` to send the
key as the header value with no prefix. Set `webhook.header` to any HTTP field Grok Bot expects.

Do not put the live URL or key in a committed file. Keep them in the process environment, or in
a local file that git ignores.

## Inbound POST

The node POSTs immediately after an inbound message is admitted (or injected from a grant).
The body is JSON:

```json
{
  "node_id": "urn:onlyne:gateway:grok-bot",
  "sender": "urn:onlyne:agent:alice",
  "authenticated_author": "urn:onlyne:agent:alice",
  "reply_authority": "urn:onlyne:agent:alice",
  "message_id": "msg-01",
  "timestamp": "2026-11-03T14:00:00.000Z",
  "thread": { "owner": "urn:onlyne:gateway:grok-bot", "id": "…" },
  "seq": "1",
  "delivery_kind": "note",
  "msg": "hello",
  "message": {
    "id": "msg-01",
    "sender": "urn:onlyne:agent:alice",
    "delivery_kind": "note",
    "msg": "hello",
    "meta": {}
  }
}
```

The sender key is only in the configured header. Logs name the URL origin and path, never the
key, never a query string.

### Failures

A row is written to the store before the POST. Timeouts, network errors, `408`, `429`, and 5xx
retry with exponential backoff up to `max_attempts`. Other non-2xx responses are terminal.
Retries of one message never wait on another: each owed row has its own in-flight slot. After
the bound is exhausted the row stays as `failed` so it is not silently dropped; `/grok/status`
shows the queue.

## Outbound replies

Grok Bot replies by POSTing to this process, not by speaking Onlyne itself:

```sh
curl -X POST http://127.0.0.1:3995/grok/reply \
  -H "Authorization: Bearer $GROK_BOT_WEBHOOK_KEY" \
  -H "content-type: application/json" \
  -d '{"to":"urn:onlyne:agent:alice","msg":"ack","thread":{"owner":"urn:onlyne:gateway:grok-bot","id":"…"},"in_reply_to":{"sender":"urn:onlyne:agent:alice","id":"msg-01"}}'
```

The path is `reply_path` (default `/grok/reply`). It uses the same sender key as the inbound
webhook. The node sends the message under its own identity with `response: none`. This path is
a host surface; `/onlyne/v3` is unchanged.

For a richer outbound surface — listing inbox, threads, neighbors, resources — run
`@onlyne/mcp-server` beside this node.

## Status

```sh
curl http://127.0.0.1:3995/grok/status
```

The answer names the node, the redacted webhook, and the owed / failed / delivered counts.
`/grok/status` is local host diagnostics, not a protocol route.

## Library use

```ts
import { createGrokBotNode } from '@onlyne/grok-bot';
```

Tests inject `fetch` and `storage: 'memory'`. The CLI always supplies an identity key.

## License

MIT.
