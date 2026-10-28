# @onlyne/node-host

The host half that every Onlyne node shares: identity keys on disk, DNS-SD discovery, the
gateway trust terminal, a bounded log ring, and the English/Simplified Chinese message
catalogs.

The library owns protocol state. This package owns the filesystem and terminal work that the
library is not allowed to do.

## Entry points

| Export | Contents |
|---|---|
| `@onlyne/node-host` | `startNodeHost`, `GatewayTui`, `createLogger`, `NetworkDiscovery` |
| `@onlyne/node-host/keys` | `createNodeKey`, `loadOrCreateKey`, `writeKeyMaterial`, `ensureKeyDir` |
| `@onlyne/node-host/network` | DNS-SD publish and browse over `bonjour-service` |
| `@onlyne/node-host/i18n` | `Locale`, `resolveLocale`, `getLocale`, `setLocale`, `createTranslator` |

`startNodeHost` publishes the bound endpoint, browses for peers, re-observes the catalog every
30 seconds, and on a terminal hands the operator the trust screen. Discovery only adds
candidates: a key exchange still needs both fingerprints confirmed.

`startNodeHost({ status })` accepts an async callback that returns gateway-specific display lines.
The terminal refreshes these lines with its trust view once per second. Hosts use them for connection
state, server tool counts, scheduled tasks, and delivery queues. `ONLYNE_NO_TUI=1` keeps JSON logging
and skips terminal startup.

## Log levels

`createLogger` returns one callable line writer plus the other three levels: calling it, and
`logger.log`, write `info`, so every existing host line keeps its level. `ONLYNE_LOG_LEVEL` selects
the threshold, default `info`; a fourth argument overrides it for one process. Every level that
passes goes to the ring the TUI renders and to the file sink, and each JSON line names its `level`.

| Level | Meaning | Examples |
|---|---|---|
| `debug` | Per-pass work and protocol internals, off by default | reconcile pulls, replays, injected-context details |
| `info` | State transitions and business milestones, one line each | `onlyne.tui.ready`, `connector.state`, a committed send |
| `warn` | Degraded but running | reconnects, retries, an unconfirmed delivery, a rate-limit hit, a failed reconcile pull |
| `error` | The operation failed and the caller lost something | a received event that could not be stored |

A sink that is a plain function rather than a `createLogger` result records every line it is
handed: it has no threshold of its own, so filtering belongs to the host that built it.

## Language

`resolveLocale` reads an explicit value first, then `ONLYNE_LANG`, then `LC_ALL` or `LANG`, and
falls back to `en`. A module that renders text keeps its own `en` and `zh-CN` catalogs and calls
`createTranslator`; the returned function reads the process locale on every call.

## Runtime

The package ships TypeScript source for Bun. It performs filesystem access, so it belongs to the
host, never to `@onlyne/lib`.

## License

MIT.
