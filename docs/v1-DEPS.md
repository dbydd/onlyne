# v1 dependency inventory

This inventory reads every `Cargo.toml` under `crates/` except `crates/onlyne-legacy/` and every plugin manifest under `plugins/`. Dependency rows include normal, development, and build dependencies. `workspace` records a requirement inherited from the root manifest. Feature lists report features written in the declaring manifest; `default-features=false` is shown explicitly.

## External dependencies

| dependency | version requirement | feature list | declaring crates |
|---|---|---|---|
| `aes-gcm` | `0.10` | [] | onlyne-gateway |
| `anyhow` | `1.0` | [] | onlyne-client |
| `anyhow` | `1` | [] | onlyne-server, onlyne-store, onlyne-testkit |
| `anyhow` | workspace | [] | onlyne-session |
| `async-stream` | `0.3` | [] | onlyne-adapter |
| `async-trait` | `0.1` | [] | onlyne-adapter, onlyne-gateway, onlyne-gateway-telegram, onlyne-server, onlyne-testkit |
| `async-trait` | workspace | [] | onlyne-gateway-feishu, onlyne-gateway-qqbot, onlyne-gateway-weixin |
| `base64` | `0.22.1` | [] | onlyne-net |
| `base64` | `0.22` | [] | onlyne-client, onlyne-config, onlyne-gateway, onlyne-gateway-telegram, onlyne-server (dev-dependencies), onlyne-testkit |
| `base64` | workspace | [] | onlyne-cli, onlyne-gateway-feishu, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto |
| `chrono` | `0.4` | [`serde`] | onlyne-adapter (dev-dependencies), onlyne-client, onlyne-gateway, onlyne-gateway-feishu, onlyne-server, onlyne-store, onlyne-testkit |
| `chrono` | workspace | [] | onlyne-proto, onlyne-session |
| `clap` | `4.5` | [`derive`] | onlyne-client |
| `clap` | `4` | [`derive`] | onlyne-gateway, onlyne-server, onlyne-testkit |
| `clap` | workspace | [] | onlyne-cli |
| `clap_complete` | workspace | [] | onlyne-cli |
| `ed25519-dalek` | `2.1.1` | [`std`, `rand_core`]; default-features=false | onlyne-net |
| `ed25519-dalek` | `2.1` | [`std`, `rand_core`]; default-features=false | onlyne-client |
| `flate2` | `1.0` | [] | onlyne-client |
| `futures-util` | `0.3` | [] | onlyne-adapter, onlyne-testkit |
| `open_lark` | `0.17.0` | [`websocket`]; default-features=false | onlyne-gateway-feishu |
| `parking_lot` | `0.12` | [] | onlyne-client |
| `pulldown-cmark` | `0.12` | [] | onlyne-gateway |
| `qrcode` | `0.14` | []; default-features=false | onlyne-gateway |
| `rand` | `0.8.6` | [] | onlyne-net |
| `rand` | `0.8` | [] | onlyne-client |
| `rcgen` | `0.13.2` | [`ring`, `pem`]; default-features=false | onlyne-net |
| `reqwest` | workspace | [] | onlyne-gateway-qqbot |
| `reqwest` | workspace | [`multipart`] | onlyne-gateway-feishu |
| `resvg` | `0.45` | [`text`, `system-fonts`]; default-features=false | onlyne-gateway |
| `rusqlite` | `0.32` | [`bundled`, `chrono`, `serde_json`] | onlyne-client, onlyne-store |
| `rusqlite` | `0.32` | [`bundled`] | onlyne-gateway, onlyne-server |
| `rustls` | `0.23.40` | [`ring`, `std`]; default-features=false | onlyne-net |
| `rustls` | `0.23` | [`ring`, `std`]; default-features=false | onlyne-server |
| `rustls-pemfile` | `2.2.0` | [] | onlyne-net |
| `rustls-pki-types` | `1.14.1` | [] | onlyne-net |
| `schemars` | `0.8` | [] | onlyne-config |
| `schemars` | workspace | [`chrono`, `uuid1`] | onlyne-proto |
| `serde` | `1.0.228` | [`derive`] | onlyne-net |
| `serde` | `1.0` | [`derive`] | onlyne-client |
| `serde` | `1` | [`derive`] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-gateway-telegram, onlyne-server, onlyne-store, onlyne-testkit |
| `serde` | workspace | [] | onlyne-cli, onlyne-frame, onlyne-gateway-feishu, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto, onlyne-session |
| `serde_json` | `1.0.150` | [] | onlyne-net |
| `serde_json` | `1.0` | [] | onlyne-client |
| `serde_json` | `1` | [] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-gateway-telegram, onlyne-server, onlyne-store, onlyne-testkit |
| `serde_json` | workspace | [] | onlyne-cli, onlyne-frame, onlyne-gateway-feishu, onlyne-gateway-feishu (dev-dependencies), onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto, onlyne-session |
| `sha2` | `0.10.9` | [] | onlyne-net |
| `sha2` | `0.10` | [] | onlyne-config, onlyne-gateway |
| `sha2` | workspace | [] | onlyne-proto |
| `tar` | `0.4` | [] | onlyne-client |
| `teloxide` | `0.17.0` | [`rustls`]; default-features=false | onlyne-gateway-telegram |
| `tempfile` | `3.0` | [] | onlyne-client (dev-dependencies) |
| `tempfile` | `3.27.0` | [] | onlyne-net (dev-dependencies) |
| `tempfile` | `3` | [] | onlyne-config (dev-dependencies), onlyne-gateway (dev-dependencies), onlyne-layout (dev-dependencies), onlyne-server (dev-dependencies), onlyne-store (dev-dependencies), onlyne-testkit |
| `tempfile` | workspace | [] | onlyne-cli (dev-dependencies), onlyne-session (dev-dependencies) |
| `time` | `0.3.49` | [`std`, `formatting`]; default-features=false | onlyne-net |
| `tokio` | `1.0` | [`rt-multi-thread`, `macros`, `net`, `io-util`, `fs`, `sync`, `time`, `process`] | onlyne-client |
| `tokio` | `1.52.3` | [`macros`, `net`, `io-util`, `fs`, `time`, `rt-multi-thread`] | onlyne-net |
| `tokio` | `1` | [`fs`, `macros`, `net`, `process`, `rt-multi-thread`, `time`] | onlyne-gateway |
| `tokio` | `1` | [`io-util`, `io-std`, `net`, `process`, `rt-multi-thread`, `macros`, `sync`, `time`, `fs`] | onlyne-testkit |
| `tokio` | `1` | [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter |
| `tokio` | `1` | [`rt-multi-thread`, `macros`, `net`, `io-util`, `sync`, `time`, `fs`, `signal`] | onlyne-server |
| `tokio` | workspace | [] | onlyne-cli, onlyne-frame, onlyne-gateway-feishu, onlyne-gateway-qqbot, onlyne-gateway-weixin |
| `tokio` | workspace | [`rt-multi-thread`, `macros`] | onlyne-frame (dev-dependencies) |
| `tokio-rustls` | `0.26.4` | [`ring`]; default-features=false | onlyne-net |
| `tokio-stream` | `0.1` | [] | onlyne-testkit |
| `tokio-tungstenite` | workspace | [] | onlyne-gateway-qqbot |
| `toml` | `0.8` | [] | onlyne-client, onlyne-config, onlyne-server |
| `tracing` | `0.1` | [] | onlyne-adapter, onlyne-client, onlyne-server, onlyne-store, onlyne-testkit |
| `tracing` | workspace | [] | onlyne-gateway-feishu, onlyne-session |
| `unicode-display-width` | `0.2` | [] | onlyne-gateway |
| `unicode-segmentation` | `1` | [] | onlyne-store |
| `uuid` | `1.0` | [`v4`, `serde`] | onlyne-client |
| `uuid` | `1` | [`v4`, `serde`] | onlyne-server |
| `uuid` | workspace | [] | onlyne-proto |
| `wechat-ilink` | workspace | [] | onlyne-gateway-weixin |
| `x509-parser` | `0.16.0` | [] | onlyne-net |

A crate in parentheses declares the dependency in `[dev-dependencies]` or `[build-dependencies]`. A plain crate name declares it in `[dependencies]`.

## Internal edges

| declaring crate | dependency | section | version requirement | path/reference |
|---|---|---|---|---|
| `onlyne-adapter` | `onlyne-frame` | `dependencies` | unspecified | `../onlyne-frame` |
| `onlyne-adapter` | `onlyne-proto` | `dependencies` | unspecified | `../onlyne-proto` |
| `onlyne-cli` | `onlyne-frame` | `dependencies` | workspace | `` |
| `onlyne-cli` | `onlyne-proto` | `dependencies` | workspace | `` |
| `onlyne-client` | `onlyne-adapter` | `dependencies` | unspecified | `../onlyne-adapter` |
| `onlyne-client` | `onlyne-config` | `dependencies` | unspecified | `../onlyne-config` |
| `onlyne-client` | `onlyne-frame` | `dependencies` | unspecified | `../onlyne-frame` |
| `onlyne-client` | `onlyne-layout` | `dependencies` | unspecified | `../onlyne-layout` |
| `onlyne-client` | `onlyne-net` | `dependencies` | unspecified | `../onlyne-net` |
| `onlyne-client` | `onlyne-proto` | `dependencies` | unspecified | `../onlyne-proto` |
| `onlyne-client` | `onlyne-session` | `dependencies` | unspecified | `../onlyne-session` |
| `onlyne-client` | `onlyne-store` | `dependencies` | unspecified | `../onlyne-store` |
| `onlyne-client` | `onlyne-testkit` | `dev-dependencies` | unspecified | `../onlyne-testkit` |
| `onlyne-gateway` | `onlyne-adapter` | `dependencies` | `1.0.0` | `../onlyne-adapter` |
| `onlyne-gateway` | `onlyne-config` | `dependencies` | `1.0.0` | `../onlyne-config` |
| `onlyne-gateway` | `onlyne-gateway-feishu` | `dependencies` | `1.0.0` | `../../plugins/onlyne-gateway-feishu` |
| `onlyne-gateway` | `onlyne-gateway-qqbot` | `dependencies` | `1.0.0` | `../../plugins/onlyne-gateway-qqbot` |
| `onlyne-gateway` | `onlyne-gateway-telegram` | `dependencies` | `1.0.0` | `../../plugins/onlyne-gateway-telegram` |
| `onlyne-gateway` | `onlyne-gateway-weixin` | `dependencies` | `1.0.0` | `../../plugins/onlyne-gateway-weixin` |
| `onlyne-gateway` | `onlyne-net` | `dependencies` | `1.0.0` | `../onlyne-net` |
| `onlyne-gateway` | `onlyne-proto` | `dependencies` | `1.0.0` | `../onlyne-proto` |
| `onlyne-gateway-feishu` | `onlyne-adapter` | `dependencies` | workspace | `` |
| `onlyne-gateway-feishu` | `onlyne-proto` | `dependencies` | workspace | `` |
| `onlyne-gateway-qqbot` | `onlyne-adapter` | `dependencies` | workspace | `` |
| `onlyne-gateway-qqbot` | `onlyne-proto` | `dependencies` | workspace | `` |
| `onlyne-gateway-telegram` | `onlyne-adapter` | `dependencies` | workspace | `` |
| `onlyne-gateway-telegram` | `onlyne-proto` | `dependencies` | workspace | `` |
| `onlyne-gateway-weixin` | `onlyne-adapter` | `dependencies` | workspace | `` |
| `onlyne-gateway-weixin` | `onlyne-proto` | `dependencies` | workspace | `` |
| `onlyne-net` | `onlyne-frame` | `dependencies` | `1.0.0` | `../onlyne-frame` |
| `onlyne-net` | `onlyne-proto` | `dependencies` | unspecified | `../onlyne-proto` |
| `onlyne-server` | `onlyne-adapter` | `dependencies` | `1.0.0` | `../onlyne-adapter` |
| `onlyne-server` | `onlyne-config` | `dependencies` | `1.0.0` | `../onlyne-config` |
| `onlyne-server` | `onlyne-frame` | `dependencies` | `1.0.0` | `../onlyne-frame` |
| `onlyne-server` | `onlyne-layout` | `dependencies` | `1.0.0` | `../onlyne-layout` |
| `onlyne-server` | `onlyne-net` | `dependencies` | `1.0.0` | `../onlyne-net` |
| `onlyne-server` | `onlyne-proto` | `dependencies` | `1.0.0` | `../onlyne-proto` |
| `onlyne-server` | `onlyne-store` | `dependencies` | `1.0.0` | `../onlyne-store` |
| `onlyne-server` | `onlyne-testkit` | `dev-dependencies` | `1.0.0` | `../onlyne-testkit` |
| `onlyne-store` | `onlyne-frame` | `dev-dependencies` | unspecified | `../onlyne-frame` |
| `onlyne-store` | `onlyne-proto` | `dependencies` | unspecified | `../onlyne-proto` |
| `onlyne-store` | `onlyne-session` | `dependencies` | unspecified | `../onlyne-session` |
| `onlyne-testkit` | `onlyne-adapter` | `dependencies` | unspecified | `../onlyne-adapter` |
| `onlyne-testkit` | `onlyne-config` | `dependencies` | unspecified | `../onlyne-config` |
| `onlyne-testkit` | `onlyne-frame` | `dependencies` | unspecified | `../onlyne-frame` |
| `onlyne-testkit` | `onlyne-proto` | `dependencies` | unspecified | `../onlyne-proto` |

Firewall review against `docs/v1-PLAN.md` §1 lines 74–77:

- `onlyne-proto` declares no `tokio` dependency (checked in its external row above).
- `onlyne-session` edges: ; none names `onlyne-store`, `onlyne-net`, or `onlyne-proto`.
- `onlyne-client` declares no `onlyne-server`; `onlyne-server` declares no `onlyne-client`.
- Plugin internal edges are onlyne-adapter, onlyne-proto; no plugin declares a server-internal crate.
- No firewall violation appears in the manifests reviewed.

## Conflicts

A conflict is any dependency with more than one manifest-level version requirement or feature/default-feature set. Workspace requirements are kept verbatim because this inventory is based on declaring manifests.

| dependency | variant A | crates | variant B or additional variant | crates |
|---|---|---|---|---|
| `anyhow` | `1.0` / [] | onlyne-client | `1` / [] | onlyne-server, onlyne-store, onlyne-testkit |
| `anyhow` | `1.0` / [] | onlyne-client | workspace / [] | onlyne-session |
| `async-trait` | `0.1` / [] | onlyne-adapter, onlyne-gateway, onlyne-gateway-telegram, onlyne-server, onlyne-testkit | workspace / [] | onlyne-gateway-feishu, onlyne-gateway-qqbot, onlyne-gateway-weixin |
| `base64` | workspace / [] | onlyne-cli, onlyne-gateway-feishu, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto | `0.22` / [] | onlyne-client, onlyne-config, onlyne-gateway, onlyne-gateway-telegram, onlyne-server (dev-dependencies), onlyne-testkit |
| `base64` | workspace / [] | onlyne-cli, onlyne-gateway-feishu, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto | `0.22.1` / [] | onlyne-net |
| `chrono` | `0.4` / [`serde`] | onlyne-adapter (dev-dependencies), onlyne-client, onlyne-gateway, onlyne-gateway-feishu, onlyne-server, onlyne-store, onlyne-testkit | workspace / [] | onlyne-proto, onlyne-session |
| `clap` | workspace / [] | onlyne-cli | `4.5` / [`derive`] | onlyne-client |
| `clap` | workspace / [] | onlyne-cli | `4` / [`derive`] | onlyne-gateway, onlyne-server, onlyne-testkit |
| `ed25519-dalek` | `2.1` / [`std`, `rand_core`]; default-features=false | onlyne-client | `2.1.1` / [`std`, `rand_core`]; default-features=false | onlyne-net |
| `rand` | `0.8` / [] | onlyne-client | `0.8.6` / [] | onlyne-net |
| `reqwest` | workspace / [`multipart`] | onlyne-gateway-feishu | workspace / [] | onlyne-gateway-qqbot |
| `rusqlite` | `0.32` / [`bundled`, `chrono`, `serde_json`] | onlyne-client, onlyne-store | `0.32` / [`bundled`] | onlyne-gateway, onlyne-server |
| `rustls` | `0.23.40` / [`ring`, `std`]; default-features=false | onlyne-net | `0.23` / [`ring`, `std`]; default-features=false | onlyne-server |
| `schemars` | `0.8` / [] | onlyne-config | workspace / [`chrono`, `uuid1`] | onlyne-proto |
| `serde` | `1` / [`derive`] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-gateway-telegram, onlyne-server, onlyne-store, onlyne-testkit | workspace / [] | onlyne-cli, onlyne-frame, onlyne-gateway-feishu, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto, onlyne-session |
| `serde` | `1` / [`derive`] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-gateway-telegram, onlyne-server, onlyne-store, onlyne-testkit | `1.0` / [`derive`] | onlyne-client |
| `serde` | `1` / [`derive`] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-gateway-telegram, onlyne-server, onlyne-store, onlyne-testkit | `1.0.228` / [`derive`] | onlyne-net |
| `serde_json` | `1` / [] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-gateway-telegram, onlyne-server, onlyne-store, onlyne-testkit | workspace / [] | onlyne-cli, onlyne-frame, onlyne-gateway-feishu, onlyne-gateway-feishu (dev-dependencies), onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto, onlyne-session |
| `serde_json` | `1` / [] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-gateway-telegram, onlyne-server, onlyne-store, onlyne-testkit | `1.0` / [] | onlyne-client |
| `serde_json` | `1` / [] | onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-gateway-telegram, onlyne-server, onlyne-store, onlyne-testkit | `1.0.150` / [] | onlyne-net |
| `sha2` | `0.10` / [] | onlyne-config, onlyne-gateway | `0.10.9` / [] | onlyne-net |
| `sha2` | `0.10` / [] | onlyne-config, onlyne-gateway | workspace / [] | onlyne-proto |
| `tempfile` | workspace / [] | onlyne-cli (dev-dependencies), onlyne-session (dev-dependencies) | `3.0` / [] | onlyne-client (dev-dependencies) |
| `tempfile` | workspace / [] | onlyne-cli (dev-dependencies), onlyne-session (dev-dependencies) | `3` / [] | onlyne-config (dev-dependencies), onlyne-gateway (dev-dependencies), onlyne-layout (dev-dependencies), onlyne-server (dev-dependencies), onlyne-store (dev-dependencies), onlyne-testkit |
| `tempfile` | workspace / [] | onlyne-cli (dev-dependencies), onlyne-session (dev-dependencies) | `3.27.0` / [] | onlyne-net (dev-dependencies) |
| `tokio` | `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter | workspace / [] | onlyne-cli, onlyne-frame, onlyne-gateway-feishu, onlyne-gateway-qqbot, onlyne-gateway-weixin |
| `tokio` | `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter | `1.0` / [`rt-multi-thread`, `macros`, `net`, `io-util`, `fs`, `sync`, `time`, `process`] | onlyne-client |
| `tokio` | `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter | workspace / [`rt-multi-thread`, `macros`] | onlyne-frame (dev-dependencies) |
| `tokio` | `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter | `1` / [`fs`, `macros`, `net`, `process`, `rt-multi-thread`, `time`] | onlyne-gateway |
| `tokio` | `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter | `1.52.3` / [`macros`, `net`, `io-util`, `fs`, `time`, `rt-multi-thread`] | onlyne-net |
| `tokio` | `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter | `1` / [`rt-multi-thread`, `macros`, `net`, `io-util`, `sync`, `time`, `fs`, `signal`] | onlyne-server |
| `tokio` | `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] | onlyne-adapter | `1` / [`io-util`, `io-std`, `net`, `process`, `rt-multi-thread`, `macros`, `sync`, `time`, `fs`] | onlyne-testkit |
| `tracing` | `0.1` / [] | onlyne-adapter, onlyne-client, onlyne-server, onlyne-store, onlyne-testkit | workspace / [] | onlyne-gateway-feishu, onlyne-session |
| `uuid` | `1.0` / [`v4`, `serde`] | onlyne-client | workspace / [] | onlyne-proto |
| `uuid` | `1.0` / [`v4`, `serde`] | onlyne-client | `1` / [`v4`, `serde`] | onlyne-server |

Conflicting dependencies by name:

- `anyhow`: `1.0` / [] (onlyne-client); `1` / [] (onlyne-server, onlyne-store, onlyne-testkit); workspace / [] (onlyne-session).
- `async-trait`: `0.1` / [] (onlyne-adapter, onlyne-gateway, onlyne-gateway-telegram, onlyne-server, onlyne-testkit); workspace / [] (onlyne-gateway-feishu, onlyne-gateway-qqbot, onlyne-gateway-weixin).
- `base64`: workspace / [] (onlyne-cli, onlyne-gateway-feishu, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto); `0.22` / [] (onlyne-client, onlyne-config, onlyne-gateway, onlyne-gateway-telegram, onlyne-server (dev-dependencies), onlyne-testkit); `0.22.1` / [] (onlyne-net).
- `chrono`: `0.4` / [`serde`] (onlyne-adapter (dev-dependencies), onlyne-client, onlyne-gateway, onlyne-gateway-feishu, onlyne-server, onlyne-store, onlyne-testkit); workspace / [] (onlyne-proto, onlyne-session).
- `clap`: workspace / [] (onlyne-cli); `4.5` / [`derive`] (onlyne-client); `4` / [`derive`] (onlyne-gateway, onlyne-server, onlyne-testkit).
- `ed25519-dalek`: `2.1` / [`std`, `rand_core`]; default-features=false (onlyne-client); `2.1.1` / [`std`, `rand_core`]; default-features=false (onlyne-net).
- `rand`: `0.8` / [] (onlyne-client); `0.8.6` / [] (onlyne-net).
- `reqwest`: workspace / [`multipart`] (onlyne-gateway-feishu); workspace / [] (onlyne-gateway-qqbot).
- `rusqlite`: `0.32` / [`bundled`, `chrono`, `serde_json`] (onlyne-client, onlyne-store); `0.32` / [`bundled`] (onlyne-gateway, onlyne-server).
- `rustls`: `0.23.40` / [`ring`, `std`]; default-features=false (onlyne-net); `0.23` / [`ring`, `std`]; default-features=false (onlyne-server).
- `schemars`: `0.8` / [] (onlyne-config); workspace / [`chrono`, `uuid1`] (onlyne-proto).
- `serde`: `1` / [`derive`] (onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-gateway-telegram, onlyne-server, onlyne-store, onlyne-testkit); workspace / [] (onlyne-cli, onlyne-frame, onlyne-gateway-feishu, onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto, onlyne-session); `1.0` / [`derive`] (onlyne-client); `1.0.228` / [`derive`] (onlyne-net).
- `serde_json`: `1` / [] (onlyne-adapter, onlyne-config, onlyne-gateway, onlyne-gateway-telegram, onlyne-server, onlyne-store, onlyne-testkit); workspace / [] (onlyne-cli, onlyne-frame, onlyne-gateway-feishu, onlyne-gateway-feishu (dev-dependencies), onlyne-gateway-qqbot, onlyne-gateway-weixin, onlyne-proto, onlyne-session); `1.0` / [] (onlyne-client); `1.0.150` / [] (onlyne-net).
- `sha2`: `0.10` / [] (onlyne-config, onlyne-gateway); `0.10.9` / [] (onlyne-net); workspace / [] (onlyne-proto).
- `tempfile`: workspace / [] (onlyne-cli (dev-dependencies), onlyne-session (dev-dependencies)); `3.0` / [] (onlyne-client (dev-dependencies)); `3` / [] (onlyne-config (dev-dependencies), onlyne-gateway (dev-dependencies), onlyne-layout (dev-dependencies), onlyne-server (dev-dependencies), onlyne-store (dev-dependencies), onlyne-testkit); `3.27.0` / [] (onlyne-net (dev-dependencies)).
- `tokio`: `1` / [`io-util`, `net`, `sync`, `time`, `rt`, `macros`] (onlyne-adapter); workspace / [] (onlyne-cli, onlyne-frame, onlyne-gateway-feishu, onlyne-gateway-qqbot, onlyne-gateway-weixin); `1.0` / [`rt-multi-thread`, `macros`, `net`, `io-util`, `fs`, `sync`, `time`, `process`] (onlyne-client); workspace / [`rt-multi-thread`, `macros`] (onlyne-frame (dev-dependencies)); `1` / [`fs`, `macros`, `net`, `process`, `rt-multi-thread`, `time`] (onlyne-gateway); `1.52.3` / [`macros`, `net`, `io-util`, `fs`, `time`, `rt-multi-thread`] (onlyne-net); `1` / [`rt-multi-thread`, `macros`, `net`, `io-util`, `sync`, `time`, `fs`, `signal`] (onlyne-server); `1` / [`io-util`, `io-std`, `net`, `process`, `rt-multi-thread`, `macros`, `sync`, `time`, `fs`] (onlyne-testkit).
- `tracing`: `0.1` / [] (onlyne-adapter, onlyne-client, onlyne-server, onlyne-store, onlyne-testkit); workspace / [] (onlyne-gateway-feishu, onlyne-session).
- `uuid`: `1.0` / [`v4`, `serde`] (onlyne-client); workspace / [] (onlyne-proto); `1` / [`v4`, `serde`] (onlyne-server).

## Manifest source set

| manifest | package |
|---|---|
| `crates/onlyne-adapter/Cargo.toml` | `onlyne-adapter` |
| `crates/onlyne-cli/Cargo.toml` | `onlyne-cli` |
| `crates/onlyne-client/Cargo.toml` | `onlyne-client` |
| `crates/onlyne-config/Cargo.toml` | `onlyne-config` |
| `crates/onlyne-frame/Cargo.toml` | `onlyne-frame` |
| `crates/onlyne-gateway/Cargo.toml` | `onlyne-gateway` |
| `crates/onlyne-layout/Cargo.toml` | `onlyne-layout` |
| `crates/onlyne-net/Cargo.toml` | `onlyne-net` |
| `crates/onlyne-proto/Cargo.toml` | `onlyne-proto` |
| `crates/onlyne-server/Cargo.toml` | `onlyne-server` |
| `crates/onlyne-session/Cargo.toml` | `onlyne-session` |
| `crates/onlyne-store/Cargo.toml` | `onlyne-store` |
| `crates/onlyne-testkit/Cargo.toml` | `onlyne-testkit` |
| `plugins/onlyne-gateway-feishu/Cargo.toml` | `onlyne-gateway-feishu` |
| `plugins/onlyne-gateway-qqbot/Cargo.toml` | `onlyne-gateway-qqbot` |
| `plugins/onlyne-gateway-telegram/Cargo.toml` | `onlyne-gateway-telegram` |
| `plugins/onlyne-gateway-weixin/Cargo.toml` | `onlyne-gateway-weixin` |

## Recommended workspace.dependencies

This block satisfies the union of the feature sets declared by the crates on disk. Entries carry a comment naming the crate that forced a non-obvious requirement.

```toml
[workspace.dependencies]
# internal edges
onlyne-frame = { path = "crates/onlyne-frame" }
onlyne-proto = { path = "crates/onlyne-proto" }
onlyne-config = { path = "crates/onlyne-config" }
onlyne-layout = { path = "crates/onlyne-layout" }
onlyne-store = { path = "crates/onlyne-store" }
onlyne-session = { path = "crates/onlyne-session" }
onlyne-net = { path = "crates/onlyne-net" }
onlyne-adapter = { path = "crates/onlyne-adapter" }
onlyne-server = { path = "crates/onlyne-server" }
onlyne-client = { path = "crates/onlyne-client" }
onlyne-testkit = { path = "crates/onlyne-testkit" }

# data and encoding
anyhow = "1"
async-stream = "0.3"                                    # onlyne-adapter streams adapter frames
async-trait = "0.1"                                     # every backend and plugin trait is async
base64 = "0.22"
chrono = { version = "0.4", features = ["serde"] }      # envelope timestamps serialize on the wire
futures-util = "0.3"
parking_lot = "0.12"                                    # onlyne-client dispatch locks
rand = "0.8"
rusqlite = { version = "0.32", features = ["bundled", "chrono", "serde_json"] }  # onlyne-store and onlyne-client bind chrono and JSON values
schemars = { version = "0.8", features = ["chrono", "uuid1"] }                   # onlyne-proto exports dated and uuid schemas
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
tar = "0.4"                                             # onlyne-client unpacks release archives
flate2 = "1.0"                                          # onlyne-client decompresses the same archives
tempfile = "3"
tokio-stream = "0.1"                                    # onlyne-testkit drives adapter streams
toml = "0.8"
tracing = "0.1"
uuid = { version = "1", features = ["v4", "serde"] }

# command line
clap = { version = "4", features = ["derive", "env"] }  # onlyne-cli reads socket and root overrides from the environment
# clap_complete stays a direct dependency of onlyne-cli, the only crate that needs it.

# identity and transport
ed25519-dalek = { version = "2.1", default-features = false, features = ["std", "rand_core"] }
rcgen = { version = "0.13", default-features = false, features = ["ring", "pem"] }
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }   # onlyne-server terminates TLS with the ring provider
rustls-pemfile = "2"
rustls-pki-types = "1"                                  # onlyne-net carries rustls certificate types
time = { version = "0.3", default-features = false, features = ["std", "formatting"] }  # onlyne-net formats x509 validity
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "io-util", "io-std", "fs", "sync", "time", "process", "rt", "signal"] }  # `signal` carries the SIGHUP reload trigger from docs/v1-PLAN.md §5 line 270
tokio-rustls = { version = "0.26", default-features = false, features = ["ring"] }
x509-parser = "0.16"                                    # onlyne-net inspects peer certificates

# gateway host rendering
aes-gcm = "0.10"
pulldown-cmark = "0.12"
qrcode = { version = "0.14", default-features = false }
resvg = { version = "0.45", default-features = false, features = ["text", "system-fonts"] }
unicode-display-width = "0.2"
unicode-segmentation = "1"                                # onlyne-store cuts ledger.out_head on a grapheme boundary

# platform SDKs and their HTTP transport, gateway side only
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls-webpki-roots", "multipart"] }  # forced by onlyne-gateway-feishu (multipart image upload) and onlyne-gateway-qqbot
openlark = { version = "0.17.0", default-features = false, features = ["websocket"] }
teloxide = { version = "0.17.0", default-features = false, features = ["rustls"] }
wechat-ilink = "0.5.0"
tokio-tungstenite = { version = "0.29", default-features = false, features = ["connect", "rustls-tls-webpki-roots"] }
```

One correction to the consolidation brief, grounded in the manifests on disk: `reqwest` stays in the block because `plugins/onlyne-gateway-feishu/Cargo.toml:17` declares it with the `multipart` feature and `plugins/onlyne-gateway-feishu/src/lib.rs:762` holds a `reqwest::Client`, while `plugins/onlyne-gateway-qqbot/Cargo.toml:17` declares it for `plugins/onlyne-gateway-qqbot/src/lib.rs:21`. Dropping it breaks both plugins. `tokio` carries `signal` because `docs/v1-PLAN.md` §5 line 270 names `onlyne server reload` and `SIGHUP` as the reload triggers, and `crates/onlyne-server/Cargo.toml` requests the feature on its own `tokio` line.

Entries removed as unused: `signature`, `tracing-appender`, `tracing-subscriber`, and `url` are declared by no crate on disk.
