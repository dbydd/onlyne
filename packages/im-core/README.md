# @onlyne/im-core

The connector contract for IM platforms and the driver bridge that turns platform events into
Onlyne resources.

A connector owns one platform connection. It normalizes inbound events, sends outbound
messages, and declares the extra platform operations it implements. The bridge owns the
protocol side: it derives a resource id from the platform coordinate, adopts the resource on
first contact, appends events once per platform event id, and maps replies to the resource that
carries them.

## Exports

- `Connector`, `ImEvent`, `SendOutcome` — the connector contract.
- `createDriverBridge` — the `ResourceDriver` the gateway hands to `@onlyne/lib`.
- `coordinateOf`, `resolveCredential` — id derivation and the credential chain (explicit value,
  credential file, then the platform's environment pair).

## Example

```ts
import { createDriverBridge } from '@onlyne/im-core';

const bridge = createDriverBridge({ connector, platform: 'lark', threadOf, log });
```

## Runtime

The package ships TypeScript source for Bun and has no platform dependency. Install a connector
package such as `@onlyne/im-lark` beside it to serve a real platform.

## License

MIT.
