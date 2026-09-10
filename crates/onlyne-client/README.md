# onlyne-client

One workspace, one role, one daemon. Many concurrent sessions inside the role.

## Runloop tasks

The foreground loop runs four concurrent tasks over one server connection:

1. Frame reader: reads server frames and answers hello. A reader error or a `bye` parks delivery.
2. Pull/ack delivery loop: pulls deliveries and acks settled outcomes. `accept_new = false` declines new pulls while running sessions settle.
3. Intent flusher: flushes pending intents in `seq` order after handshake and welcome.
4. Adapter socket acceptor: serves plugin and local CLI connections while the server link reconnects.

After reconnect the order is handshake, welcome, flush intents, resume pulling.

## Accept rule

While `accept_new` is false the client declines to pull. Queued rows wait on the server.

## Intent table

| state | meaning | next |
| --- | --- | --- |
| pending | enqueued before the first write attempt | accepted, retrying, exhausted, deleted |
| retrying | waiting for a later attempt | accepted, retrying, exhausted, deleted |
| accepted | receipt stored | terminal |
| exhausted | attempt ceiling reached with a recorded fault | terminal |

Enqueue precedes the first socket write. Attempt increments follow the spec backoff ladder. Exhaustion records `intent_exhausted` and pushes `Report::Fault` when the server link exists.

## Error split

Permanent, no retry: `acl_denied`, `invalid`, `conflict`, `forbidden`, `unknown_role`, `not_admin`, `bad_frame`, `frame_too_large`, `protocol_version`.

Retryable: `internal`, `duplicate` replayed as the stored receipt, connection errors.

## Local prose

`Welcome.prose` is cached in `prose_cache` keyed by role with `spec_hash`. Role queries answer from that cache. The CLI prose export reads the same record.
