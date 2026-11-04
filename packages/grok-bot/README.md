# @onlyne/grok-bot

An Onlyne node that wakes [Grok Bot](https://x.com) whenever a message is delivered to it.
Each inbound admission is recorded, then POSTed as JSON to a webhook URL Grok Bot owns. The
webhook URL and the sender key it requires come from configuration and environment variables;
they are never hard-coded and must not be committed.

The process is an ordinary Onlyne node: identity keys, DNS-SD discovery, and the trust terminal
are the same pieces every gateway uses. It does not call a model. Inbound delivery is a webhook
POST (cloud Grok Bot) and, independently, a pull inbox (a device behind NAT). Outbound replies
are a small authenticated HTTP path so either consumer can send back through this node's
identity after it wakes.

## One account, one node, many threads

Keys are provisioned **per account** (operator / account scope), not per Grok Bot assistant.
Run **one grok-bot node** (one node id, one keypair, one webhook) for that account. Every
assistant DM and every group chat is a separate Onlyne thread under that node:

| Chat | Thread id | Owner |
|---|---|---|
| Assistant DM | `grokbot:dm:<agent_id>` | this grok-bot node |
| Group chat | `grokbot:group:<group_id>` | this grok-bot node |

`<agent_id>` and `<group_id>` are taken as-is (Onlyne node ids keep their `urn:onlyne:…`
colons). Example: a DM with `urn:onlyne:agent:alice` is thread
`{ "owner": "urn:onlyne:gateway:grok-bot", "id": "grokbot:dm:urn:onlyne:agent:alice" }`.

Do not stand up one grok-bot process or keypair per assistant. Pairing and trust stay at
account/node scope; isolation is the thread id plus `meta.grokbot`.

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
  },
  "trust_seeds": [
    { "node_id": "urn:onlyne:agent:alice", "fingerprint": "0123456789abcdef" }
  ]
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
| `ONLYNE_GROK_TRUST_SEEDS` | JSON array of `{ "node_id", "fingerprint" }` pins (overrides file `trust_seeds`) |

The default header is `Authorization: Bearer <key>`. Set `webhook.scheme` to `""` to send the
key as the header value with no prefix. Set `webhook.header` to any HTTP field Grok Bot expects.

Do not put the live URL or key in a committed file. Keep them in the process environment, or in
a local file that git ignores.

## Inbound threads

This node does **not** invent a random thread on first contact. Every peer admission is bound
to a stable grok-bot thread **before** `admit`:

1. If `message.meta.thread` is already a grok-bot-owned `grokbot:dm:…` or `grokbot:group:…`
   id, that id is kept.
2. Else `meta.data.grokbot` (the HTTP-safe bag) or `meta.grokbot` selects the chat:
   - `chat_kind`: `dm` (default) or `group`
   - `chat_id`: assistant / group id (`agent_id` is a DM alias, `group_id` a group alias)
3. Else the sender is a DM: `grokbot:dm:<sender>`.

A `group` kind without `chat_id` / `group_id` is refused. The node creates the thread and the
sender's membership if they do not already exist, then admits onto that `thread_ref`. Two DMs
or two groups never share a thread.

A peer that still names a legacy random UUID owned by this node is honored so an old
conversation can continue; new admissions without a thread always get the stable scheme.

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
  "thread": {
    "owner": "urn:onlyne:gateway:grok-bot",
    "id": "grokbot:dm:urn:onlyne:agent:alice"
  },
  "seq": "1",
  "delivery_kind": "note",
  "msg": "hello",
  "message": {
    "id": "msg-01",
    "sender": "urn:onlyne:agent:alice",
    "delivery_kind": "note",
    "msg": "hello",
    "meta": {
      "thread": {
        "owner": "urn:onlyne:gateway:grok-bot",
        "id": "grokbot:dm:urn:onlyne:agent:alice"
      },
      "grokbot": { "chat_kind": "dm", "chat_id": "urn:onlyne:agent:alice" }
    }
  },
  "meta": {
    "thread": {
      "owner": "urn:onlyne:gateway:grok-bot",
      "id": "grokbot:dm:urn:onlyne:agent:alice"
    },
    "grokbot": { "chat_kind": "dm", "chat_id": "urn:onlyne:agent:alice" }
  }
}
```

`meta.grokbot.chat_kind` and `meta.grokbot.chat_id` are always present so a multi-bot router
can fan out without parsing the thread id. They match the landed thread when the id uses the
stable scheme; a legacy random UUID falls back to `{ "chat_kind": "dm", "chat_id": "<sender>" }`.

The sender key is only in the configured header. Logs and `/grok/status` name the URL origin
plus a short hash of the full URL (`https://host#a1b2c3d4`), never the path, query, or key.
Grok Bot puts the routine id in the path, so the path is half the credential.

### Failures

A row is written to the store before the POST. Timeouts, network errors, `408`, `429`, and 5xx
retry with exponential backoff up to `max_attempts`. Other non-2xx responses are terminal.
Retries of one message never wait on another: each owed row has its own in-flight slot. After
the bound is exhausted the row stays as `failed` so it is not silently dropped; `/grok/status`
shows the queue.

## Outbound-only pull (no public IP)

A Cursor CLI / OMP node behind NAT cannot receive the webhook POST. It opens every connection
to this grok-bot node, using the **same account sender key**:

| Call | Purpose |
|---|---|
| `GET /grok/events?wait_ms=25000&limit=16&lease_ms=30000` | Long-poll owed wakes (empty list if none) |
| `POST /grok/events/ack` `{"ids":["sender/message-id"]}` | Confirm handling; at-least-once until then |
| `POST /grok/reply` | Unchanged reply path |

The pull inbox is **independent of the webhook**. Cloud Grok Bot can still be POSTed; a NAT
device can pull the same payload. An unacked lease expires and the same delivery id is offered
again. Rows written while the device is offline stay owed until it reconnects.

```ts
import { GrokBotPullClient } from '@onlyne/grok-bot';

const client = new GrokBotPullClient({
  origin: 'http://127.0.0.1:3995',
  webhook: { key: process.env.GROK_BOT_WEBHOOK_KEY!, header: 'Authorization', scheme: 'Bearer' },
});
const items = await client.pull();
for (const item of items) {
  // item.payload is the same JSON as the webhook body (thread, meta.grokbot, …)
  await client.ack([item.id]);
  await client.reply({
    to: item.payload.node_id as string,
    msg: 'ack',
    thread: item.payload.thread as { owner: string; id: string },
    in_reply_to: { sender: item.payload.sender as string, id: item.payload.message_id as string },
  });
}
```

`wait_ms=0` is a single poll. Default long-poll is 25s (max 30s). The HTTP listener sets
Bun.serve `idleTimeout` above that wait: the default Bun idle timeout is 10s and would
otherwise reset a quiet long-poll before it returned. Thread ids and `/grok/reply`
`thread` / `in_reply_to` rules are unchanged.

Messages this grok-bot node authors (`/grok/reply`) are **not** enqueued on the webhook or
the pull inbox. A consumer that answers every wake would otherwise loop on itself. Peer
inbound still wakes both paths.

The first message that lands on a new `grokbot:dm:` / `grokbot:group:` thread reports
`thread_created: true` even though the node stages that thread before `admit` (the library
would otherwise say `false` because the record already existed). `seq: "1"` is the first
entry either way.

## Headless pairing (`ONLYNE_NO_TUI`)

Without a TTY the trust terminal cannot run, so a server grok-bot cannot press `[a]` to
approve a peer. Pin **specific** peers in `trust_seeds` (or `ONLYNE_GROK_TRUST_SEEDS`):

```json
"trust_seeds": [
  { "node_id": "urn:onlyne:agent:alice", "fingerprint": "0123456789abcdef" }
]
```

Each pin is a node id plus the 16-hex fingerprint of **that peer's current key**. The library
treats a matching inbound statement as already locally approved. `approvePinned()` (called at
startup after seed discovery) also calls `trust.accept` for each pin; inspect refuses a live
key that does not match. Peers **not** in the list stay at `invited` until a human confirms
them on a TTY. A pin for a peer this node has not discovered yet is not a warning: the node
logs that the pin will be applied when that peer is first seen.

A pin does **not** override an existing non-revoked trust row. If this node already recorded
that `node_id` under a different public key, inbound statements and `approvePinned` fail with
`id_conflict` and the stored row is left as-is. The new-key peer stays at `requested` until
this node revokes the old row and re-approves, or the peer uses a new node id. A proven
rotation (`previous_signature` from the old key) keeps the existing local approval and does
not need a new pin.

This is not auto-accept-all. A missing fingerprint, `*`, or an empty pin is refused at
config load. A wrong fingerprint is `unauthenticated`. Compare fingerprints out of band
before pinning — a pin is as strong as the channel you used to copy it. Revoke still works.

## Outbound replies

Grok Bot replies by POSTing to this process, not by speaking Onlyne itself. The supported way
to continue the same conversation is to echo `thread` and `in_reply_to` from the webhook:

```sh
curl -X POST http://127.0.0.1:3995/grok/reply \
  -H "Authorization: Bearer $GROK_BOT_WEBHOOK_KEY" \
  -H "content-type: application/json" \
  -d '{"to":"urn:onlyne:gateway:grok-bot","msg":"ack","thread":{"owner":"urn:onlyne:gateway:grok-bot","id":"grokbot:dm:urn:onlyne:agent:alice"},"in_reply_to":{"sender":"urn:onlyne:agent:alice","id":"msg-01"}}'
```

`to` is this node (`node_id` in the webhook): the grok-bot node owns the thread and admits the
reply. `in_reply_to` also notifies the original sender.

| Body | What happens |
|---|---|
| `thread` + `in_reply_to` from the webhook | Lands on that `thread_ref` (supported continue) |
| `thread` only | Lands on that thread as a new root in the conversation |
| `in_reply_to` only | Recovers the original message's landing when this node admitted it |
| neither | Refused (`not_found`): this node has no default thread and does not invent one |

The path is `reply_path` (default `/grok/reply`). It uses the same sender key as the inbound
webhook. The node sends the message under its own identity with `response: none`. This path is
a host surface; `/onlyne/v3` is unchanged.

For a richer outbound surface — listing inbox, threads, neighbors, resources — run
`@onlyne/mcp-server` beside this node. That is a **separate process with its own node id and
store**. Multi-bot fan-out on this account is isolated by grok-bot threads; MCP tools read the
MCP node's own store unless the two identities are later unified.

## Status

```sh
curl http://127.0.0.1:3995/grok/status
```

The answer names the node, the redacted webhook, and the owed / failed / delivered counts.
`/grok/status` is local host diagnostics, not a protocol route.

## Library use

```ts
import { createGrokBotNode, grokBotThreadId } from '@onlyne/grok-bot';

grokBotThreadId({ kind: 'dm', chat_id: 'urn:onlyne:agent:alice' });
// 'grokbot:dm:urn:onlyne:agent:alice'
```

Tests inject `fetch` and `storage: 'memory'`. The CLI always supplies an identity key.

## License

MIT.
