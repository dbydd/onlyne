# @onlyne/im-fake

A scripted in-memory IM connector. It replays inbound events and dequeues one send answer per
outbound message, so a gateway or bridge test runs with no platform account.

## Example

```ts
import { createFakeConnector } from '@onlyne/im-fake';

const connector = createFakeConnector({
  events: [{ platform: 'fake', conversation: 'c1', kind: 'message', text: 'hello', source_id: 'e1' }],
  answers: [{ state: 'committed', message_handle: 'm1' }],
});
```

Inbound events that carry no platform handle receive a synthetic `om_fake_N` handle, because the
bridge requires a reply target on every message event. An empty answer queue throws, which keeps
a test from passing on an unintended send.

The gateway also loads this package by platform name `fake`, so a configuration entry with
`"platform": "fake"` and a `script` object needs no code.

## Runtime

The package ships TypeScript source for Bun.

## License

MIT.
