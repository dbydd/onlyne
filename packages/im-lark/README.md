# @onlyne/im-lark

The Lark (Feishu) connector for the Onlyne IM gateway.

It normalizes inbound messages, posts, images, files, card actions, reactions, recalls, and bot
membership changes into `ImEvent`s. Outbound it sends text, Markdown, and interactive cards,
downloads inbound attachments into a media directory, and implements the platform operations
`update_card`, `recall`, and `react`.

## Credential

The constructor accepts the credential in this order: an explicit `app_id:app_secret` value, a
credential file (a `$VAR` reference resolves from the environment), then the pair
`FEISHU_APP_ID` and `FEISHU_APP_SECRET`.

A missing credential keeps the connector running in onboarding state. It reports the variables
it needs in the gateway status area instead of stopping the node.

## Example

```ts
import { createLarkConnector } from '@onlyne/im-lark';

const connector = createLarkConnector({ mediaDir: './var/media' });
await connector.start();
```

The gateway resolves this package by platform name `lark`, so a gateway configuration entry with
`"platform": "lark"` needs no direct import.

## Runtime

The package ships TypeScript source for Bun. A host that serves only other platforms never
installs it: the gateway loads connectors at runtime and answers `unsupported_delivery` when the
package is absent.

## License

MIT.
